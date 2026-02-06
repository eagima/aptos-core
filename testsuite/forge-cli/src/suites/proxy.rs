// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Forge test suites for Proxy Primary Consensus.

use aptos_forge::{success_criteria::SuccessCriteria, ForgeConfig};
use aptos_testcases::proxy_primary_test::{ProxyPrimaryHappyPathTest, ProxyPrimaryLoadTest};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};

/// Get a proxy consensus test by name.
pub fn get_proxy_test(test_name: &str, _duration: Duration) -> Option<ForgeConfig> {
    let test = match test_name {
        "proxy_primary_happy_path" => proxy_primary_happy_path_test(),
        "proxy_primary_load" => proxy_primary_load_test(),
        _ => return None,
    };
    Some(test)
}

/// Basic happy path test for proxy primary consensus.
///
/// 7 validators split into 3 geo-distributed regions with injected
/// inter-region latency (150-300ms). This simulates production topology
/// where primary consensus is slow (geo-distributed quorum) while proxy
/// consensus accumulates multiple blocks per primary round.
pub fn proxy_primary_happy_path_test() -> ForgeConfig {
    ForgeConfig::default()
        .with_initial_validator_count(NonZeroUsize::new(7).unwrap())
        .add_network_test(ProxyPrimaryHappyPathTest::default())
        .with_validator_override_node_config_fn(Arc::new(|config, _| {
            config.consensus.enable_proxy_consensus = true;
            // Proxy at 1s (matching original consensus default), primary at 10s
            // to ensure proxy accumulates multiple blocks per primary round.
            config.consensus.proxy_consensus_config.round_initial_timeout_ms = 1000;
            config.consensus.round_initial_timeout_ms = 10000;
        }))
        // Simple success criteria for local swarm (no Prometheus needed)
        .with_success_criteria(
            SuccessCriteria::new(10) // Low TPS threshold for Phase 1 testing
                .add_no_restarts()
                .add_wait_for_catchup_s(30),
        )
}

/// Load test for proxy primary consensus.
fn proxy_primary_load_test() -> ForgeConfig {
    ForgeConfig::default()
        .with_initial_validator_count(NonZeroUsize::new(4).unwrap())
        .add_network_test(ProxyPrimaryLoadTest::default())
        .with_validator_override_node_config_fn(Arc::new(|config, _| {
            config.consensus.enable_proxy_consensus = true;
            config.consensus.proxy_consensus_config.round_initial_timeout_ms = 100;
            config.consensus.round_initial_timeout_ms = 10000;
        }))
        // Simple success criteria for local swarm (no Prometheus needed)
        .with_success_criteria(
            SuccessCriteria::new(10) // Low TPS threshold for Phase 1 testing
                .add_no_restarts()
                .add_wait_for_catchup_s(30),
        )
}
