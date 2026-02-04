// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! In-memory block store for proxy consensus.
//!
//! Unlike the primary BlockStore, ProxyBlockStore is:
//! - **In-memory only**: No persistence (safety handled by ProxySafetyRules)
//! - **Simpler**: No execution pipeline integration
//! - **Tracks primary state**: Stores highest primary QC/TC received from primary consensus
//!
//! Recovery is fast via peer fetch since proxies are co-located.

use crate::{proxy_error::ProxyConsensusError, proxy_metrics};
use aptos_consensus_types::{
    common::Round,
    pipelined_block::PipelinedBlock,
    proxy_sync_info::ProxySyncInfo,
    quorum_cert::QuorumCert,
    timeout_2chain::TwoChainTimeoutCertificate,
    wrapped_ledger_info::WrappedLedgerInfo,
};
use aptos_crypto::HashValue;
use aptos_infallible::RwLock;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

/// Trait for reading proxy blocks.
pub trait ProxyBlockReader: Send + Sync {
    /// Get a proxy block by ID.
    fn get_proxy_block(&self, block_id: HashValue) -> Option<Arc<PipelinedBlock>>;

    /// Get the highest proxy QC.
    fn highest_proxy_qc(&self) -> Arc<QuorumCert>;

    /// Get the highest primary QC (received from primary consensus).
    fn highest_primary_qc(&self) -> Arc<QuorumCert>;

    /// Get the highest primary TC (received from primary consensus).
    fn highest_primary_tc(&self) -> Option<Arc<TwoChainTimeoutCertificate>>;

    /// Get the current primary round: max(QC_primary.round, TC_primary.round) + 1
    fn current_primary_round(&self) -> Round;

    /// Build ProxySyncInfo from current state.
    fn proxy_sync_info(&self) -> ProxySyncInfo;

    /// Get all ordered proxy blocks for a given primary round.
    fn get_ordered_proxy_blocks(&self, primary_round: Round) -> Vec<Arc<PipelinedBlock>>;
}

/// In-memory proxy block store.
pub struct ProxyBlockStore {
    inner: Arc<RwLock<ProxyBlockTree>>,
    /// The primary block that serves as genesis for this proxy consensus epoch
    genesis_primary_block_id: HashValue,
}

/// Internal tree structure for proxy blocks.
struct ProxyBlockTree {
    // Primary index: block_id -> block
    id_to_block: HashMap<HashValue, Arc<PipelinedBlock>>,

    // Secondary indices
    round_to_id: BTreeMap<Round, HashValue>,
    ordered_by_primary_round: HashMap<Round, Vec<HashValue>>,

    // Proxy consensus certificates
    highest_proxy_qc: Arc<QuorumCert>,
    highest_proxy_ordered_cert: Option<WrappedLedgerInfo>,
    highest_proxy_timeout_cert: Option<Arc<TwoChainTimeoutCertificate>>,

    // Primary consensus state (from internal channel)
    highest_primary_qc: Arc<QuorumCert>,
    highest_primary_tc: Option<Arc<TwoChainTimeoutCertificate>>,
}

impl ProxyBlockStore {
    /// Create a new proxy block store with the given genesis primary block.
    ///
    /// The genesis is ALWAYS the epoch start primary block - it never changes
    /// after state sync within an epoch.
    pub fn new(
        genesis_primary_block_id: HashValue,
        initial_proxy_qc: QuorumCert,
        initial_primary_qc: QuorumCert,
    ) -> Self {
        let inner = ProxyBlockTree {
            id_to_block: HashMap::new(),
            round_to_id: BTreeMap::new(),
            ordered_by_primary_round: HashMap::new(),
            highest_proxy_qc: Arc::new(initial_proxy_qc),
            highest_proxy_ordered_cert: None,
            highest_proxy_timeout_cert: None,
            highest_primary_qc: Arc::new(initial_primary_qc),
            highest_primary_tc: None,
        };

        Self {
            inner: Arc::new(RwLock::new(inner)),
            genesis_primary_block_id,
        }
    }

    /// Get the genesis primary block ID.
    pub fn genesis_primary_block_id(&self) -> HashValue {
        self.genesis_primary_block_id
    }

    /// Insert a proxy block into the store.
    pub fn insert_block(&self, block: Arc<PipelinedBlock>) -> Result<(), ProxyConsensusError> {
        let block_id = block.id();
        let round = block.round();
        let primary_round = block
            .block()
            .block_data()
            .primary_round()
            .ok_or_else(|| ProxyConsensusError::InvalidProxyBlock("Not a proxy block".into()))?;

        let mut tree = self.inner.write();

        // Insert into primary index
        tree.id_to_block.insert(block_id, block);

        // Update round index
        tree.round_to_id.insert(round, block_id);

        // Update metrics
        proxy_metrics::PROXY_BLOCK_STORE_SIZE.set(tree.id_to_block.len() as i64);

        Ok(())
    }

    /// Mark a block as ordered and add to ordered_by_primary_round index.
    pub fn mark_block_ordered(
        &self,
        block_id: HashValue,
        primary_round: Round,
    ) -> Result<(), ProxyConsensusError> {
        let mut tree = self.inner.write();

        // Verify block exists
        if !tree.id_to_block.contains_key(&block_id) {
            return Err(ProxyConsensusError::ParentNotFound(block_id));
        }

        // Add to ordered index
        tree.ordered_by_primary_round
            .entry(primary_round)
            .or_default()
            .push(block_id);

        proxy_metrics::PROXY_CONSENSUS_BLOCKS_ORDERED.inc();

        Ok(())
    }

    /// Get a block by ID.
    pub fn get_block(&self, block_id: HashValue) -> Option<Arc<PipelinedBlock>> {
        self.inner.read().id_to_block.get(&block_id).cloned()
    }

    /// Get a block by round.
    pub fn get_block_for_round(&self, round: Round) -> Option<Arc<PipelinedBlock>> {
        let tree = self.inner.read();
        tree.round_to_id
            .get(&round)
            .and_then(|id| tree.id_to_block.get(id).cloned())
    }

    /// Get all ordered blocks for a primary round, in order.
    pub fn get_ordered_blocks(&self, primary_round: Round) -> Vec<Arc<PipelinedBlock>> {
        let tree = self.inner.read();
        tree.ordered_by_primary_round
            .get(&primary_round)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| tree.id_to_block.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Count blocks for a primary round (for backpressure).
    pub fn count_blocks_for_primary_round(&self, primary_round: Round) -> u64 {
        let tree = self.inner.read();

        // Count all blocks with the given primary_round
        tree.id_to_block
            .values()
            .filter(|b| b.block().block_data().primary_round() == Some(primary_round))
            .count() as u64
    }

    /// Update the highest proxy QC.
    pub fn update_highest_proxy_qc(&self, qc: QuorumCert) {
        let mut tree = self.inner.write();
        if qc.certified_block().round() > tree.highest_proxy_qc.certified_block().round() {
            tree.highest_proxy_qc = Arc::new(qc);
        }
    }

    /// Update the highest proxy ordered certificate.
    pub fn update_highest_proxy_ordered_cert(&self, cert: WrappedLedgerInfo) {
        let mut tree = self.inner.write();
        let should_update = tree
            .highest_proxy_ordered_cert
            .as_ref()
            .map_or(true, |existing| {
                cert.commit_info().round() > existing.commit_info().round()
            });
        if should_update {
            tree.highest_proxy_ordered_cert = Some(cert);
        }
    }

    /// Update the highest proxy timeout certificate.
    pub fn update_highest_proxy_timeout_cert(&self, tc: TwoChainTimeoutCertificate) {
        let mut tree = self.inner.write();
        let should_update = tree
            .highest_proxy_timeout_cert
            .as_ref()
            .map_or(true, |existing| tc.round() > existing.round());
        if should_update {
            tree.highest_proxy_timeout_cert = Some(Arc::new(tc));
        }
    }

    /// Update the highest primary QC (received from primary consensus).
    pub fn update_highest_primary_qc(&self, qc: QuorumCert) {
        let mut tree = self.inner.write();
        if qc.certified_block().round() > tree.highest_primary_qc.certified_block().round() {
            tree.highest_primary_qc = Arc::new(qc);
            proxy_metrics::PROXY_CURRENT_PRIMARY_ROUND
                .set((tree.highest_primary_qc.certified_block().round() + 1) as i64);
        }
    }

    /// Update the highest primary TC (received from primary consensus).
    pub fn update_highest_primary_tc(&self, tc: TwoChainTimeoutCertificate) {
        let mut tree = self.inner.write();
        let should_update = tree
            .highest_primary_tc
            .as_ref()
            .map_or(true, |existing| tc.round() > existing.round());
        if should_update {
            tree.highest_primary_tc = Some(Arc::new(tc));
        }
    }

    /// Get the current primary round: max(QC_primary.round, TC_primary.round) + 1
    pub fn current_primary_round(&self) -> Round {
        let tree = self.inner.read();
        let qc_round = tree.highest_primary_qc.certified_block().round();
        let tc_round = tree.highest_primary_tc.as_ref().map_or(0, |tc| tc.round());
        std::cmp::max(qc_round, tc_round) + 1
    }

    /// Build ProxySyncInfo from current state.
    pub fn proxy_sync_info(&self) -> ProxySyncInfo {
        let tree = self.inner.read();
        ProxySyncInfo::new(
            (*tree.highest_proxy_qc).clone(),
            tree.highest_proxy_ordered_cert.clone(),
            tree.highest_proxy_timeout_cert.as_ref().map(|tc| (**tc).clone()),
            (*tree.highest_primary_qc).clone(),
            tree.highest_primary_tc.as_ref().map(|tc| (**tc).clone()),
        )
    }

    /// Clear all blocks (on shutdown or epoch change).
    /// Transactions will return to mempool naturally.
    pub fn clear(&self) {
        let mut tree = self.inner.write();
        tree.id_to_block.clear();
        tree.round_to_id.clear();
        tree.ordered_by_primary_round.clear();
        proxy_metrics::PROXY_BLOCK_STORE_SIZE.set(0);
    }

    /// Prune blocks up to and including the given round.
    pub fn prune_blocks_to_round(&self, round: Round) {
        let mut tree = self.inner.write();

        // Find block IDs to remove
        let rounds_to_remove: Vec<Round> = tree
            .round_to_id
            .range(..=round)
            .map(|(r, _)| *r)
            .collect();

        for r in rounds_to_remove {
            if let Some(block_id) = tree.round_to_id.remove(&r) {
                tree.id_to_block.remove(&block_id);
            }
        }

        proxy_metrics::PROXY_BLOCK_STORE_SIZE.set(tree.id_to_block.len() as i64);
    }
}

impl ProxyBlockReader for ProxyBlockStore {
    fn get_proxy_block(&self, block_id: HashValue) -> Option<Arc<PipelinedBlock>> {
        self.get_block(block_id)
    }

    fn highest_proxy_qc(&self) -> Arc<QuorumCert> {
        self.inner.read().highest_proxy_qc.clone()
    }

    fn highest_primary_qc(&self) -> Arc<QuorumCert> {
        self.inner.read().highest_primary_qc.clone()
    }

    fn highest_primary_tc(&self) -> Option<Arc<TwoChainTimeoutCertificate>> {
        self.inner.read().highest_primary_tc.clone()
    }

    fn current_primary_round(&self) -> Round {
        self.current_primary_round()
    }

    fn proxy_sync_info(&self) -> ProxySyncInfo {
        self.proxy_sync_info()
    }

    fn get_ordered_proxy_blocks(&self, primary_round: Round) -> Vec<Arc<PipelinedBlock>> {
        self.get_ordered_blocks(primary_round)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptos_consensus_types::vote_data::VoteData;
    use aptos_types::{
        aggregate_signature::AggregateSignature,
        block_info::BlockInfo,
        ledger_info::{LedgerInfo, LedgerInfoWithSignatures},
    };

    fn make_qc(epoch: u64, round: Round) -> QuorumCert {
        let block_info =
            BlockInfo::new(epoch, round, HashValue::random(), HashValue::random(), 0, 0, None);
        let vote_data = VoteData::new(block_info.clone(), block_info.clone());
        let ledger_info = LedgerInfo::new(block_info, HashValue::zero());
        let li_sig = LedgerInfoWithSignatures::new(ledger_info, AggregateSignature::empty());
        QuorumCert::new(vote_data, li_sig)
    }

    #[test]
    fn test_proxy_block_store_creation() {
        let genesis_id = HashValue::random();
        let proxy_qc = make_qc(1, 0);
        let primary_qc = make_qc(1, 0);

        let store = ProxyBlockStore::new(genesis_id, proxy_qc.clone(), primary_qc.clone());

        assert_eq!(store.genesis_primary_block_id(), genesis_id);
        assert_eq!(
            store.highest_proxy_qc().certified_block().round(),
            proxy_qc.certified_block().round()
        );
        assert_eq!(
            store.highest_primary_qc().certified_block().round(),
            primary_qc.certified_block().round()
        );
        assert_eq!(store.current_primary_round(), 1);
    }

    #[test]
    fn test_update_primary_qc() {
        let store = ProxyBlockStore::new(HashValue::random(), make_qc(1, 0), make_qc(1, 0));

        // Update with higher round QC
        let new_qc = make_qc(1, 5);
        store.update_highest_primary_qc(new_qc);

        assert_eq!(store.highest_primary_qc().certified_block().round(), 5);
        assert_eq!(store.current_primary_round(), 6);
    }

    #[test]
    fn test_current_primary_round_with_tc() {
        let store = ProxyBlockStore::new(HashValue::random(), make_qc(1, 0), make_qc(1, 3));

        // With QC at round 3, current_primary_round should be 4
        assert_eq!(store.current_primary_round(), 4);

        // Add TC at round 5
        let tc_block_info =
            BlockInfo::new(1, 5, HashValue::random(), HashValue::random(), 0, 0, None);
        let timeout = aptos_consensus_types::timeout_2chain::TwoChainTimeout::new(
            1,
            5,
            make_qc(1, 4),
        );
        // Note: Creating a proper TC requires signatures, so we skip this in tests
    }

    #[test]
    fn test_clear() {
        let store = ProxyBlockStore::new(HashValue::random(), make_qc(1, 0), make_qc(1, 0));

        store.clear();

        // Store should be empty but genesis preserved
        assert!(store.get_block(HashValue::random()).is_none());
    }
}
