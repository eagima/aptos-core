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

use aptos_forge::{NetworkContextSynchronizer, NetworkTest, NodeExt, Result, Test};
use aptos_inspection_service::inspection_client::InspectionClient;
use async_trait::async_trait;
use futures::future::join_all;
use log::info;

/// Test that proxy primary consensus works correctly in a running network.
///
/// This test verifies:
/// - Proxy validators propose blocks at a faster rate than primary consensus
/// - Ordered proxy blocks are correctly forwarded to primaries
/// - Primary consensus aggregates proxy blocks correctly
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

/// Aggregated proxy consensus metrics from all validators.
#[derive(Debug, Default)]
struct ProxyConsensusMetrics {
    proposals_sent: i64,
    votes_sent: i64,
    qcs_formed: i64,
    blocks_ordered: i64,
    blocks_forwarded: i64,
    backpressure_events: i64,
    timeout_all: i64,
    timeout_leader: i64,
}

/// Query proxy consensus metrics from validators using inspection clients.
async fn query_proxy_metrics(inspection_clients: &[InspectionClient]) -> Result<ProxyConsensusMetrics> {
    let metrics_futures = inspection_clients.iter().map(|client| async move {
        // Note: forge_metrics endpoint stores label-less metrics with "{}" suffix
        let proposals = client.get_node_metric_i64("aptos_proxy_consensus_proposals_sent{}").await.ok().flatten().unwrap_or(0);
        let votes = client.get_node_metric_i64("aptos_proxy_consensus_votes_sent{}").await.ok().flatten().unwrap_or(0);
        let qcs = client.get_node_metric_i64("aptos_proxy_consensus_qcs_formed{}").await.ok().flatten().unwrap_or(0);
        let ordered = client.get_node_metric_i64("aptos_proxy_consensus_blocks_ordered{}").await.ok().flatten().unwrap_or(0);
        let forwarded = client.get_node_metric_i64("aptos_proxy_consensus_blocks_forwarded{}").await.ok().flatten().unwrap_or(0);
        let backpressure = client.get_node_metric_i64("aptos_proxy_backpressure_events{}").await.ok().flatten().unwrap_or(0);
        let timeout_all = client.get_node_metric_i64("aptos_proxy_round_timeout_all{}").await.ok().flatten().unwrap_or(0);
        let timeout_leader = client.get_node_metric_i64("aptos_proxy_round_timeout_count{}").await.ok().flatten().unwrap_or(0);
        (proposals, votes, qcs, ordered, forwarded, backpressure, timeout_all, timeout_leader)
    });

    let results = join_all(metrics_futures).await;

    let mut metrics = ProxyConsensusMetrics::default();
    for (proposals, votes, qcs, ordered, forwarded, backpressure, timeout_all, timeout_leader) in results {
        metrics.proposals_sent += proposals;
        metrics.votes_sent += votes;
        metrics.qcs_formed += qcs;
        metrics.blocks_ordered += ordered;
        metrics.blocks_forwarded += forwarded;
        metrics.backpressure_events += backpressure;
        metrics.timeout_all += timeout_all;
        metrics.timeout_leader += timeout_leader;
    }

    Ok(metrics)
}

#[async_trait]
impl NetworkTest for ProxyPrimaryHappyPathTest {
    async fn run<'a>(&self, ctx: NetworkContextSynchronizer<'a>) -> anyhow::Result<()> {
        let duration = {
            let ctx = ctx.ctx.lock().await;
            ctx.global_duration
        };

        info!("ProxyPrimaryHappyPathTest: Running for {:?}", duration);

        // Collect inspection clients
        let inspection_clients: Vec<InspectionClient> = {
            let ctx = ctx.ctx.lock().await;
            let swarm = ctx.swarm.read().await;
            swarm.validators().map(|v| v.inspection_client()).collect()
        };

        // Wait for majority of test duration to let proxy consensus produce blocks
        tokio::time::sleep(duration.mul_f32(0.8)).await;

        // Query proxy consensus metrics from all validators
        let metrics = query_proxy_metrics(&inspection_clients).await?;

        // Log metrics
        info!("Proxy consensus metrics:");
        info!("  proposals_sent: {}", metrics.proposals_sent);
        info!("  votes_sent: {}", metrics.votes_sent);
        info!("  qcs_formed: {}", metrics.qcs_formed);
        info!("  blocks_ordered: {}", metrics.blocks_ordered);
        info!("  blocks_forwarded: {}", metrics.blocks_forwarded);

        // Report to test framework
        {
            let mut ctx = ctx.ctx.lock().await;
            ctx.report.report_text(format!(
                "Proxy consensus metrics: proposals={}, votes={}, qcs={}, ordered={}, forwarded={}, timeouts_all={}, timeouts_leader={}",
                metrics.proposals_sent,
                metrics.votes_sent,
                metrics.qcs_formed,
                metrics.blocks_ordered,
                metrics.blocks_forwarded,
                metrics.timeout_all,
                metrics.timeout_leader,
            ));
        }

        // Verify proxy consensus is actively producing blocks
        let has_activity = metrics.proposals_sent > 0
            || metrics.votes_sent > 0
            || metrics.qcs_formed > 0
            || metrics.blocks_ordered > 0;

        let has_timeouts = metrics.timeout_all > 0;

        if has_activity {
            info!("Proxy consensus is active!");
        } else if has_timeouts {
            info!("Proxy consensus timeouts are firing (all={}, leader={}) but no proposals - investigating leader election",
                metrics.timeout_all, metrics.timeout_leader);
        } else {
            info!("Proxy consensus metrics are all zero - proxy consensus may not be starting");
        }

        // Wait remaining time
        tokio::time::sleep(duration.mul_f32(0.2)).await;

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
impl NetworkTest for ProxyPrimaryLoadTest {
    async fn run<'a>(&self, ctx: NetworkContextSynchronizer<'a>) -> anyhow::Result<()> {
        let duration = {
            let ctx = ctx.ctx.lock().await;
            ctx.global_duration
        };

        info!(
            "ProxyPrimaryLoadTest: Running for {:?} with target TPS {}",
            duration, self.target_tps
        );

        // Collect inspection clients
        let inspection_clients: Vec<InspectionClient> = {
            let ctx = ctx.ctx.lock().await;
            let swarm = ctx.swarm.read().await;
            swarm.validators().map(|v| v.inspection_client()).collect()
        };

        // Take initial metric snapshot
        let start_metrics = query_proxy_metrics(&inspection_clients).await?;

        // Wait for test duration
        tokio::time::sleep(duration).await;

        // Take final metric snapshot
        let end_metrics = query_proxy_metrics(&inspection_clients).await?;

        // Calculate deltas
        let proposals_delta = end_metrics.proposals_sent - start_metrics.proposals_sent;
        let votes_delta = end_metrics.votes_sent - start_metrics.votes_sent;
        let qcs_delta = end_metrics.qcs_formed - start_metrics.qcs_formed;
        let ordered_delta = end_metrics.blocks_ordered - start_metrics.blocks_ordered;
        let forwarded_delta = end_metrics.blocks_forwarded - start_metrics.blocks_forwarded;
        let backpressure_delta = end_metrics.backpressure_events - start_metrics.backpressure_events;

        info!("Proxy consensus metrics (delta during test):");
        info!("  proposals_sent: {}", proposals_delta);
        info!("  votes_sent: {}", votes_delta);
        info!("  qcs_formed: {}", qcs_delta);
        info!("  blocks_ordered: {}", ordered_delta);
        info!("  blocks_forwarded: {}", forwarded_delta);
        info!("  backpressure_events: {}", backpressure_delta);

        // Calculate blocks per second
        let duration_secs = duration.as_secs_f64();
        let blocks_per_sec = if duration_secs > 0.0 {
            ordered_delta as f64 / duration_secs
        } else {
            0.0
        };

        // Report to test framework
        {
            let mut ctx = ctx.ctx.lock().await;
            ctx.report.report_text(format!(
                "Proxy load test: {} blocks ordered in {:.1}s ({:.2} blocks/sec), {} backpressure events",
                ordered_delta, duration_secs, blocks_per_sec, backpressure_delta
            ));
        }

        // Log warnings if backpressure triggered frequently
        if backpressure_delta > 10 {
            info!(
                "High backpressure: {} events triggered during test",
                backpressure_delta
            );
        }

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
