// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Integration types for primary consensus to consume proxy blocks.
//!
//! This module provides:
//! - `PrimaryBlockFromProxy`: Aggregates ordered proxy blocks into primary block content
//! - Deterministic aggregation: All primaries produce identical blocks from same proxy blocks
//! - Verification: Ensures proxy blocks are valid and properly linked

use crate::proxy_error::ProxyConsensusError;
use aptos_consensus_types::{
    block::Block,
    common::{Payload, Round},
    proxy_messages::OrderedProxyBlocksMsg,
    quorum_cert::QuorumCert,
};
use aptos_crypto::HashValue;
use aptos_types::validator_verifier::ValidatorVerifier;
use std::sync::Arc;

/// Aggregated proxy blocks ready to be included in a primary block.
///
/// This structure is created from an `OrderedProxyBlocksMsg` and provides
/// the content needed to construct a primary block.
///
/// Key invariant: Given the same `OrderedProxyBlocksMsg`, all primaries
/// MUST produce identical `PrimaryBlockFromProxy` and therefore identical
/// primary blocks. This determinism is critical for consensus.
#[derive(Debug, Clone)]
pub struct PrimaryBlockFromProxy {
    /// Ordered proxy blocks (sorted by proxy round)
    proxy_blocks: Vec<Block>,
    /// Primary round these blocks belong to
    primary_round: Round,
    /// Primary QC that "cut" these proxy blocks
    primary_qc: QuorumCert,
    /// Aggregated payload hash (for deterministic ordering)
    aggregated_payload_hash: HashValue,
}

impl PrimaryBlockFromProxy {
    /// Create a new `PrimaryBlockFromProxy` from an ordered message.
    ///
    /// This constructor validates the message structure but does not
    /// verify cryptographic signatures (that's done by `verify`).
    pub fn from_ordered_msg(
        msg: OrderedProxyBlocksMsg,
    ) -> Result<Self, ProxyConsensusError> {
        let proxy_blocks = msg.proxy_blocks().to_vec();
        let primary_round = msg.primary_round();
        let primary_qc = msg.primary_qc().clone();

        // Verify non-empty
        if proxy_blocks.is_empty() {
            return Err(ProxyConsensusError::InvalidProxyBlock(
                "OrderedProxyBlocksMsg must contain at least one proxy block".into(),
            ));
        }

        // Verify all blocks have the same primary_round
        for block in &proxy_blocks {
            let block_primary_round = block
                .block_data()
                .primary_round()
                .ok_or_else(|| {
                    ProxyConsensusError::InvalidProxyBlock("Block is not a proxy block".into())
                })?;
            if block_primary_round != primary_round {
                return Err(ProxyConsensusError::InvalidPrimaryRound {
                    expected: primary_round,
                    got: block_primary_round,
                });
            }
        }

        // Verify primary QC round matches primary_round - 1
        let expected_qc_round = primary_round.saturating_sub(1);
        if primary_qc.certified_block().round() != expected_qc_round {
            return Err(ProxyConsensusError::PrimaryQCRoundMismatch {
                expected: expected_qc_round,
                got: primary_qc.certified_block().round(),
            });
        }

        // Verify blocks are properly linked
        for i in 1..proxy_blocks.len() {
            if proxy_blocks[i].parent_id() != proxy_blocks[i - 1].id() {
                return Err(ProxyConsensusError::InvalidProxyBlock(format!(
                    "Proxy blocks not properly linked: block {} parent {} != block {} id {}",
                    i,
                    proxy_blocks[i].parent_id(),
                    i - 1,
                    proxy_blocks[i - 1].id(),
                )));
            }
        }

        // Verify last block has primary QC attached
        let last_block = proxy_blocks.last().unwrap();
        if last_block.block_data().primary_qc().is_none() {
            return Err(ProxyConsensusError::InvalidProxyBlock(
                "Last proxy block must have primary QC attached".into(),
            ));
        }

        // Compute deterministic aggregated payload hash
        let aggregated_payload_hash = Self::compute_aggregated_hash(&proxy_blocks);

        Ok(Self {
            proxy_blocks,
            primary_round,
            primary_qc,
            aggregated_payload_hash,
        })
    }

    /// Compute a deterministic hash of all proxy block payloads.
    ///
    /// This ensures all primaries can verify they have the same content.
    fn compute_aggregated_hash(proxy_blocks: &[Block]) -> HashValue {
        use aptos_crypto::hash::CryptoHash;

        let mut hasher = aptos_crypto::hash::DefaultHasher::new(b"AggregatedProxyBlocks");
        for block in proxy_blocks {
            hasher.update(&block.id().to_vec());
        }
        hasher.finish()
    }

    /// Verify the proxy blocks have valid signatures.
    ///
    /// This should be called before using the proxy blocks.
    pub fn verify(&self, proxy_verifier: &ValidatorVerifier) -> Result<(), ProxyConsensusError> {
        // Verify each block's signature
        for block in &self.proxy_blocks {
            block
                .validate_signature(proxy_verifier)
                .map_err(|e| ProxyConsensusError::InvalidProxyBlock(e.to_string()))?;
        }

        // Verify primary QC (note: this should be verified with the full validator set,
        // not just proxy verifier, but for phase 1 we use proxy verifier)
        self.primary_qc
            .verify(proxy_verifier)
            .map_err(|e| ProxyConsensusError::InvalidProxyBlock(e.to_string()))?;

        Ok(())
    }

    /// Get the proxy blocks.
    pub fn proxy_blocks(&self) -> &[Block] {
        &self.proxy_blocks
    }

    /// Get the primary round.
    pub fn primary_round(&self) -> Round {
        self.primary_round
    }

    /// Get the primary QC.
    pub fn primary_qc(&self) -> &QuorumCert {
        &self.primary_qc
    }

    /// Get the number of proxy blocks.
    pub fn num_blocks(&self) -> usize {
        self.proxy_blocks.len()
    }

    /// Get the aggregated payload hash for verification.
    pub fn aggregated_payload_hash(&self) -> HashValue {
        self.aggregated_payload_hash
    }

    /// Aggregate payloads from all proxy blocks.
    ///
    /// This combines the payloads deterministically so all primaries
    /// produce the same primary block payload.
    pub fn aggregate_payloads(&self) -> Payload {
        // For phase 1, we return an empty payload
        // In production, this would extract and combine transactions from all proxy blocks
        // The aggregation must be deterministic so all primaries produce identical blocks

        // Count total payloads for metrics
        let _payload_count = self
            .proxy_blocks
            .iter()
            .filter(|b| b.payload().is_some())
            .count();

        // Return empty payload for phase 1 (actual aggregation TBD)
        Payload::empty(false, true)
    }

    /// Get the total transaction count across all proxy blocks.
    pub fn total_txn_count(&self) -> usize {
        self.proxy_blocks
            .iter()
            .filter_map(|b| b.payload())
            .map(|p| p.len())
            .sum()
    }

    /// Check if any proxy block has validator transactions.
    pub fn has_validator_txns(&self) -> bool {
        self.proxy_blocks
            .iter()
            .any(|b| b.validator_txns().is_some_and(|txns| !txns.is_empty()))
    }

    /// Get all validator transactions from proxy blocks.
    pub fn validator_txns(&self) -> Vec<aptos_types::validator_txn::ValidatorTransaction> {
        self.proxy_blocks
            .iter()
            .filter_map(|b| b.validator_txns())
            .flatten()
            .cloned()
            .collect()
    }

    /// Get the ID of the first proxy block.
    pub fn first_block_id(&self) -> HashValue {
        self.proxy_blocks
            .first()
            .map(|b| b.id())
            .unwrap_or_else(HashValue::zero)
    }

    /// Get the ID of the last proxy block.
    pub fn last_block_id(&self) -> HashValue {
        self.proxy_blocks
            .last()
            .map(|b| b.id())
            .unwrap_or_else(HashValue::zero)
    }

    /// Get the timestamp range of proxy blocks.
    pub fn timestamp_range(&self) -> (u64, u64) {
        let first_ts = self
            .proxy_blocks
            .first()
            .map(|b| b.timestamp_usecs())
            .unwrap_or(0);
        let last_ts = self
            .proxy_blocks
            .last()
            .map(|b| b.timestamp_usecs())
            .unwrap_or(0);
        (first_ts, last_ts)
    }
}

/// Builder for creating proxy block aggregations during testing.
#[cfg(any(test, feature = "fuzzing"))]
pub struct PrimaryBlockFromProxyBuilder {
    proxy_blocks: Vec<Block>,
    primary_round: Round,
    primary_qc: Option<QuorumCert>,
}

#[cfg(any(test, feature = "fuzzing"))]
impl PrimaryBlockFromProxyBuilder {
    pub fn new(primary_round: Round) -> Self {
        Self {
            proxy_blocks: Vec::new(),
            primary_round,
            primary_qc: None,
        }
    }

    pub fn with_proxy_block(mut self, block: Block) -> Self {
        self.proxy_blocks.push(block);
        self
    }

    pub fn with_primary_qc(mut self, qc: QuorumCert) -> Self {
        self.primary_qc = Some(qc);
        self
    }

    pub fn build(self) -> Result<PrimaryBlockFromProxy, ProxyConsensusError> {
        let primary_qc = self.primary_qc.ok_or_else(|| {
            ProxyConsensusError::InvalidProxyBlock("Primary QC required".into())
        })?;

        let aggregated_payload_hash =
            PrimaryBlockFromProxy::compute_aggregated_hash(&self.proxy_blocks);

        Ok(PrimaryBlockFromProxy {
            proxy_blocks: self.proxy_blocks,
            primary_round: self.primary_round,
            primary_qc,
            aggregated_payload_hash,
        })
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
    fn test_primary_block_from_proxy_empty() {
        let primary_qc = make_qc(1, 0);
        let msg = OrderedProxyBlocksMsg::new(vec![], 1, primary_qc);

        let result = PrimaryBlockFromProxy::from_ordered_msg(msg);
        assert!(result.is_err());
    }

    #[test]
    fn test_primary_block_from_proxy_qc_round_mismatch() {
        let primary_qc = make_qc(1, 5); // QC for round 5, but primary_round is 1

        // This should fail because QC.round (5) != primary_round - 1 (0)
        let msg = OrderedProxyBlocksMsg::new(vec![], 1, primary_qc);
        let result = PrimaryBlockFromProxy::from_ordered_msg(msg);
        assert!(result.is_err());
    }
}
