// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Main event loop for proxy consensus.
//!
//! ProxyRoundManager handles the core consensus logic for proxy validators:
//! - Processing proposals, votes, and order votes
//! - Coordinating with primary consensus via channels
//! - Forwarding ordered proxy blocks to all primaries
//!
//! Key design principles:
//! - In-memory only (no persistence, safety via ProxySafetyRules)
//! - Simpler than primary RoundManager (no execution pipeline)
//! - Bidirectional communication with primary consensus

use crate::{
    proxy_block_store::ProxyBlockStore,
    proxy_error::ProxyConsensusError,
    proxy_leader_election::ProxyLeaderElection,
    proxy_metrics,
    proxy_network_sender::ProxyNetworkSender,
    proxy_safety_rules::ProxySafetyRules,
};
use aptos_consensus_types::{
    block::Block,
    common::{Author, Round},
    order_vote::OrderVote,
    proxy_messages::{
        OrderedProxyBlocksMsg, ProxyOrderVoteMsg, ProxyProposalMsg, ProxyVoteMsg,
    },
    quorum_cert::QuorumCert,
    timeout_2chain::TwoChainTimeoutCertificate,
    vote::Vote,
};
use aptos_crypto::HashValue;
use aptos_infallible::Mutex;
use aptos_logger::prelude::*;
use aptos_time_service::TimeService;
use aptos_types::{epoch_state::EpochState, validator_verifier::ValidatorVerifier};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::mpsc;

/// Events sent from primary RoundManager to proxy RoundManager.
#[derive(Debug)]
pub enum PrimaryToProxyEvent {
    /// New primary QC available - may trigger proxy block "cutting"
    NewPrimaryQC(Arc<QuorumCert>),
    /// New primary TC available - for tracking primary round
    NewPrimaryTC(Arc<TwoChainTimeoutCertificate>),
    /// Shutdown signal
    Shutdown,
}

/// Events sent from proxy RoundManager to primary RoundManager.
#[derive(Debug)]
pub enum ProxyToPrimaryEvent {
    /// Ordered proxy blocks ready to be aggregated into primary block
    OrderedProxyBlocks(OrderedProxyBlocksMsg),
}

/// Verified event from network after signature verification.
#[derive(Debug)]
pub enum VerifiedProxyEvent {
    ProxyProposalMsg(Box<ProxyProposalMsg>),
    ProxyVoteMsg(Box<ProxyVoteMsg>),
    ProxyOrderVoteMsg(Box<ProxyOrderVoteMsg>),
}

/// Configuration for proxy round manager.
#[derive(Clone, Debug)]
pub struct ProxyRoundManagerConfig {
    /// Round timeout duration
    pub round_timeout: Duration,
    /// Maximum blocks per primary round before backpressure
    pub max_blocks_per_primary_round: u64,
    /// Backpressure delay when limit is reached
    pub backpressure_delay_ms: u64,
}

impl Default for ProxyRoundManagerConfig {
    fn default() -> Self {
        Self {
            round_timeout: Duration::from_millis(1000),
            max_blocks_per_primary_round: 20,
            backpressure_delay_ms: 50,
        }
    }
}

/// Main event loop for proxy consensus.
pub struct ProxyRoundManager {
    /// Epoch state including validator verifier
    epoch_state: Arc<EpochState>,
    /// Proxy block store
    block_store: Arc<ProxyBlockStore>,
    /// Current round state
    current_round: Round,
    /// Leader election
    leader_election: Arc<ProxyLeaderElection>,
    /// Safety rules for signing
    safety_rules: Arc<Mutex<ProxySafetyRules>>,
    /// Network sender
    network: Arc<ProxyNetworkSender>,
    /// Time service
    #[allow(dead_code)]
    time_service: TimeService,
    /// Configuration
    config: ProxyRoundManagerConfig,
    /// Our author address
    author: Author,

    /// Pending votes being aggregated for QC formation
    pending_votes: HashMap<HashValue, Vec<Vote>>,
    /// Pending order votes being aggregated
    pending_order_votes: HashMap<HashValue, Vec<OrderVote>>,

    /// Current primary round (from primary consensus)
    primary_round: Round,
    /// Latest primary QC received
    latest_primary_qc: Option<Arc<QuorumCert>>,
    /// Whether we have a primary QC that hasn't been attached yet
    pending_primary_qc: Option<Arc<QuorumCert>>,
}

impl ProxyRoundManager {
    /// Create a new proxy round manager.
    pub fn new(
        epoch_state: Arc<EpochState>,
        block_store: Arc<ProxyBlockStore>,
        leader_election: Arc<ProxyLeaderElection>,
        safety_rules: Arc<Mutex<ProxySafetyRules>>,
        network: Arc<ProxyNetworkSender>,
        time_service: TimeService,
        config: ProxyRoundManagerConfig,
        author: Author,
        initial_round: Round,
        initial_primary_round: Round,
    ) -> Self {
        Self {
            epoch_state,
            block_store,
            current_round: initial_round,
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            pending_votes: HashMap::new(),
            pending_order_votes: HashMap::new(),
            primary_round: initial_primary_round,
            latest_primary_qc: None,
            pending_primary_qc: None,
        }
    }

    /// Get the proxy verifier.
    fn proxy_verifier(&self) -> &ValidatorVerifier {
        &self.epoch_state.verifier
    }

    /// Process a new primary QC from primary consensus.
    ///
    /// When a new primary QC is received:
    /// 1. Update our tracked primary round
    /// 2. Store the QC to be attached to the next proxy proposal
    pub fn process_primary_qc(&mut self, qc: Arc<QuorumCert>) {
        let qc_round = qc.certified_block().round();
        let new_primary_round = qc_round + 1;

        if new_primary_round > self.primary_round {
            info!(
                "ProxyRoundManager: received primary QC for round {}, advancing to primary round {}",
                qc_round, new_primary_round
            );

            self.primary_round = new_primary_round;
            self.latest_primary_qc = Some(qc.clone());
            self.pending_primary_qc = Some(qc.clone());

            // Update block store
            self.block_store
                .update_highest_primary_qc((*qc).clone());

            proxy_metrics::PROXY_CURRENT_PRIMARY_ROUND.set(new_primary_round as i64);
        }
    }

    /// Process a new primary TC from primary consensus.
    pub fn process_primary_tc(&mut self, tc: Arc<TwoChainTimeoutCertificate>) {
        let tc_round = tc.round();
        let new_primary_round = tc_round + 1;

        if new_primary_round > self.primary_round {
            info!(
                "ProxyRoundManager: received primary TC for round {}, advancing to primary round {}",
                tc_round, new_primary_round
            );

            self.primary_round = new_primary_round;
            self.block_store
                .update_highest_primary_tc((*tc).clone());

            proxy_metrics::PROXY_CURRENT_PRIMARY_ROUND.set(new_primary_round as i64);
        }
    }

    /// Process a proxy proposal message.
    ///
    /// This is a stub implementation for Phase 1.
    /// Full implementation will include:
    /// - Signature verification
    /// - Block insertion
    /// - Vote generation
    pub async fn process_proxy_proposal_msg(
        &mut self,
        proposal_msg: ProxyProposalMsg,
        sender: Author,
    ) -> Result<(), ProxyConsensusError> {
        let proposal = proposal_msg.proposal();
        let round = proposal.round();

        debug!(
            "ProxyRoundManager: received proposal for round {} from {}",
            round, sender
        );

        // Verify round is valid
        if round <= self.current_round {
            return Err(ProxyConsensusError::RoundTooOld {
                round,
                last_voted: self.current_round,
            });
        }

        // Update current round
        self.current_round = round;
        proxy_metrics::PROXY_CURRENT_ROUND.set(round as i64);

        // TODO: Insert block into store and vote on it
        // This requires proper VoteProposal construction which depends on
        // execution state that we don't have in proxy consensus.
        // For Phase 1, we'll implement the skeleton and fill in details.

        Ok(())
    }

    /// Process a proxy vote message.
    ///
    /// This is a stub implementation for Phase 1.
    pub async fn process_proxy_vote_msg(
        &mut self,
        vote_msg: ProxyVoteMsg,
        sender: Author,
        _primary_tx: &mpsc::UnboundedSender<ProxyToPrimaryEvent>,
    ) -> Result<(), ProxyConsensusError> {
        let vote = vote_msg.vote();
        let block_id = vote.vote_data().proposed().id();
        let round = vote.vote_data().proposed().round();

        debug!(
            "ProxyRoundManager: received vote for round {} from {}",
            round, sender
        );

        // Aggregate votes
        self.pending_votes
            .entry(block_id)
            .or_default()
            .push(vote.clone());

        // Get votes for this block and calculate voting power
        let verifier = self.proxy_verifier();
        let quorum_power = verifier.quorum_voting_power() as u128;

        let (voting_power, num_votes) = {
            let votes = self.pending_votes.get(&block_id).unwrap();
            let power: u128 = votes
                .iter()
                .filter_map(|v| verifier.get_voting_power(&v.author()).map(|p| p as u128))
                .sum();
            (power, votes.len())
        };

        if voting_power >= quorum_power {
            debug!(
                "ProxyRoundManager: formed QC for round {} with {} votes",
                round, num_votes
            );

            // TODO: Form QC and proceed to order vote
            // This requires proper signature aggregation

            // Clean up pending votes
            self.pending_votes.remove(&block_id);
        }

        Ok(())
    }

    /// Process a proxy order vote message.
    ///
    /// This is a stub implementation for Phase 1.
    pub async fn process_proxy_order_vote_msg(
        &mut self,
        order_vote_msg: ProxyOrderVoteMsg,
        sender: Author,
        primary_tx: &mpsc::UnboundedSender<ProxyToPrimaryEvent>,
    ) -> Result<(), ProxyConsensusError> {
        let order_vote = order_vote_msg.order_vote();
        let block_id = order_vote.ledger_info().commit_info().id();
        let round = order_vote.ledger_info().commit_info().round();

        debug!(
            "ProxyRoundManager: received order vote for round {} from {}",
            round, sender
        );

        // Aggregate order votes
        self.pending_order_votes
            .entry(block_id)
            .or_default()
            .push(order_vote.clone());

        // Get votes and calculate voting power
        let verifier = self.proxy_verifier();
        let quorum_power = verifier.quorum_voting_power() as u128;

        let (voting_power, num_votes) = {
            let order_votes = self.pending_order_votes.get(&block_id).unwrap();
            let power: u128 = order_votes
                .iter()
                .filter_map(|v| verifier.get_voting_power(&v.author()).map(|p| p as u128))
                .sum();
            (power, order_votes.len())
        };

        if voting_power >= quorum_power {
            debug!(
                "ProxyRoundManager: formed order cert for round {} with {} votes",
                round, num_votes
            );

            // Mark block as ordered
            let primary_round = self.primary_round;
            if let Err(e) = self
                .block_store
                .mark_block_ordered(block_id, primary_round)
            {
                warn!("Failed to mark block as ordered: {:?}", e);
            }

            // Check if we should forward ordered blocks
            let block = self.block_store.get_block(block_id);
            if let Some(block) = block {
                if block.block().block_data().primary_qc().is_some() {
                    // This is a cutting point
                    self.forward_ordered_blocks(primary_round, primary_tx)
                        .await?;
                }
            }

            // Clean up
            self.pending_order_votes.remove(&block_id);
        }

        Ok(())
    }

    /// Forward ordered proxy blocks to all primaries.
    async fn forward_ordered_blocks(
        &mut self,
        primary_round: Round,
        primary_tx: &mpsc::UnboundedSender<ProxyToPrimaryEvent>,
    ) -> Result<(), ProxyConsensusError> {
        let ordered_blocks = self.block_store.get_ordered_blocks(primary_round);

        if ordered_blocks.is_empty() {
            return Ok(());
        }

        let primary_qc = self.latest_primary_qc.as_ref().ok_or_else(|| {
            ProxyConsensusError::PrimaryQCRoundMismatch {
                expected: primary_round.saturating_sub(1),
                got: 0,
            }
        })?;

        // Collect blocks
        let blocks: Vec<Block> = ordered_blocks
            .iter()
            .map(|b| b.block().clone())
            .collect();

        // Create ordered proxy blocks message
        let ordered_msg =
            OrderedProxyBlocksMsg::new(blocks.clone(), primary_round, (**primary_qc).clone());

        info!(
            "ProxyRoundManager: forwarding {} ordered proxy blocks for primary round {}",
            blocks.len(),
            primary_round
        );

        // Broadcast to all primaries via network
        self.network
            .broadcast_ordered_proxy_blocks(ordered_msg.clone())
            .await;

        // Also send via channel to local primary RoundManager
        let _ = primary_tx.send(ProxyToPrimaryEvent::OrderedProxyBlocks(ordered_msg));

        proxy_metrics::PROXY_CONSENSUS_BLOCKS_FORWARDED.inc_by(blocks.len() as u64);

        // Clear pending primary QC since we've used it
        self.pending_primary_qc = None;

        Ok(())
    }

    /// Process a new round event (timeout or QC).
    pub async fn process_new_round(&mut self, round: Round) -> Result<(), ProxyConsensusError> {
        self.current_round = round;
        proxy_metrics::PROXY_CURRENT_ROUND.set(round as i64);

        // Check if we are the leader
        if self.leader_election.is_leader(round) {
            info!("ProxyRoundManager: we are leader for round {}", round);
            // Proposal generation would happen here
            // For now, this is handled by the external proposal generator
        }

        Ok(())
    }

    /// Get the current round.
    pub fn current_round(&self) -> Round {
        self.current_round
    }

    /// Get the current primary round.
    pub fn current_primary_round(&self) -> Round {
        self.primary_round
    }

    /// Get whether we should attach a primary QC to the next proposal.
    pub fn should_attach_primary_qc(&self) -> bool {
        self.pending_primary_qc.is_some()
    }

    /// Take the pending primary QC to attach to a proposal.
    pub fn take_pending_primary_qc(&mut self) -> Option<Arc<QuorumCert>> {
        self.pending_primary_qc.take()
    }

    /// Get the block store.
    pub fn block_store(&self) -> &Arc<ProxyBlockStore> {
        &self.block_store
    }

    /// Get the epoch state.
    pub fn epoch_state(&self) -> &Arc<EpochState> {
        &self.epoch_state
    }

    /// Get the configuration.
    pub fn config(&self) -> &ProxyRoundManagerConfig {
        &self.config
    }

    /// Start the proxy round manager event loop.
    ///
    /// This is the main entry point for running proxy consensus. It:
    /// 1. Listens for events from primary consensus (QC/TC updates)
    /// 2. Processes proxy consensus messages from network (TODO: Phase 2)
    /// 3. Handles round timeouts (TODO: Phase 2)
    pub async fn start(
        mut self,
        mut primary_rx: mpsc::UnboundedReceiver<PrimaryToProxyEvent>,
        primary_tx: mpsc::UnboundedSender<ProxyToPrimaryEvent>,
    ) {
        info!(
            epoch = self.epoch_state.epoch,
            author = %self.author,
            initial_round = self.current_round,
            initial_primary_round = self.primary_round,
            "ProxyRoundManager starting event loop"
        );

        loop {
            tokio::select! {
                // Handle events from primary consensus
                event = primary_rx.recv() => {
                    match event {
                        Some(PrimaryToProxyEvent::NewPrimaryQC(qc)) => {
                            self.process_primary_qc(qc);
                        }
                        Some(PrimaryToProxyEvent::NewPrimaryTC(tc)) => {
                            self.process_primary_tc(tc);
                        }
                        Some(PrimaryToProxyEvent::Shutdown) => {
                            info!("ProxyRoundManager received shutdown signal");
                            break;
                        }
                        None => {
                            info!("ProxyRoundManager: primary channel closed, shutting down");
                            break;
                        }
                    }
                }

                // TODO: Phase 2 - Add network message handling
                // network_msg = self.network_rx.recv() => { ... }

                // TODO: Phase 2 - Add round timeout handling
                // _ = self.round_timeout.tick() => { ... }
            }
        }

        // Use primary_tx to suppress unused warning (will be used in Phase 2)
        drop(primary_tx);

        info!(
            epoch = self.epoch_state.epoch,
            "ProxyRoundManager event loop terminated"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_round_manager_config_default() {
        let config = ProxyRoundManagerConfig::default();
        assert_eq!(config.round_timeout, Duration::from_millis(1000));
        assert_eq!(config.max_blocks_per_primary_round, 20);
        assert_eq!(config.backpressure_delay_ms, 50);
    }
}
