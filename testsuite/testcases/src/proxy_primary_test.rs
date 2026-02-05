// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Forge E2E tests for Proxy Primary Consensus.
//!
//! Proxy Primary Consensus is a two-layer consensus where:
//! 1. Proxy validators run fast local consensus (10+ blocks per primary round)
//! 2. Ordered proxy blocks are forwarded to all primaries
//! 3. Primaries aggregate proxy blocks into primary blocks
//!
//! ## Test Requirements
//!
//! To enable proxy consensus, nodes must be configured with:
//! ```yaml
//! consensus:
//!   enable_proxy_consensus: true
//!   proxy_consensus_config:
//!     round_initial_timeout_ms: 100
//!     max_proxy_blocks_per_primary_round: 20
//! ```
//!
//! ## Metrics to Verify
//!
//! - `aptos_proxy_consensus_proposals_sent` > 0
//! - `aptos_proxy_consensus_votes_sent` > 0
//! - `aptos_proxy_consensus_blocks_ordered` > 0
//! - `aptos_proxy_consensus_blocks_forwarded` > 0
//! - `aptos_proxy_blocks_per_primary_round` averaging 3+ blocks
//!
//! ## Test Scenarios (TODO: Implement when proxy consensus is fully integrated)
//!
//! 1. `test_proxy_primary_happy_path` - Normal operation, verify throughput
//! 2. `test_proxy_primary_with_load` - High transaction load, verify proxy blocks scale
//! 3. `test_proxy_primary_latency` - Measure latency improvement vs non-proxy baseline

use crate::NetworkLoadTest;
use aptos_forge::{Result, Test, TestReport};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};

/// Test that proxy primary consensus works correctly in a running network.
///
/// This test verifies:
/// - Proxy validators propose blocks at a faster rate than primary consensus
/// - Ordered proxy blocks are correctly forwarded to primaries
/// - Primary consensus aggregates proxy blocks correctly
/// - Transaction throughput is improved compared to baseline
pub struct ProxyPrimaryHappyPathTest {
    /// Expected minimum transactions per second
    pub min_tps: usize,
    /// Expected minimum proxy blocks per primary round
    pub min_proxy_blocks_per_round: usize,
}

impl Default for ProxyPrimaryHappyPathTest {
    fn default() -> Self {
        Self {
            min_tps: 1000,
            min_proxy_blocks_per_round: 3,
        }
    }
}

impl Test for ProxyPrimaryHappyPathTest {
    fn name(&self) -> &'static str {
        "proxy primary happy path test"
    }
}

#[async_trait]
impl NetworkLoadTest for ProxyPrimaryHappyPathTest {
    async fn test(
        &self,
        _swarm: Arc<tokio::sync::RwLock<Box<dyn aptos_forge::Swarm>>>,
        _report: &mut TestReport,
        duration: Duration,
    ) -> Result<()> {
        // TODO: Implement when proxy consensus is fully integrated
        //
        // 1. Verify proxy consensus is enabled on all validators
        // 2. Wait for some proxy blocks to be ordered
        // 3. Check proxy consensus metrics:
        //    - aptos_proxy_consensus_proposals_sent
        //    - aptos_proxy_consensus_votes_sent
        //    - aptos_proxy_consensus_blocks_ordered
        //    - aptos_proxy_consensus_blocks_forwarded
        //    - aptos_proxy_blocks_per_primary_round
        // 4. Verify TPS meets minimum threshold
        // 5. Report results

        log::info!(
            "ProxyPrimaryHappyPathTest: Running for {:?} (placeholder - proxy consensus not yet fully integrated)",
            duration
        );

        // For now, just sleep and succeed
        // This allows the test to be registered but not fail CI
        tokio::time::sleep(duration).await;

        Ok(())
    }
}

/// Test proxy consensus under high load conditions.
///
/// This test verifies:
/// - Proxy blocks scale with transaction load
/// - Backpressure mechanisms work correctly
/// - No block production stalls under load
pub struct ProxyPrimaryLoadTest {
    /// Target TPS to generate
    pub target_tps: usize,
    /// Maximum acceptable latency in ms
    pub max_latency_ms: u64,
}

impl Default for ProxyPrimaryLoadTest {
    fn default() -> Self {
        Self {
            target_tps: 5000,
            max_latency_ms: 3000,
        }
    }
}

impl Test for ProxyPrimaryLoadTest {
    fn name(&self) -> &'static str {
        "proxy primary load test"
    }
}

#[async_trait]
impl NetworkLoadTest for ProxyPrimaryLoadTest {
    async fn test(
        &self,
        _swarm: Arc<tokio::sync::RwLock<Box<dyn aptos_forge::Swarm>>>,
        _report: &mut TestReport,
        duration: Duration,
    ) -> Result<()> {
        // TODO: Implement when proxy consensus is fully integrated
        //
        // 1. Start high-TPS transaction generation
        // 2. Monitor proxy block production rate
        // 3. Verify backpressure works (blocks don't exceed max_proxy_blocks_per_primary_round)
        // 4. Check latency stays within bounds
        // 5. Report metrics

        log::info!(
            "ProxyPrimaryLoadTest: Running for {:?} (placeholder - proxy consensus not yet fully integrated)",
            duration
        );

        tokio::time::sleep(duration).await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_primary_test_defaults() {
        let test = ProxyPrimaryHappyPathTest::default();
        assert_eq!(test.min_tps, 1000);
        assert_eq!(test.min_proxy_blocks_per_round, 3);
    }

    #[test]
    fn test_proxy_primary_load_test_defaults() {
        let test = ProxyPrimaryLoadTest::default();
        assert_eq!(test.target_tps, 5000);
        assert_eq!(test.max_latency_ms, 3000);
    }
}
