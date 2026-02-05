// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Proxy Primary Consensus
//!
//! This crate implements proxy primary consensus where a subset of validators (proxies)
//! run fast local consensus and forward ordered blocks to the full validator set (primaries).
//!
//! # Architecture
//!
//! - **ProxyBlockStore**: In-memory storage for proxy blocks
//! - **ProxyRoundManager**: Main event loop for proxy consensus
//! - **ProxySafetyRules**: Separate safety state from primary consensus
//! - **ProxyLeaderElection**: Simple round-robin among proxy validators
//! - **ProxyNetworkSender**: Routes messages to proxy validators
//! - **PrimaryIntegration**: Aggregates proxy blocks into primary blocks

#![forbid(unsafe_code)]

pub mod primary_integration;
pub mod proxy_block_store;
pub mod proxy_error;
pub mod proxy_leader_election;
pub mod proxy_metrics;
pub mod proxy_network_sender;
pub mod proxy_proposal_generator;
pub mod proxy_round_manager;
pub mod proxy_safety_rules;

pub use primary_integration::PrimaryBlockFromProxy;
pub use proxy_block_store::{ProxyBlockReader, ProxyBlockStore};
pub use proxy_error::ProxyConsensusError;
pub use proxy_leader_election::ProxyLeaderElection;
pub use proxy_network_sender::ProxyNetworkSender;
pub use proxy_round_manager::{
    PrimaryToProxyEvent, ProxyRoundManager, ProxyRoundManagerConfig, ProxyToPrimaryEvent,
    VerifiedProxyEvent,
};
pub use proxy_safety_rules::ProxySafetyRules;
