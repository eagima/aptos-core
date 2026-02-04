// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Error types for proxy primary consensus.

use aptos_consensus_types::common::Round;
use aptos_crypto::HashValue;
use thiserror::Error;

/// Errors that can occur during proxy consensus.
#[derive(Debug, Error)]
pub enum ProxyConsensusError {
    #[error("Invalid primary_round: expected {expected}, got {got}")]
    InvalidPrimaryRound { expected: Round, got: Round },

    #[error("Primary QC round mismatch: expected {expected}, got {got}")]
    PrimaryQCRoundMismatch { expected: Round, got: Round },

    #[error("Parent block not found: {0}")]
    ParentNotFound(HashValue),

    #[error("Genesis block mismatch: expected {expected}, got {got}")]
    GenesisMismatch { expected: HashValue, got: HashValue },

    #[error("Not a proxy validator")]
    NotProxyValidator,

    #[error("Proxy consensus not active")]
    NotActive,

    #[error("Proxy consensus shutdown: TC round {tc_round} >= primary_round {primary_round}")]
    ShutdownTriggered { tc_round: Round, primary_round: Round },

    #[error("Backpressure: too many proxy blocks ({count}) for primary round {primary_round}")]
    BackpressureExceeded { count: u64, primary_round: Round },

    #[error("Block retrieval failed: {0}")]
    BlockRetrievalFailed(String),

    #[error("Block not ordered: {0}")]
    BlockNotOrdered(HashValue),

    #[error("Invalid proxy block: {0}")]
    InvalidProxyBlock(String),

    #[error("Proxy block chain broken at round {round}")]
    BrokenChain { round: Round },

    #[error("Safety rules error: {0}")]
    SafetyRules(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Channel closed")]
    ChannelClosed,

    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),

    #[error("Already proposed in round {round}")]
    AlreadyProposed { round: Round },

    #[error("Payload pull error: {0}")]
    PayloadPullError(String),

    #[error("Epoch mismatch: expected {expected}, got {got}")]
    EpochMismatch { expected: u64, got: u64 },

    #[error("Safety rules error: {0}")]
    SafetyRulesError(String),

    #[error("Round too old: {round} <= last_voted {last_voted}")]
    RoundTooOld { round: Round, last_voted: Round },

    #[error("Not safe to vote: round {round}, qc_round {qc_round}, tc_round {tc_round}, hqc_round {hqc_round}")]
    NotSafeToVote {
        round: Round,
        qc_round: Round,
        tc_round: Round,
        hqc_round: Round,
    },

    #[error("Not safe for order vote: round {round} <= highest_timeout_round {highest_timeout_round}")]
    NotSafeForOrderVote {
        round: Round,
        highest_timeout_round: Round,
    },
}

impl From<aptos_safety_rules::Error> for ProxyConsensusError {
    fn from(err: aptos_safety_rules::Error) -> Self {
        ProxyConsensusError::SafetyRules(err.to_string())
    }
}
