// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Safety rules for proxy consensus.
//!
//! ProxySafetyRules maintains SEPARATE persistent storage from primary SafetyRules
//! to ensure independent voting safety for proxy consensus. This is critical since
//! proxy consensus operates at a different pace than primary consensus.
//!
//! Key differences from primary SafetyRules:
//! - In-process only (no remote service), since proxy validators are co-located
//! - Simpler initialization (epoch state received via channel)
//! - Skips external signature verification (trust within proxy validator set)

use crate::proxy_error::ProxyConsensusError;
use aptos_consensus_types::{
    block::Block,
    common::{Author, Round},
    order_vote::OrderVote,
    order_vote_proposal::OrderVoteProposal,
    quorum_cert::QuorumCert,
    safety_data::SafetyData,
    timeout_2chain::TwoChainTimeoutCertificate,
    vote::Vote,
    vote_data::VoteData,
    vote_proposal::VoteProposal,
};
use aptos_crypto::{bls12381, hash::CryptoHash, HashValue};
use aptos_logger::prelude::*;
use aptos_secure_storage::{InMemoryStorage, KVStorage, Storage};
use aptos_types::{
    block_info::BlockInfo,
    epoch_state::EpochState,
    ledger_info::LedgerInfo,
    validator_signer::ValidatorSigner,
};
use std::sync::Arc;

/// Storage keys for proxy safety data (separate namespace from primary)
const PROXY_SAFETY_DATA: &str = "proxy_safety_data";
const PROXY_OWNER_ACCOUNT: &str = "proxy_owner_account";

/// Proxy-specific safety rules with independent persistent storage.
pub struct ProxySafetyRules {
    /// Persistent storage for proxy safety data (SEPARATE from primary).
    /// For phase 1, we use in-memory storage. In production, this should be
    /// a separate persistent store to survive restarts.
    storage: Storage,
    /// Cached safety data for performance
    cached_safety_data: Option<SafetyData>,
    /// Validator signer for signing votes
    validator_signer: Option<ValidatorSigner>,
    /// Current epoch state (set during initialization)
    epoch_state: Option<Arc<EpochState>>,
}

impl ProxySafetyRules {
    /// Create new proxy safety rules with in-memory storage.
    ///
    /// In production, this should use persistent storage separate from primary.
    pub fn new(author: Author, consensus_private_key: bls12381::PrivateKey) -> Self {
        let mut storage = Storage::from(InMemoryStorage::new());

        // Initialize owner account
        storage
            .set(PROXY_OWNER_ACCOUNT, author)
            .expect("Failed to set proxy owner account");

        // Initialize safety data with epoch 0 (will be updated on initialization)
        let safety_data = SafetyData::new(0, 0, 0, 0, None, 0);
        storage
            .set(PROXY_SAFETY_DATA, safety_data.clone())
            .expect("Failed to set initial proxy safety data");

        Self {
            storage,
            cached_safety_data: Some(safety_data),
            validator_signer: Some(ValidatorSigner::new(
                author,
                Arc::new(consensus_private_key),
            )),
            epoch_state: None,
        }
    }

    /// Initialize proxy safety rules for a new epoch.
    ///
    /// Unlike primary SafetyRules which uses EpochChangeProof, proxy safety rules
    /// receive the epoch state directly from the primary RoundManager via channel.
    pub fn initialize(&mut self, epoch_state: Arc<EpochState>) -> Result<(), ProxyConsensusError> {
        let current_epoch = self.safety_data()?.epoch;

        if epoch_state.epoch > current_epoch {
            // Start new epoch - reset safety data
            let new_safety_data = SafetyData::new(epoch_state.epoch, 0, 0, 0, None, 0);
            self.set_safety_data(new_safety_data)?;
            info!(
                "ProxySafetyRules: initialized for epoch {}",
                epoch_state.epoch
            );
        } else if epoch_state.epoch < current_epoch {
            return Err(ProxyConsensusError::EpochMismatch {
                expected: current_epoch,
                got: epoch_state.epoch,
            });
        }

        self.epoch_state = Some(epoch_state);
        Ok(())
    }

    /// Get the author (validator address).
    pub fn author(&self) -> Result<Author, ProxyConsensusError> {
        self.storage
            .get::<Author>(PROXY_OWNER_ACCOUNT)
            .map(|v| v.value)
            .map_err(|e| ProxyConsensusError::SafetyRulesError(e.to_string()))
    }

    /// Get the current safety data.
    pub fn safety_data(&mut self) -> Result<SafetyData, ProxyConsensusError> {
        if let Some(cached) = self.cached_safety_data.clone() {
            return Ok(cached);
        }

        let safety_data: SafetyData = self
            .storage
            .get(PROXY_SAFETY_DATA)
            .map(|v| v.value)
            .map_err(|e| ProxyConsensusError::SafetyRulesError(e.to_string()))?;
        self.cached_safety_data = Some(safety_data.clone());
        Ok(safety_data)
    }

    /// Set the safety data (persists to storage).
    fn set_safety_data(&mut self, data: SafetyData) -> Result<(), ProxyConsensusError> {
        self.storage
            .set(PROXY_SAFETY_DATA, data.clone())
            .map_err(|e| ProxyConsensusError::SafetyRulesError(e.to_string()))?;
        self.cached_safety_data = Some(data);
        Ok(())
    }

    /// Get the validator signer.
    fn signer(&self) -> Result<&ValidatorSigner, ProxyConsensusError> {
        self.validator_signer
            .as_ref()
            .ok_or_else(|| ProxyConsensusError::SafetyRulesError("Signer not initialized".into()))
    }

    /// Get the epoch state.
    fn epoch_state(&self) -> Result<&EpochState, ProxyConsensusError> {
        self.epoch_state
            .as_ref()
            .map(|s| s.as_ref())
            .ok_or_else(|| {
                ProxyConsensusError::SafetyRulesError("Epoch state not initialized".into())
            })
    }

    /// Sign a message.
    fn sign<T: serde::Serialize + CryptoHash>(
        &self,
        message: &T,
    ) -> Result<bls12381::Signature, ProxyConsensusError> {
        self.signer()?
            .sign(message)
            .map_err(|e| ProxyConsensusError::SafetyRulesError(e.to_string()))
    }

    /// Construct and sign a vote for a proxy block.
    pub fn construct_and_sign_proxy_vote(
        &mut self,
        vote_proposal: &VoteProposal,
        timeout_cert: Option<&TwoChainTimeoutCertificate>,
    ) -> Result<Vote, ProxyConsensusError> {
        self.signer()?;

        let proposed_block = vote_proposal.block();
        let mut safety_data = self.safety_data()?;

        // Verify epoch
        if proposed_block.epoch() != safety_data.epoch {
            return Err(ProxyConsensusError::EpochMismatch {
                expected: safety_data.epoch,
                got: proposed_block.epoch(),
            });
        }

        // Check if already voted on this round (return cached vote)
        if let Some(vote) = safety_data.last_vote.clone() {
            if vote.vote_data().proposed().round() == proposed_block.round() {
                return Ok(vote);
            }
        }

        // Generate vote data
        let vote_data = vote_proposal
            .gen_vote_data()
            .map_err(|e| ProxyConsensusError::InvalidProxyBlock(e.to_string()))?;

        // Verify and update last vote round (first voting rule)
        if proposed_block.round() <= safety_data.last_voted_round {
            return Err(ProxyConsensusError::RoundTooOld {
                round: proposed_block.round(),
                last_voted: safety_data.last_voted_round,
            });
        }
        safety_data.last_voted_round = proposed_block.round();

        // Safe to vote check (2-chain rule)
        self.safe_to_vote(proposed_block, timeout_cert)?;

        // Observe QC (update one_chain_round and preferred_round)
        self.observe_qc(proposed_block.quorum_cert(), &mut safety_data);

        // Construct ledger info (with 2-chain commit rule)
        let ledger_info = self.construct_ledger_info_2chain(proposed_block, vote_data.hash())?;

        // Sign the vote
        let author = self.signer()?.author();
        let signature = self.sign(&ledger_info)?;
        let vote = Vote::new_with_signature(vote_data, author, ledger_info, signature);

        // Cache the vote and persist
        safety_data.last_vote = Some(vote.clone());
        self.set_safety_data(safety_data)?;

        Ok(vote)
    }

    /// Construct and sign an order vote for a proxy block.
    pub fn construct_and_sign_proxy_order_vote(
        &mut self,
        order_vote_proposal: &OrderVoteProposal,
    ) -> Result<OrderVote, ProxyConsensusError> {
        self.signer()?;

        let proposed_block = order_vote_proposal.block();
        let mut safety_data = self.safety_data()?;

        // Verify epoch
        if proposed_block.epoch() != safety_data.epoch {
            return Err(ProxyConsensusError::EpochMismatch {
                expected: safety_data.epoch,
                got: proposed_block.epoch(),
            });
        }

        // Verify QC matches block
        let qc = order_vote_proposal.quorum_cert();
        if qc.certified_block() != order_vote_proposal.block_info() {
            return Err(ProxyConsensusError::InvalidProxyBlock(
                "QC doesn't match block info".into(),
            ));
        }
        if qc.certified_block().id() != proposed_block.id() {
            return Err(ProxyConsensusError::InvalidProxyBlock(
                "QC block ID doesn't match proposed block".into(),
            ));
        }

        // Observe QC
        self.observe_qc(qc, &mut safety_data);

        // Safe for order vote check
        if proposed_block.round() <= safety_data.highest_timeout_round {
            return Err(ProxyConsensusError::NotSafeForOrderVote {
                round: proposed_block.round(),
                highest_timeout_round: safety_data.highest_timeout_round,
            });
        }

        // Construct and sign order vote
        let author = self.signer()?.author();
        let ledger_info =
            LedgerInfo::new(order_vote_proposal.block_info().clone(), HashValue::zero());
        let signature = self.sign(&ledger_info)?;
        let order_vote = OrderVote::new_with_signature(author, ledger_info, signature);

        self.set_safety_data(safety_data)?;
        Ok(order_vote)
    }

    /// Sign a proxy proposal (block data).
    pub fn sign_proxy_proposal(
        &mut self,
        block_data: &aptos_consensus_types::block_data::BlockData,
    ) -> Result<bls12381::Signature, ProxyConsensusError> {
        self.signer()?;

        // Verify author matches signer
        let signer_author = self.signer()?.author();
        if block_data.author() != Some(signer_author) {
            return Err(ProxyConsensusError::InvalidProxyBlock(
                "Proposal author doesn't match signer".into(),
            ));
        }

        let mut safety_data = self.safety_data()?;

        // Verify epoch
        if block_data.epoch() != safety_data.epoch {
            return Err(ProxyConsensusError::EpochMismatch {
                expected: safety_data.epoch,
                got: block_data.epoch(),
            });
        }

        // Verify round is higher than last voted
        if block_data.round() <= safety_data.last_voted_round {
            return Err(ProxyConsensusError::InvalidProxyBlock(format!(
                "Proposed round {} is not higher than last voted round {}",
                block_data.round(),
                safety_data.last_voted_round
            )));
        }

        // Update preferred round from QC
        self.observe_qc(block_data.quorum_cert(), &mut safety_data);
        // Note: we don't persist here to save latency (will be persisted upon voting)

        self.sign(block_data)
    }

    /// Update one_chain_round and preferred_round from a QC.
    fn observe_qc(&self, qc: &QuorumCert, safety_data: &mut SafetyData) {
        let one_chain = qc.certified_block().round();
        let two_chain = qc.parent_block().round();

        if one_chain > safety_data.one_chain_round {
            safety_data.one_chain_round = one_chain;
        }
        if two_chain > safety_data.preferred_round {
            safety_data.preferred_round = two_chain;
        }
    }

    /// Check if it's safe to vote according to 2-chain rules.
    ///
    /// Safe to vote if:
    /// 1. block.round == block.qc.round + 1, OR
    /// 2. block.round == tc.round + 1 && block.qc.round >= tc.highest_hqc.round
    fn safe_to_vote(
        &self,
        block: &Block,
        maybe_tc: Option<&TwoChainTimeoutCertificate>,
    ) -> Result<(), ProxyConsensusError> {
        let round = block.round();
        let qc_round = block.quorum_cert().certified_block().round();
        let tc_round = maybe_tc.map_or(0, |tc| tc.round());
        let hqc_round = maybe_tc.map_or(0, |tc| tc.highest_hqc_round());

        fn next_round(r: Round) -> Option<Round> {
            r.checked_add(1)
        }

        if Some(round) == next_round(qc_round)
            || (Some(round) == next_round(tc_round) && qc_round >= hqc_round)
        {
            Ok(())
        } else {
            Err(ProxyConsensusError::NotSafeToVote {
                round,
                qc_round,
                tc_round,
                hqc_round,
            })
        }
    }

    /// Construct ledger info according to 2-chain commit rule.
    ///
    /// Commits B0 if B0 <- B1 and round(B0) + 1 = round(B1)
    fn construct_ledger_info_2chain(
        &self,
        proposed_block: &Block,
        consensus_data_hash: HashValue,
    ) -> Result<LedgerInfo, ProxyConsensusError> {
        let block1 = proposed_block.round();
        let block0 = proposed_block.quorum_cert().certified_block().round();

        // Check 2-chain commit rule
        let commit = block0.checked_add(1) == Some(block1);

        let commit_info = if commit {
            proposed_block.quorum_cert().certified_block().clone()
        } else {
            BlockInfo::empty()
        };

        Ok(LedgerInfo::new(commit_info, consensus_data_hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptos_consensus_types::vote_data::VoteData;
    use aptos_crypto::PrivateKey;
    use aptos_types::{
        aggregate_signature::AggregateSignature,
        ledger_info::LedgerInfoWithSignatures,
        validator_signer::ValidatorSigner,
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
    fn test_proxy_safety_rules_creation() {
        let signer = ValidatorSigner::from_int(0);
        let author = signer.author();
        let private_key = signer.private_key().clone();

        let mut rules = ProxySafetyRules::new(author, private_key);

        assert_eq!(rules.author().unwrap(), author);
        assert_eq!(rules.safety_data().unwrap().epoch, 0);
    }

    #[test]
    fn test_proxy_safety_rules_initialization() {
        let signer = ValidatorSigner::from_int(0);
        let author = signer.author();
        let private_key = signer.private_key().clone();

        let mut rules = ProxySafetyRules::new(author, private_key);

        // Create a minimal epoch state
        let epoch_state = Arc::new(EpochState::empty());

        rules.initialize(epoch_state).unwrap();
        assert_eq!(rules.safety_data().unwrap().epoch, 0); // empty epoch state has epoch 0
    }

    #[test]
    fn test_observe_qc() {
        let signer = ValidatorSigner::from_int(0);
        let author = signer.author();
        let private_key = signer.private_key().clone();

        let rules = ProxySafetyRules::new(author, private_key);

        let qc = make_qc(1, 5);
        let mut safety_data = SafetyData::new(1, 0, 0, 0, None, 0);

        rules.observe_qc(&qc, &mut safety_data);

        assert_eq!(safety_data.one_chain_round, 5);
    }
}
