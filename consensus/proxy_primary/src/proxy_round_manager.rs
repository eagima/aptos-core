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
    proxy_block_store::{ProxyBlockReader, ProxyBlockStore},
    proxy_error::ProxyConsensusError,
    proxy_leader_election::ProxyLeaderElection,
    proxy_metrics,
    proxy_network_sender::ProxyNetworkSender,
    proxy_proposal_generator::ProxyProposalGenerator,
    proxy_safety_rules::ProxySafetyRules,
};
use aptos_consensus_types::{
    block::Block,
    common::{Author, Round},
    order_vote::OrderVote,
    order_vote_proposal::OrderVoteProposal,
    pipelined_block::{OrderedBlockWindow, PipelinedBlock},
    proxy_messages::{
        OrderedProxyBlocksMsg, ProxyOrderVoteMsg, ProxyProposalMsg, ProxyVoteMsg,
    },
    quorum_cert::QuorumCert,
    timeout_2chain::TwoChainTimeoutCertificate,
    vote_data::VoteData,
    vote_proposal::VoteProposal,
};
use aptos_types::proof::AccumulatorExtensionProof;
use aptos_crypto::{hash::CryptoHash, HashValue};
use aptos_infallible::Mutex;
use aptos_logger::prelude::*;
use aptos_time_service::TimeService;
use aptos_types::{
    epoch_state::EpochState,
    ledger_info::{LedgerInfo, LedgerInfoWithSignatures, SignatureAggregator},
    validator_verifier::ValidatorVerifier,
};
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
    /// Proposal generator for creating proxy block proposals
    proposal_generator: Option<Arc<ProxyProposalGenerator<ProxyBlockStore>>>,

    /// Pending votes being aggregated for QC formation
    /// Key: ledger_info digest, Value: (VoteData, SignatureAggregator)
    pending_votes: HashMap<HashValue, (VoteData, SignatureAggregator<LedgerInfo>)>,
    /// Pending order votes being aggregated
    pending_order_votes: HashMap<HashValue, Vec<OrderVote>>,

    /// Last round we proposed for (to prevent re-proposals)
    last_proposed_round: Round,

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
            proposal_generator: None,
            pending_votes: HashMap::new(),
            pending_order_votes: HashMap::new(),
            last_proposed_round: 0,
            primary_round: initial_primary_round,
            latest_primary_qc: None,
            pending_primary_qc: None,
        }
    }

    /// Create a new proxy round manager with a proposal generator.
    pub fn new_with_proposal_generator(
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
        proposal_generator: Arc<ProxyProposalGenerator<ProxyBlockStore>>,
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
            proposal_generator: Some(proposal_generator),
            pending_votes: HashMap::new(),
            pending_order_votes: HashMap::new(),
            last_proposed_round: 0,
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
    /// Handles incoming proposals by:
    /// 1. Verifying the proposal signature
    /// 2. Inserting the block into proxy block store
    /// 3. Signing a vote and sending to the next leader
    pub async fn process_proxy_proposal_msg(
        &mut self,
        proposal_msg: ProxyProposalMsg,
        sender: Author,
    ) -> Result<(), ProxyConsensusError> {
        let proposal = proposal_msg.proposal().clone();
        let round = proposal.round();

        debug!(
            "ProxyRoundManager: received proposal for round {} from {}",
            round, sender
        );

        // Verify round is valid (must be higher than current)
        if round < self.current_round {
            return Err(ProxyConsensusError::RoundTooOld {
                round,
                last_voted: self.current_round,
            });
        }

        // Verify proposal signature
        proposal
            .validate_signature(&self.epoch_state.verifier)
            .map_err(|e| {
                ProxyConsensusError::InvalidProxyBlock(format!("Invalid signature: {:?}", e))
            })?;

        // Update current round
        self.current_round = round;
        proxy_metrics::PROXY_CURRENT_ROUND.set(round as i64);

        // Create PipelinedBlock and insert into block store
        // For proxy consensus, we don't execute blocks, so we use a simplified PipelinedBlock
        let pipelined_block =
            PipelinedBlock::new_ordered(proposal.clone(), OrderedBlockWindow::empty());
        self.block_store.insert_block(Arc::new(pipelined_block))?;

        // Create VoteProposal for signing
        // Use decoupled_execution=true since proxy consensus doesn't execute blocks
        // Create an empty accumulator extension proof (ordering only, no execution)
        let accumulator_extension_proof = AccumulatorExtensionProof::new(vec![], 0, vec![]);

        let vote_proposal = VoteProposal::new(
            accumulator_extension_proof,
            proposal.clone(),
            None, // next_epoch_state - not handling epoch changes in Phase 1
            true, // decoupled_execution - ordering only without execution
        );

        // Get TC if available (for 2-chain voting rules)
        let tc: Option<&TwoChainTimeoutCertificate> = None;

        // Sign vote using safety rules
        let vote = self
            .safety_rules
            .lock()
            .construct_and_sign_proxy_vote(&vote_proposal, tc)?;

        // Get sync info and create vote message
        let sync_info = self.block_store.proxy_sync_info();
        let vote_msg = ProxyVoteMsg::new(vote, sync_info);

        // Determine next leader and send vote
        let next_round = round + 1;
        let next_leader = self.leader_election.get_leader(next_round);

        // Send vote to next leader (or broadcast if we are the next leader)
        if next_leader == self.author {
            // We are the next leader, broadcast to all so they can see our vote
            self.network.broadcast_proxy_vote(vote_msg).await;
        } else {
            // Send to next leader
            self.network
                .send_proxy_vote(vote_msg, vec![next_leader])
                .await;
        }

        debug!(
            "ProxyRoundManager: voted on proposal for round {}, sent to next leader {:?}",
            round, next_leader
        );

        Ok(())
    }

    /// Process a proxy vote message.
    ///
    /// Aggregates votes and forms QC when quorum is reached.
    /// After QC formation, broadcasts order vote.
    pub async fn process_proxy_vote_msg(
        &mut self,
        vote_msg: ProxyVoteMsg,
        sender: Author,
        primary_tx: &mpsc::UnboundedSender<ProxyToPrimaryEvent>,
    ) -> Result<(), ProxyConsensusError> {
        let vote = vote_msg.vote();
        let li_digest = vote.ledger_info().hash();
        let round = vote.vote_data().proposed().round();

        debug!(
            "ProxyRoundManager: received vote for round {} from {}",
            round, sender
        );

        // Get the verifier - use direct field access for split borrow
        let verifier = &self.epoch_state.verifier;

        // Get or create the signature aggregator for this ledger info
        let (vote_data, sig_aggregator) =
            self.pending_votes.entry(li_digest).or_insert_with(|| {
                (
                    vote.vote_data().clone(),
                    SignatureAggregator::new(vote.ledger_info().clone()),
                )
            });

        // Add the signature from this vote
        sig_aggregator.add_signature(vote.author(), vote.signature_with_status());

        // Check if we have enough voting power for a QC
        match sig_aggregator.check_voting_power(verifier, true) {
            Ok(_voting_power) => {
                // We have quorum! Form the QC
                info!(
                    "ProxyRoundManager: formed QC for round {} with quorum",
                    round
                );

                // Clone vote_data before aggregation so we can use it after removing from pending_votes
                let vote_data_clone = vote_data.clone();

                // Aggregate signatures and create LedgerInfoWithSignatures
                match sig_aggregator.aggregate_and_verify(verifier) {
                    Ok((ledger_info, aggregated_sig)) => {
                        let li_with_sig =
                            LedgerInfoWithSignatures::new(ledger_info, aggregated_sig);
                        let qc = Arc::new(QuorumCert::new(vote_data_clone.clone(), li_with_sig));

                        // Update block store with the new QC
                        self.block_store.update_highest_proxy_qc((*qc).clone());
                        proxy_metrics::PROXY_CONSENSUS_QCS_FORMED.inc();

                        // Advance to next round
                        self.current_round = round + 1;
                        proxy_metrics::PROXY_CURRENT_ROUND.set(self.current_round as i64);

                        // Clean up pending votes (now we can mutably borrow since vote_data_clone is owned)
                        self.pending_votes.remove(&li_digest);

                        // Sign and broadcast order vote
                        let block_id = vote_data_clone.proposed().id();
                        if let Some(pipelined_block) = self.block_store.get_block(block_id) {
                            let block = pipelined_block.block().clone();
                            let block_info = vote_data_clone.proposed().clone();

                            // Create order vote proposal
                            let order_vote_proposal =
                                OrderVoteProposal::new(block, block_info, qc.clone());

                            // Sign order vote using safety rules (hold lock briefly, then release)
                            let order_vote_result = self
                                .safety_rules
                                .lock()
                                .construct_and_sign_proxy_order_vote(&order_vote_proposal);

                            // Now we can await without holding the lock
                            match order_vote_result {
                                Ok(order_vote) => {
                                    // Create order vote message with QC
                                    let order_vote_msg =
                                        ProxyOrderVoteMsg::new(order_vote, (*qc).clone());

                                    // Broadcast to all proxy validators
                                    self.network
                                        .broadcast_proxy_order_vote(order_vote_msg)
                                        .await;

                                    debug!(
                                        "ProxyRoundManager: signed and broadcast order vote for round {}",
                                        round
                                    );
                                },
                                Err(e) => {
                                    warn!(
                                        "ProxyRoundManager: failed to sign order vote for round {}: {:?}",
                                        round, e
                                    );
                                },
                            }
                        } else {
                            warn!(
                                "ProxyRoundManager: block {} not found for order vote",
                                block_id
                            );
                        }

                        // Walk parent chain to mark all blocks for this primary round as ordered
                        let primary_round = self.primary_round;
                        let mut walk_id = block_id;
                        let mut has_cutting_point = false;
                        loop {
                            if let Some(walk_block) = self.block_store.get_block(walk_id) {
                                if walk_block.block().block_data().primary_round()
                                    != Some(primary_round)
                                {
                                    break;
                                }
                                let _ = self
                                    .block_store
                                    .mark_block_ordered(walk_id, primary_round);
                                if walk_block.block().block_data().primary_qc().is_some() {
                                    has_cutting_point = true;
                                }
                                walk_id = walk_block.block().parent_id();
                            } else {
                                break;
                            }
                        }

                        // If we hit a cutting point, forward all ordered blocks
                        if has_cutting_point {
                            if let Err(e) = self
                                .forward_ordered_blocks(primary_round, primary_tx)
                                .await
                            {
                                warn!("Failed to forward ordered blocks: {:?}", e);
                            }
                        }

                        debug!(
                            "ProxyRoundManager: QC formed for round {}, advancing to round {}",
                            round, self.current_round
                        );
                    },
                    Err(e) => {
                        warn!("Error aggregating signatures for QC: {:?}", e);
                    },
                }
            },
            Err(aptos_types::validator_verifier::VerifyError::TooLittleVotingPower {
                voting_power,
                ..
            }) => {
                debug!(
                    "ProxyRoundManager: vote added for round {}, current power: {}",
                    round, voting_power
                );
            },
            Err(e) => {
                warn!("Error checking voting power: {:?}", e);
            },
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

        // Get primary QC: use latest_primary_qc, or extract from cutting-point block
        let primary_qc = if let Some(ref qc) = self.latest_primary_qc {
            (**qc).clone()
        } else {
            // Try to get primary QC from the last ordered block (the cutting point)
            ordered_blocks
                .iter()
                .rev()
                .find_map(|b| b.block().block_data().primary_qc().cloned())
                .ok_or_else(|| ProxyConsensusError::PrimaryQCRoundMismatch {
                    expected: primary_round.saturating_sub(1),
                    got: 0,
                })?
        };

        // Collect blocks
        let blocks: Vec<Block> = ordered_blocks
            .iter()
            .map(|b| b.block().clone())
            .collect();

        // Create ordered proxy blocks message
        let ordered_msg =
            OrderedProxyBlocksMsg::new(blocks.clone(), primary_round, primary_qc);

        info!(
            "ProxyRoundManager: forwarding {} ordered proxy blocks for primary round {}",
            blocks.len(),
            primary_round
        );

        // Broadcast to all primaries via network
        // Note: ProxyNetworkSender::broadcast_ordered_proxy_blocks already increments
        // PROXY_CONSENSUS_BLOCKS_FORWARDED and PROXY_BLOCKS_PER_PRIMARY_ROUND metrics
        self.network
            .broadcast_ordered_proxy_blocks(ordered_msg.clone())
            .await;

        // Also send via channel to local primary RoundManager
        let _ = primary_tx.send(ProxyToPrimaryEvent::OrderedProxyBlocks(ordered_msg));

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
    /// 2. Processes proxy consensus messages from network
    /// 3. Handles round timeouts for leader proposal
    pub async fn start(
        mut self,
        mut primary_rx: mpsc::UnboundedReceiver<PrimaryToProxyEvent>,
        primary_tx: mpsc::UnboundedSender<ProxyToPrimaryEvent>,
        mut network_rx: mpsc::UnboundedReceiver<VerifiedProxyEvent>,
    ) {
        info!(
            epoch = self.epoch_state.epoch,
            author = %self.author,
            initial_round = self.current_round,
            initial_primary_round = self.primary_round,
            "ProxyRoundManager starting event loop"
        );

        // Round timeout interval for leader proposal trigger
        let mut round_timeout = tokio::time::interval(self.config.round_timeout);

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

                // Handle network messages from proxy validators
                network_event = network_rx.recv() => {
                    match network_event {
                        Some(VerifiedProxyEvent::ProxyProposalMsg(msg)) => {
                            let sender = msg.proposer();
                            if let Err(e) = self.process_proxy_proposal_msg(*msg, sender).await {
                                warn!("Error processing proxy proposal: {:?}", e);
                            }
                        }
                        Some(VerifiedProxyEvent::ProxyVoteMsg(msg)) => {
                            let sender = msg.vote().author();
                            if let Err(e) = self.process_proxy_vote_msg(*msg, sender, &primary_tx).await {
                                warn!("Error processing proxy vote: {:?}", e);
                            }
                        }
                        Some(VerifiedProxyEvent::ProxyOrderVoteMsg(msg)) => {
                            let sender = msg.order_vote().author();
                            if let Err(e) = self.process_proxy_order_vote_msg(*msg, sender, &primary_tx).await {
                                warn!("Error processing proxy order vote: {:?}", e);
                            }
                        }
                        None => {
                            info!("ProxyRoundManager: network channel closed, shutting down");
                            break;
                        }
                    }
                }

                // Handle round timeout - trigger leader proposal
                _ = round_timeout.tick() => {
                    if let Err(e) = self.process_round_timeout(&primary_tx).await {
                        debug!("Round timeout processing error: {:?}", e);
                    }
                }
            }
        }

        info!(
            epoch = self.epoch_state.epoch,
            "ProxyRoundManager event loop terminated"
        );
    }

    /// Process round timeout - if we are the leader, generate and broadcast a proposal.
    async fn process_round_timeout(
        &mut self,
        _primary_tx: &mpsc::UnboundedSender<ProxyToPrimaryEvent>,
    ) -> Result<(), ProxyConsensusError> {
        let round = self.current_round;

        // Increment timeout counter for all nodes
        proxy_metrics::PROXY_ROUND_TIMEOUT_ALL.inc();

        let expected_leader = self.leader_election.get_leader(round);
        let is_leader = self.leader_election.is_leader(round);

        debug!(
            round = round,
            author = %self.author,
            expected_leader = %expected_leader,
            is_leader = is_leader,
            num_validators = self.leader_election.num_validators(),
            "ProxyRoundManager: round timeout fired"
        );

        if !is_leader {
            return Ok(());
        }

        // Skip if we already proposed for this round
        if round <= self.last_proposed_round {
            return Ok(());
        }

        // Check if we have a proposal generator
        let proposal_generator = match &self.proposal_generator {
            Some(pg) => pg.clone(),
            None => {
                debug!(
                    "ProxyRoundManager: leader for round {}, but no proposal generator",
                    round
                );
                return Ok(());
            },
        };

        info!(
            "ProxyRoundManager: leader for round {}, generating proposal",
            round
        );

        // Get highest proxy QC as the parent QC
        let hqc = (*self.block_store.highest_proxy_qc()).clone();

        // Check if we should attach a primary QC
        // Only attach if QC.round == primary_round - 1
        let primary_qc = self.pending_primary_qc.take().and_then(|pqc| {
            let expected_qc_round = self.primary_round.saturating_sub(1);
            if pqc.certified_block().round() == expected_qc_round {
                Some((*pqc).clone())
            } else {
                // Put it back if not the right round
                self.pending_primary_qc = Some(pqc);
                None
            }
        });

        // Generate proposal using proposal generator
        let block_data = proposal_generator
            .generate_proxy_proposal(round, hqc.clone(), self.primary_round, primary_qc)
            .await?;

        // Sign the proposal using safety rules
        let signature = self
            .safety_rules
            .lock()
            .sign_proxy_proposal(&block_data)?;

        // Create Block from BlockData and signature
        let block = Block::new_proposal_from_block_data_and_signature(block_data, signature);

        // Get sync info from block store
        let sync_info = self.block_store.proxy_sync_info();

        // Create ProxyProposalMsg
        let proposal_msg = ProxyProposalMsg::new(block.clone(), sync_info);

        // Broadcast proposal to all proxy validators
        self.network.broadcast_proxy_proposal(proposal_msg).await;

        // Record that we've proposed for this round
        self.last_proposed_round = round;

        // Increment round timeout counter for metrics
        proxy_metrics::PROXY_ROUND_TIMEOUT_COUNT.inc();

        // Leader self-processing: insert block into own block store and create self-vote
        let pipelined_block =
            PipelinedBlock::new_ordered(block.clone(), OrderedBlockWindow::empty());
        if let Err(e) = self.block_store.insert_block(Arc::new(pipelined_block)) {
            warn!(
                "ProxyRoundManager: failed to insert own proposal into block store: {:?}",
                e
            );
        }

        // Create self-vote using safety rules
        let accumulator_extension_proof = AccumulatorExtensionProof::new(vec![], 0, vec![]);
        let vote_proposal = VoteProposal::new(
            accumulator_extension_proof,
            block.clone(),
            None,  // next_epoch_state
            true,  // decoupled_execution
        );
        match self
            .safety_rules
            .lock()
            .construct_and_sign_proxy_vote(&vote_proposal, None)
        {
            Ok(vote) => {
                // Add self-vote to pending_votes
                let li_digest = vote.ledger_info().hash();
                let (_, sig_aggregator) =
                    self.pending_votes.entry(li_digest).or_insert_with(|| {
                        (
                            vote.vote_data().clone(),
                            SignatureAggregator::new(vote.ledger_info().clone()),
                        )
                    });
                sig_aggregator.add_signature(vote.author(), vote.signature_with_status());

                debug!(
                    "ProxyRoundManager: leader self-voted on proposal for round {}",
                    round
                );
            },
            Err(e) => {
                warn!(
                    "ProxyRoundManager: failed to create self-vote for round {}: {:?}",
                    round, e
                );
            },
        }

        debug!(
            "ProxyRoundManager: broadcast proposal for round {}, primary_round {}",
            round, self.primary_round
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ProxyBlockReader, ProxyBlockStore, ProxyLeaderElection, ProxyNetworkSender,
        ProxySafetyRules,
    };
    use aptos_consensus_types::vote_data::VoteData;
    use aptos_types::{
        aggregate_signature::AggregateSignature,
        block_info::BlockInfo,
        ledger_info::{LedgerInfo, LedgerInfoWithSignatures},
        validator_verifier::random_validator_verifier,
    };

    fn make_test_qc(epoch: u64, round: Round) -> QuorumCert {
        let block_info =
            BlockInfo::new(epoch, round, HashValue::random(), HashValue::random(), 0, 0, None);
        let vote_data = VoteData::new(block_info.clone(), block_info);
        let ledger_info = LedgerInfo::new(BlockInfo::empty(), HashValue::zero());
        let li_sig = LedgerInfoWithSignatures::new(ledger_info, AggregateSignature::empty());
        QuorumCert::new(vote_data, li_sig)
    }

    fn create_test_round_manager() -> (
        ProxyRoundManager,
        mpsc::UnboundedSender<PrimaryToProxyEvent>,
        mpsc::UnboundedReceiver<ProxyToPrimaryEvent>,
    ) {
        let (signers, verifier) = random_validator_verifier(4, None, true);
        let author = signers[0].author();
        let epoch = 1;

        let epoch_state = Arc::new(EpochState::new(epoch, verifier.into()));

        let proxy_validators: Vec<_> = signers.iter().map(|s| s.author()).collect();
        let leader_election = Arc::new(ProxyLeaderElection::new(proxy_validators.clone(), author));
        let block_store = Arc::new(ProxyBlockStore::new_for_epoch(epoch));
        let safety_rules = Arc::new(Mutex::new(ProxySafetyRules::new_stub(
            author,
            epoch_state.clone(),
        )));
        let network = Arc::new(ProxyNetworkSender::new_stub(author, proxy_validators));
        let time_service = TimeService::real();
        let config = ProxyRoundManagerConfig::default();

        let round_manager = ProxyRoundManager::new(
            epoch_state,
            block_store,
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            1, // initial_round
            1, // initial_primary_round
        );

        let (primary_to_proxy_tx, _primary_to_proxy_rx) =
            mpsc::unbounded_channel::<PrimaryToProxyEvent>();
        let (_proxy_to_primary_tx, proxy_to_primary_rx) =
            mpsc::unbounded_channel::<ProxyToPrimaryEvent>();

        (round_manager, primary_to_proxy_tx, proxy_to_primary_rx)
    }

    #[test]
    fn test_proxy_round_manager_config_default() {
        let config = ProxyRoundManagerConfig::default();
        assert_eq!(config.round_timeout, Duration::from_millis(1000));
        assert_eq!(config.max_blocks_per_primary_round, 20);
        assert_eq!(config.backpressure_delay_ms, 50);
    }

    #[test]
    fn test_proxy_round_manager_creation() {
        let (signers, verifier) = random_validator_verifier(4, None, true);
        let author = signers[0].author();
        let epoch = 1;

        let epoch_state = Arc::new(EpochState::new(epoch, verifier.into()));

        let proxy_validators: Vec<_> = signers.iter().map(|s| s.author()).collect();
        let leader_election = Arc::new(ProxyLeaderElection::new(proxy_validators.clone(), author));
        let block_store = Arc::new(ProxyBlockStore::new_for_epoch(epoch));
        let safety_rules = Arc::new(Mutex::new(ProxySafetyRules::new_stub(
            author,
            epoch_state.clone(),
        )));
        let network = Arc::new(ProxyNetworkSender::new_stub(author, proxy_validators));
        let time_service = TimeService::real();
        let config = ProxyRoundManagerConfig::default();

        let round_manager = ProxyRoundManager::new(
            epoch_state.clone(),
            block_store,
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            1, // initial_round
            1, // initial_primary_round
        );

        assert_eq!(round_manager.current_round(), 1);
        assert_eq!(round_manager.current_primary_round(), 1);
        assert_eq!(round_manager.epoch_state().epoch, epoch);
    }

    #[test]
    fn test_process_primary_qc() {
        let (signers, verifier) = random_validator_verifier(4, None, true);
        let author = signers[0].author();
        let epoch = 1;

        let epoch_state = Arc::new(EpochState::new(epoch, verifier.into()));

        let proxy_validators: Vec<_> = signers.iter().map(|s| s.author()).collect();
        let leader_election = Arc::new(ProxyLeaderElection::new(proxy_validators.clone(), author));
        let block_store = Arc::new(ProxyBlockStore::new_for_epoch(epoch));
        let safety_rules = Arc::new(Mutex::new(ProxySafetyRules::new_stub(
            author,
            epoch_state.clone(),
        )));
        let network = Arc::new(ProxyNetworkSender::new_stub(author, proxy_validators));
        let time_service = TimeService::real();
        let config = ProxyRoundManagerConfig::default();

        let mut round_manager = ProxyRoundManager::new(
            epoch_state,
            block_store,
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            1, // initial_round
            1, // initial_primary_round
        );

        // Process a QC for round 5
        let qc = Arc::new(make_test_qc(epoch, 5));
        round_manager.process_primary_qc(qc);

        // Primary round should advance to QC.round + 1 = 6
        assert_eq!(round_manager.current_primary_round(), 6);
        assert!(round_manager.should_attach_primary_qc());

        // Take the pending QC
        let pending_qc = round_manager.take_pending_primary_qc();
        assert!(pending_qc.is_some());
        assert_eq!(pending_qc.unwrap().certified_block().round(), 5);

        // Should no longer have pending QC
        assert!(!round_manager.should_attach_primary_qc());
    }

    #[test]
    fn test_process_primary_qc_does_not_go_backwards() {
        let (signers, verifier) = random_validator_verifier(4, None, true);
        let author = signers[0].author();
        let epoch = 1;

        let epoch_state = Arc::new(EpochState::new(epoch, verifier.into()));

        let proxy_validators: Vec<_> = signers.iter().map(|s| s.author()).collect();
        let leader_election = Arc::new(ProxyLeaderElection::new(proxy_validators.clone(), author));
        let block_store = Arc::new(ProxyBlockStore::new_for_epoch(epoch));
        let safety_rules = Arc::new(Mutex::new(ProxySafetyRules::new_stub(
            author,
            epoch_state.clone(),
        )));
        let network = Arc::new(ProxyNetworkSender::new_stub(author, proxy_validators));
        let time_service = TimeService::real();
        let config = ProxyRoundManagerConfig::default();

        let mut round_manager = ProxyRoundManager::new(
            epoch_state,
            block_store,
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            1, // initial_round
            1, // initial_primary_round
        );

        // Process QC for round 10
        let qc1 = Arc::new(make_test_qc(epoch, 10));
        round_manager.process_primary_qc(qc1);
        assert_eq!(round_manager.current_primary_round(), 11);

        // Process QC for round 5 (older) - should not go backwards
        let qc2 = Arc::new(make_test_qc(epoch, 5));
        round_manager.process_primary_qc(qc2);
        assert_eq!(round_manager.current_primary_round(), 11); // Still 11
    }

    #[tokio::test]
    async fn test_event_loop_shutdown() {
        let (signers, verifier) = random_validator_verifier(4, None, true);
        let author = signers[0].author();
        let epoch = 1;

        let epoch_state = Arc::new(EpochState::new(epoch, verifier.into()));

        let proxy_validators: Vec<_> = signers.iter().map(|s| s.author()).collect();
        let leader_election = Arc::new(ProxyLeaderElection::new(proxy_validators.clone(), author));
        let block_store = Arc::new(ProxyBlockStore::new_for_epoch(epoch));
        let safety_rules = Arc::new(Mutex::new(ProxySafetyRules::new_stub(
            author,
            epoch_state.clone(),
        )));
        let network = Arc::new(ProxyNetworkSender::new_stub(author, proxy_validators));
        let time_service = TimeService::real();
        let config = ProxyRoundManagerConfig::default();

        let round_manager = ProxyRoundManager::new(
            epoch_state,
            block_store,
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            1,
            1,
        );

        let (primary_tx, primary_rx) = mpsc::unbounded_channel();
        let (proxy_tx, _proxy_rx) = mpsc::unbounded_channel();
        let (_network_tx, network_rx) = mpsc::unbounded_channel::<VerifiedProxyEvent>();

        // Spawn the event loop
        let handle = tokio::spawn(async move {
            round_manager.start(primary_rx, proxy_tx, network_rx).await;
        });

        // Send shutdown signal
        primary_tx.send(PrimaryToProxyEvent::Shutdown).unwrap();

        // Wait for the event loop to terminate
        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(result.is_ok(), "Event loop should terminate on shutdown");
    }

    #[tokio::test]
    async fn test_event_loop_processes_qc() {
        let (signers, verifier) = random_validator_verifier(4, None, true);
        let author = signers[0].author();
        let epoch = 1;

        let epoch_state = Arc::new(EpochState::new(epoch, verifier.into()));

        let proxy_validators: Vec<_> = signers.iter().map(|s| s.author()).collect();
        let leader_election = Arc::new(ProxyLeaderElection::new(proxy_validators.clone(), author));
        let block_store = Arc::new(ProxyBlockStore::new_for_epoch(epoch));
        let safety_rules = Arc::new(Mutex::new(ProxySafetyRules::new_stub(
            author,
            epoch_state.clone(),
        )));
        let network = Arc::new(ProxyNetworkSender::new_stub(author, proxy_validators));
        let time_service = TimeService::real();
        let config = ProxyRoundManagerConfig::default();

        let round_manager = ProxyRoundManager::new(
            epoch_state,
            block_store.clone(),
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            1,
            1,
        );

        let (primary_tx, primary_rx) = mpsc::unbounded_channel();
        let (proxy_tx, _proxy_rx) = mpsc::unbounded_channel();
        let (_network_tx, network_rx) = mpsc::unbounded_channel::<VerifiedProxyEvent>();

        // Spawn the event loop
        let handle = tokio::spawn(async move {
            round_manager.start(primary_rx, proxy_tx, network_rx).await;
        });

        // Send a QC
        let qc = Arc::new(make_test_qc(epoch, 5));
        primary_tx
            .send(PrimaryToProxyEvent::NewPrimaryQC(qc))
            .unwrap();

        // Give it time to process
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Check the block store was updated
        assert_eq!(block_store.highest_primary_qc().certified_block().round(), 5);

        // Shutdown
        primary_tx.send(PrimaryToProxyEvent::Shutdown).unwrap();

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_event_loop_channel_closed() {
        let (signers, verifier) = random_validator_verifier(4, None, true);
        let author = signers[0].author();
        let epoch = 1;

        let epoch_state = Arc::new(EpochState::new(epoch, verifier.into()));

        let proxy_validators: Vec<_> = signers.iter().map(|s| s.author()).collect();
        let leader_election = Arc::new(ProxyLeaderElection::new(proxy_validators.clone(), author));
        let block_store = Arc::new(ProxyBlockStore::new_for_epoch(epoch));
        let safety_rules = Arc::new(Mutex::new(ProxySafetyRules::new_stub(
            author,
            epoch_state.clone(),
        )));
        let network = Arc::new(ProxyNetworkSender::new_stub(author, proxy_validators));
        let time_service = TimeService::real();
        let config = ProxyRoundManagerConfig::default();

        let round_manager = ProxyRoundManager::new(
            epoch_state,
            block_store,
            leader_election,
            safety_rules,
            network,
            time_service,
            config,
            author,
            1,
            1,
        );

        let (primary_tx, primary_rx) = mpsc::unbounded_channel();
        let (proxy_tx, _proxy_rx) = mpsc::unbounded_channel();
        let (_network_tx, network_rx) = mpsc::unbounded_channel::<VerifiedProxyEvent>();

        // Spawn the event loop
        let handle = tokio::spawn(async move {
            round_manager.start(primary_rx, proxy_tx, network_rx).await;
        });

        // Drop the sender to close the channel
        drop(primary_tx);

        // Event loop should terminate when channel is closed
        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(result.is_ok(), "Event loop should terminate when channel closes");
    }
}
