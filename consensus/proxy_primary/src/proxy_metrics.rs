// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Metrics for proxy primary consensus.

use once_cell::sync::Lazy;
use prometheus::{
    register_histogram, register_histogram_vec, register_int_counter, register_int_counter_vec,
    register_int_gauge, Histogram, HistogramVec, IntCounter, IntCounterVec, IntGauge,
};

// ============================================================================
// Core Consensus Metrics
// ============================================================================

/// Number of proxy proposals sent
pub static PROXY_CONSENSUS_PROPOSALS_SENT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_consensus_proposals_sent",
        "Number of proxy proposals sent"
    )
    .unwrap()
});

/// Number of proxy votes sent
pub static PROXY_CONSENSUS_VOTES_SENT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_consensus_votes_sent",
        "Number of proxy votes sent"
    )
    .unwrap()
});

/// Number of proxy order votes sent
pub static PROXY_CONSENSUS_ORDER_VOTES_SENT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_consensus_order_votes_sent",
        "Number of proxy order votes sent"
    )
    .unwrap()
});

/// Number of proxy blocks ordered
pub static PROXY_CONSENSUS_BLOCKS_ORDERED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_consensus_blocks_ordered",
        "Number of proxy blocks ordered"
    )
    .unwrap()
});

/// Number of proxy consensus QCs formed
pub static PROXY_CONSENSUS_QCS_FORMED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_consensus_qcs_formed",
        "Number of proxy consensus QCs formed"
    )
    .unwrap()
});

/// Number of ordered proxy block messages forwarded to primaries
pub static PROXY_CONSENSUS_BLOCKS_FORWARDED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_consensus_blocks_forwarded",
        "Number of ordered proxy block messages forwarded to primaries"
    )
    .unwrap()
});

// ============================================================================
// Timing Metrics
// ============================================================================

/// Latency from primary QC received to proxy block with QC ordered
pub static PROXY_CONSENSUS_PRIMARY_QC_LATENCY: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!(
        "aptos_proxy_consensus_primary_qc_latency_seconds",
        "Latency from primary QC received to proxy block with QC ordered"
    )
    .unwrap()
});

/// Time to order a proxy block (from proposal to order cert)
pub static PROXY_BLOCK_ORDERING_LATENCY: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!(
        "aptos_proxy_block_ordering_latency_seconds",
        "Time to order a proxy block from proposal to order cert"
    )
    .unwrap()
});

/// Time for proxy proposal generation
pub static PROXY_PROPOSAL_GENERATION_LATENCY: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!(
        "aptos_proxy_proposal_generation_latency_seconds",
        "Time for proxy proposal generation"
    )
    .unwrap()
});

// ============================================================================
// State Metrics
// ============================================================================

/// Number of proxy blocks per primary round
pub static PROXY_BLOCKS_PER_PRIMARY_ROUND: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!(
        "aptos_proxy_blocks_per_primary_round",
        "Number of proxy blocks per primary round"
    )
    .unwrap()
});

/// Current proxy round
pub static PROXY_CURRENT_ROUND: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "aptos_proxy_current_round",
        "Current proxy consensus round"
    )
    .unwrap()
});

/// Current primary round tracked by proxy
pub static PROXY_CURRENT_PRIMARY_ROUND: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "aptos_proxy_current_primary_round",
        "Current primary round tracked by proxy consensus"
    )
    .unwrap()
});

/// Number of blocks in proxy block store
pub static PROXY_BLOCK_STORE_SIZE: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "aptos_proxy_block_store_size",
        "Number of blocks in proxy block store"
    )
    .unwrap()
});

// ============================================================================
// Event Counters
// ============================================================================

/// Proxy consensus shutdown events
pub static PROXY_CONSENSUS_SHUTDOWN_COUNT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_consensus_shutdown_count",
        "Number of proxy consensus shutdown events"
    )
    .unwrap()
});

/// Backpressure events
pub static PROXY_BACKPRESSURE_EVENTS: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_backpressure_events",
        "Number of backpressure events in proxy consensus"
    )
    .unwrap()
});

/// Round timeout events (all, including non-leader)
pub static PROXY_ROUND_TIMEOUT_ALL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_round_timeout_all",
        "Number of round timeout events in proxy consensus (all nodes)"
    )
    .unwrap()
});

/// Round timeout events (leader only, proposal sent)
pub static PROXY_ROUND_TIMEOUT_COUNT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "aptos_proxy_round_timeout_count",
        "Number of round timeout events in proxy consensus (leader)"
    )
    .unwrap()
});

/// State transitions by type
pub static PROXY_STATE_TRANSITIONS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "aptos_proxy_state_transitions",
        "Proxy consensus state transitions",
        &["from_state", "to_state"]
    )
    .unwrap()
});

// ============================================================================
// Message Counters
// ============================================================================

/// Messages received by type
pub static PROXY_MESSAGES_RECEIVED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "aptos_proxy_messages_received",
        "Proxy consensus messages received by type",
        &["message_type"]
    )
    .unwrap()
});

/// Messages sent by type
pub static PROXY_MESSAGES_SENT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "aptos_proxy_messages_sent",
        "Proxy consensus messages sent by type",
        &["message_type"]
    )
    .unwrap()
});

// ============================================================================
// Error Counters
// ============================================================================

/// Errors by type
pub static PROXY_ERRORS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "aptos_proxy_errors",
        "Proxy consensus errors by type",
        &["error_type"]
    )
    .unwrap()
});

// ============================================================================
// Vote Aggregation Metrics
// ============================================================================

/// Votes received per round
pub static PROXY_VOTES_RECEIVED: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "aptos_proxy_votes_received",
        "Votes received per proxy round",
        &["vote_type"]
    )
    .unwrap()
});
