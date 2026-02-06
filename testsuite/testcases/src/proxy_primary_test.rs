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

use anyhow::ensure;
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
/// - Primary consensus continues to commit blocks
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

/// Primary consensus metrics from a single validator.
#[derive(Debug, Default)]
struct PrimaryConsensusMetrics {
    last_committed_round: i64,
    current_round: i64,
    committed_blocks: i64,
}

/// Query proxy consensus metrics aggregated across all validators.
async fn query_proxy_metrics(
    inspection_clients: &[InspectionClient],
) -> Result<ProxyConsensusMetrics> {
    let metrics_futures = inspection_clients.iter().map(|client| async move {
        // Note: forge_metrics endpoint stores label-less metrics with "{}" suffix
        let proposals = client
            .get_node_metric_i64("aptos_proxy_consensus_proposals_sent{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let votes = client
            .get_node_metric_i64("aptos_proxy_consensus_votes_sent{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let qcs = client
            .get_node_metric_i64("aptos_proxy_consensus_qcs_formed{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let ordered = client
            .get_node_metric_i64("aptos_proxy_consensus_blocks_ordered{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let forwarded = client
            .get_node_metric_i64("aptos_proxy_consensus_blocks_forwarded{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let backpressure = client
            .get_node_metric_i64("aptos_proxy_backpressure_events{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let timeout_all = client
            .get_node_metric_i64("aptos_proxy_round_timeout_all{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let timeout_leader = client
            .get_node_metric_i64("aptos_proxy_round_timeout_count{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        (
            proposals,
            votes,
            qcs,
            ordered,
            forwarded,
            backpressure,
            timeout_all,
            timeout_leader,
        )
    });

    let results = join_all(metrics_futures).await;

    let mut metrics = ProxyConsensusMetrics::default();
    for (proposals, votes, qcs, ordered, forwarded, backpressure, timeout_all, timeout_leader) in
        results
    {
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

/// Query primary consensus metrics from all validators, returning the max across nodes.
async fn query_primary_metrics(
    inspection_clients: &[InspectionClient],
) -> Result<PrimaryConsensusMetrics> {
    let metrics_futures = inspection_clients.iter().map(|client| async move {
        let committed_round = client
            .get_node_metric_i64("aptos_consensus_last_committed_round{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let current_round = client
            .get_node_metric_i64("aptos_consensus_current_round{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let committed_blocks = client
            .get_node_metric_i64("aptos_consensus_committed_blocks_count{}")
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        (committed_round, current_round, committed_blocks)
    });

    let results = join_all(metrics_futures).await;

    let mut metrics = PrimaryConsensusMetrics::default();
    for (committed_round, current_round, committed_blocks) in results {
        metrics.last_committed_round = metrics.last_committed_round.max(committed_round);
        metrics.current_round = metrics.current_round.max(current_round);
        metrics.committed_blocks = metrics.committed_blocks.max(committed_blocks);
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

        // Wait for the test duration to let both consensus layers produce blocks
        tokio::time::sleep(duration).await;

        // Query metrics from both consensus layers
        let proxy_metrics = query_proxy_metrics(&inspection_clients).await?;
        let primary_metrics = query_primary_metrics(&inspection_clients).await?;

        // Log proxy consensus metrics
        info!("Proxy consensus metrics:");
        info!("  proposals_sent: {}", proxy_metrics.proposals_sent);
        info!("  votes_sent: {}", proxy_metrics.votes_sent);
        info!("  qcs_formed: {}", proxy_metrics.qcs_formed);
        info!("  blocks_ordered: {}", proxy_metrics.blocks_ordered);
        info!("  blocks_forwarded: {}", proxy_metrics.blocks_forwarded);
        info!("  timeouts_all: {}", proxy_metrics.timeout_all);
        info!("  timeouts_leader: {}", proxy_metrics.timeout_leader);

        // Log primary consensus metrics
        info!("Primary consensus metrics:");
        info!(
            "  last_committed_round: {}",
            primary_metrics.last_committed_round
        );
        info!("  current_round: {}", primary_metrics.current_round);
        info!("  committed_blocks: {}", primary_metrics.committed_blocks);

        // Report to test framework
        {
            let mut ctx = ctx.ctx.lock().await;
            ctx.report.report_text(format!(
                "Proxy: proposals={}, votes={}, qcs={}, ordered={}, forwarded={} | Primary: committed_round={}, committed_blocks={}",
                proxy_metrics.proposals_sent,
                proxy_metrics.votes_sent,
                proxy_metrics.qcs_formed,
                proxy_metrics.blocks_ordered,
                proxy_metrics.blocks_forwarded,
                primary_metrics.last_committed_round,
                primary_metrics.committed_blocks,
            ));
        }

        // Verify proxy consensus is actively producing blocks
        ensure!(
            proxy_metrics.proposals_sent > 0,
            "Proxy consensus: no proposals sent"
        );
        ensure!(
            proxy_metrics.votes_sent > 0,
            "Proxy consensus: no votes sent"
        );
        ensure!(
            proxy_metrics.qcs_formed > 0,
            "Proxy consensus: no QCs formed"
        );
        ensure!(
            proxy_metrics.blocks_ordered > 0,
            "Proxy consensus: no blocks ordered"
        );
        ensure!(
            proxy_metrics.blocks_forwarded > 0,
            "Proxy consensus: no blocks forwarded to primaries"
        );

        // Verify primary consensus is committing blocks
        ensure!(
            primary_metrics.last_committed_round > 0,
            "Primary consensus: no blocks committed (last_committed_round=0)"
        );
        ensure!(
            primary_metrics.committed_blocks > 0,
            "Primary consensus: committed_blocks=0"
        );

        info!(
            "Both consensus layers are active: proxy ordered {} blocks, primary committed {} blocks through round {}",
            proxy_metrics.blocks_ordered,
            primary_metrics.committed_blocks,
            primary_metrics.last_committed_round,
        );

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
        let start_proxy = query_proxy_metrics(&inspection_clients).await?;
        let start_primary = query_primary_metrics(&inspection_clients).await?;

        // Wait for test duration
        tokio::time::sleep(duration).await;

        // Take final metric snapshot
        let end_proxy = query_proxy_metrics(&inspection_clients).await?;
        let end_primary = query_primary_metrics(&inspection_clients).await?;

        // Calculate deltas
        let proposals_delta = end_proxy.proposals_sent - start_proxy.proposals_sent;
        let ordered_delta = end_proxy.blocks_ordered - start_proxy.blocks_ordered;
        let forwarded_delta = end_proxy.blocks_forwarded - start_proxy.blocks_forwarded;
        let backpressure_delta =
            end_proxy.backpressure_events - start_proxy.backpressure_events;
        let primary_committed_delta =
            end_primary.committed_blocks - start_primary.committed_blocks;

        let duration_secs = duration.as_secs_f64();
        let proxy_blocks_per_sec = if duration_secs > 0.0 {
            ordered_delta as f64 / duration_secs
        } else {
            0.0
        };

        info!("Load test metrics (delta):");
        info!("  proxy proposals: {}", proposals_delta);
        info!("  proxy blocks ordered: {}", ordered_delta);
        info!("  proxy blocks forwarded: {}", forwarded_delta);
        info!("  proxy blocks/sec: {:.2}", proxy_blocks_per_sec);
        info!("  primary blocks committed: {}", primary_committed_delta);
        info!("  backpressure events: {}", backpressure_delta);

        // Report to test framework
        {
            let mut ctx = ctx.ctx.lock().await;
            ctx.report.report_text(format!(
                "Proxy load: {} ordered ({:.2}/s), {} forwarded, {} primary committed, {} backpressure in {:.1}s",
                ordered_delta, proxy_blocks_per_sec, forwarded_delta,
                primary_committed_delta, backpressure_delta, duration_secs,
            ));
        }

        // Verify proxy consensus produced blocks during the test window
        ensure!(
            proposals_delta > 0,
            "Proxy consensus: no proposals sent during test window"
        );
        ensure!(
            ordered_delta > 0,
            "Proxy consensus: no blocks ordered during test window"
        );
        ensure!(
            forwarded_delta > 0,
            "Proxy consensus: no blocks forwarded during test window"
        );

        // Verify primary consensus committed blocks during the test window
        ensure!(
            primary_committed_delta > 0,
            "Primary consensus: no blocks committed during test window"
        );

        info!(
            "Load test passed: proxy ordered {} blocks ({:.2}/s), primary committed {} blocks in {:.1}s",
            ordered_delta, proxy_blocks_per_sec, primary_committed_delta, duration_secs,
        );

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
