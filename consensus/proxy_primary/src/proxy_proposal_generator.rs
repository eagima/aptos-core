// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Proposal generator for proxy consensus.
//!
//! ProxyProposalGenerator creates proxy block proposals that include:
//! - `primary_round`: The primary round this proxy block belongs to
//! - `primary_qc`: Attached when QC.round == primary_round - 1 (cuts the proxy block)
//!
//! Key differences from primary proposal generator:
//! - Simpler backpressure based on primary round progress
//! - No execution pipeline integration
//! - Attaches primary QC when available to "cut" proxy blocks

use crate::{
    proxy_block_store::ProxyBlockReader,
    proxy_error::ProxyConsensusError,
    proxy_leader_election::ProxyLeaderElection,
    proxy_metrics,
};
use aptos_consensus_types::{
    block::Block,
    block_data::BlockData,
    common::{Author, Payload, PayloadFilter, Round},
    proxy_block_data::OptProxyBlockData,
    quorum_cert::QuorumCert,
};
use aptos_crypto::HashValue;
use aptos_infallible::Mutex;
use aptos_logger::prelude::*;
use aptos_time_service::{TimeService, TimeServiceTrait};
use aptos_types::{
    block_info::BlockInfo, on_chain_config::ValidatorTxnConfig,
    validator_txn::ValidatorTransaction,
};
use std::sync::Arc;

/// Configuration for proxy proposal backpressure.
#[derive(Clone, Debug)]
pub struct ProxyBackpressureConfig {
    /// Maximum number of proxy blocks per primary round before applying backpressure
    pub max_blocks_per_primary_round: u64,
    /// Delay to apply when backpressure is triggered
    pub backpressure_delay_ms: u64,
}

impl Default for ProxyBackpressureConfig {
    fn default() -> Self {
        Self {
            max_blocks_per_primary_round: 20,
            backpressure_delay_ms: 50,
        }
    }
}

/// Trait for pulling payload from mempool/quorum store.
///
/// This allows the proxy proposal generator to be decoupled from the
/// actual payload client implementation.
#[async_trait::async_trait]
pub trait TProxyPayloadClient: Send + Sync {
    /// Pull payload for a proxy block.
    async fn pull_proxy_payload(
        &self,
        max_txns: u64,
        max_size_bytes: u64,
        exclude_filter: PayloadFilter,
    ) -> anyhow::Result<(Vec<ValidatorTransaction>, Payload)>;
}

/// Stub payload client for Phase 1 testing.
/// Returns empty payloads without pulling from mempool.
pub struct StubProxyPayloadClient;

impl StubProxyPayloadClient {
    /// Create a new stub payload client.
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubProxyPayloadClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TProxyPayloadClient for StubProxyPayloadClient {
    async fn pull_proxy_payload(
        &self,
        _max_txns: u64,
        _max_size_bytes: u64,
        _exclude_filter: PayloadFilter,
    ) -> anyhow::Result<(Vec<ValidatorTransaction>, Payload)> {
        // Return empty payload for Phase 1
        Ok((vec![], Payload::empty(false, false)))
    }
}

/// Proposal generator for proxy consensus.
pub struct ProxyProposalGenerator<B: ProxyBlockReader> {
    /// Our author address
    author: Author,
    /// Proxy block store for reading block state
    block_store: Arc<B>,
    /// Payload client for pulling transactions
    payload_client: Arc<dyn TProxyPayloadClient>,
    /// Time service for timestamps
    time_service: TimeService,
    /// Last round we generated a proposal for (prevents double proposals)
    last_round_generated: Mutex<Round>,
    /// Backpressure configuration
    backpressure_config: ProxyBackpressureConfig,
    /// Maximum transactions per block
    max_block_txns: u64,
    /// Maximum block size in bytes
    max_block_bytes: u64,
    /// Validator transaction config
    vtxn_config: ValidatorTxnConfig,
}

impl<B: ProxyBlockReader> ProxyProposalGenerator<B> {
    /// Create a new proxy proposal generator.
    pub fn new(
        author: Author,
        block_store: Arc<B>,
        payload_client: Arc<dyn TProxyPayloadClient>,
        time_service: TimeService,
        backpressure_config: ProxyBackpressureConfig,
        max_block_txns: u64,
        max_block_bytes: u64,
        vtxn_config: ValidatorTxnConfig,
    ) -> Self {
        Self {
            author,
            block_store,
            payload_client,
            time_service,
            last_round_generated: Mutex::new(0),
            backpressure_config,
            max_block_txns,
            max_block_bytes,
            vtxn_config,
        }
    }

    /// Generate a proxy block proposal.
    ///
    /// # Arguments
    /// * `round` - The proxy round to propose for
    /// * `hqc` - The highest QC (parent block's QC)
    /// * `primary_round` - The current primary round
    /// * `primary_qc` - Optional primary QC to attach (if QC.round == primary_round - 1)
    ///
    /// # Returns
    /// A signed Block ready for broadcasting
    pub async fn generate_proxy_proposal(
        &self,
        round: Round,
        hqc: QuorumCert,
        primary_round: Round,
        primary_qc: Option<QuorumCert>,
    ) -> Result<BlockData, ProxyConsensusError> {
        let timer = proxy_metrics::PROXY_PROPOSAL_GENERATION_LATENCY.start_timer();

        // Check we haven't already proposed this round
        {
            let mut last_round = self.last_round_generated.lock();
            if *last_round >= round {
                return Err(ProxyConsensusError::AlreadyProposed { round });
            }
            *last_round = round;
        }

        // Apply backpressure if we have too many blocks for this primary round
        let blocks_for_primary_round = self
            .block_store
            .get_ordered_proxy_blocks(primary_round)
            .len() as u64;
        if blocks_for_primary_round >= self.backpressure_config.max_blocks_per_primary_round {
            proxy_metrics::PROXY_BACKPRESSURE_EVENTS.inc();
            tokio::time::sleep(std::time::Duration::from_millis(
                self.backpressure_config.backpressure_delay_ms,
            ))
            .await;
        }

        // Get parent block info from HQC
        let parent_block_info = hqc.certified_block().clone();

        // Validate primary QC if attached
        if let Some(ref pqc) = primary_qc {
            // Primary QC should be for primary_round - 1
            let expected_qc_round = primary_round.saturating_sub(1);
            if pqc.certified_block().round() != expected_qc_round {
                return Err(ProxyConsensusError::PrimaryQCRoundMismatch {
                    expected: expected_qc_round,
                    got: pqc.certified_block().round(),
                });
            }
        }

        // Compute timestamp
        let timestamp = self.time_service.now_unix_time();

        // Build exclusion filter from pending blocks
        let exclude_filter = self.build_payload_filter(&parent_block_info)?;

        // Pull payload from mempool/quorum store
        let (validator_txns, payload) = self
            .payload_client
            .pull_proxy_payload(self.max_block_txns, self.max_block_bytes, exclude_filter)
            .await
            .map_err(|e| ProxyConsensusError::PayloadPullError(e.to_string()))?;

        // Filter validator txns if not enabled
        let validator_txns = if self.vtxn_config.enabled() {
            validator_txns
        } else {
            vec![]
        };

        // Create the proxy block data
        let block_data = BlockData::new_from_proxy(
            hqc.certified_block().epoch(),
            round,
            timestamp.as_micros() as u64,
            hqc.clone(),
            validator_txns,
            payload,
            self.author,
            vec![], // failed_authors - simplified for proxy
            primary_round,
            primary_qc,
        );

        timer.observe_duration();
        proxy_metrics::PROXY_CONSENSUS_PROPOSALS_SENT.inc();

        Ok(block_data)
    }

    /// Generate an optimistic proxy proposal (with grandparent QC only).
    ///
    /// This is used for 1-message-delay block time where we propose
    /// optimistically before receiving the parent QC.
    pub async fn generate_opt_proxy_proposal(
        &self,
        epoch: u64,
        round: Round,
        parent: BlockInfo,
        grandparent_qc: QuorumCert,
        primary_round: Round,
        primary_qc: Option<QuorumCert>,
    ) -> Result<OptProxyBlockData, ProxyConsensusError> {
        let timer = proxy_metrics::PROXY_PROPOSAL_GENERATION_LATENCY.start_timer();

        // Check we haven't already proposed this round
        {
            let mut last_round = self.last_round_generated.lock();
            if *last_round >= round {
                return Err(ProxyConsensusError::AlreadyProposed { round });
            }
            *last_round = round;
        }

        // Validate primary QC if attached
        if let Some(ref pqc) = primary_qc {
            let expected_qc_round = primary_round.saturating_sub(1);
            if pqc.certified_block().round() != expected_qc_round {
                return Err(ProxyConsensusError::PrimaryQCRoundMismatch {
                    expected: expected_qc_round,
                    got: pqc.certified_block().round(),
                });
            }
        }

        // Compute timestamp
        let timestamp = self.time_service.now_unix_time();

        // Build exclusion filter
        let exclude_filter = self.build_payload_filter(&parent)?;

        // Pull payload
        let (validator_txns, payload) = self
            .payload_client
            .pull_proxy_payload(self.max_block_txns, self.max_block_bytes, exclude_filter)
            .await
            .map_err(|e| ProxyConsensusError::PayloadPullError(e.to_string()))?;

        // Filter validator txns if not enabled
        let validator_txns = if self.vtxn_config.enabled() {
            validator_txns
        } else {
            vec![]
        };

        // Create optimistic proxy block data
        let opt_block_data = OptProxyBlockData::new(
            validator_txns,
            payload,
            self.author,
            epoch,
            round,
            timestamp.as_micros() as u64,
            parent,
            grandparent_qc,
            primary_round,
            primary_qc,
        );

        timer.observe_duration();
        proxy_metrics::PROXY_CONSENSUS_PROPOSALS_SENT.inc();

        Ok(opt_block_data)
    }

    /// Build a payload filter to exclude transactions from pending blocks.
    fn build_payload_filter(
        &self,
        _parent_block_info: &BlockInfo,
    ) -> Result<PayloadFilter, ProxyConsensusError> {
        // For now, return an empty filter. In a full implementation,
        // we would traverse pending blocks to exclude their payloads.
        // Since proxy blocks are in-memory only, this is simpler.
        Ok(PayloadFilter::Empty)
    }

    /// Get the author of this proposal generator.
    pub fn author(&self) -> Author {
        self.author
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptos_consensus_types::vote_data::VoteData;
    use aptos_types::{
        aggregate_signature::AggregateSignature,
        ledger_info::{LedgerInfo, LedgerInfoWithSignatures},
    };
    use std::collections::HashMap;

    fn make_qc(epoch: u64, round: Round) -> QuorumCert {
        let block_info =
            BlockInfo::new(epoch, round, HashValue::random(), HashValue::random(), 0, 0, None);
        let vote_data = VoteData::new(block_info.clone(), block_info.clone());
        let ledger_info = LedgerInfo::new(block_info, HashValue::zero());
        let li_sig = LedgerInfoWithSignatures::new(ledger_info, AggregateSignature::empty());
        QuorumCert::new(vote_data, li_sig)
    }

    struct MockBlockStore {
        ordered_blocks: HashMap<Round, Vec<Arc<aptos_consensus_types::pipelined_block::PipelinedBlock>>>,
    }

    impl MockBlockStore {
        fn new() -> Self {
            Self {
                ordered_blocks: HashMap::new(),
            }
        }
    }

    impl ProxyBlockReader for MockBlockStore {
        fn get_proxy_block(
            &self,
            _block_id: HashValue,
        ) -> Option<Arc<aptos_consensus_types::pipelined_block::PipelinedBlock>> {
            None
        }

        fn highest_proxy_qc(&self) -> Arc<QuorumCert> {
            Arc::new(make_qc(1, 0))
        }

        fn highest_primary_qc(&self) -> Arc<QuorumCert> {
            Arc::new(make_qc(1, 0))
        }

        fn highest_primary_tc(
            &self,
        ) -> Option<Arc<aptos_consensus_types::timeout_2chain::TwoChainTimeoutCertificate>> {
            None
        }

        fn current_primary_round(&self) -> Round {
            1
        }

        fn proxy_sync_info(&self) -> aptos_consensus_types::proxy_sync_info::ProxySyncInfo {
            aptos_consensus_types::proxy_sync_info::ProxySyncInfo::new(
                make_qc(1, 0),
                None,
                None,
                make_qc(1, 0),
                None,
            )
        }

        fn get_ordered_proxy_blocks(
            &self,
            primary_round: Round,
        ) -> Vec<Arc<aptos_consensus_types::pipelined_block::PipelinedBlock>> {
            self.ordered_blocks.get(&primary_round).cloned().unwrap_or_default()
        }
    }

    struct MockPayloadClient;

    #[async_trait::async_trait]
    impl TProxyPayloadClient for MockPayloadClient {
        async fn pull_proxy_payload(
            &self,
            _max_txns: u64,
            _max_size_bytes: u64,
            _exclude_filter: PayloadFilter,
        ) -> anyhow::Result<(Vec<ValidatorTransaction>, Payload)> {
            Ok((vec![], Payload::empty(false, false)))
        }
    }

    #[test]
    fn test_proposal_generator_creation() {
        let author = aptos_types::account_address::AccountAddress::random();
        let block_store = Arc::new(MockBlockStore::new());
        let payload_client = Arc::new(MockPayloadClient);
        let time_service = aptos_time_service::TimeService::real();

        let generator = ProxyProposalGenerator::new(
            author,
            block_store,
            payload_client,
            time_service,
            ProxyBackpressureConfig::default(),
            1000,
            1000000,
            ValidatorTxnConfig::default_disabled(),
        );

        assert_eq!(generator.author(), author);
    }
}
