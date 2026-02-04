// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Network sender for proxy consensus.
//!
//! ProxyNetworkSender wraps a generic network sending interface and provides:
//! - Broadcasting to proxy validators only (for proxy proposals and votes)
//! - Broadcasting to ALL primaries (for ordered proxy blocks)
//!
//! The actual network implementation is provided by the consensus crate through
//! the TProxyNetworkSender trait.

use aptos_consensus_types::{
    common::Author,
    proxy_messages::{
        OptProxyProposalMsg, OrderedProxyBlocksMsg, ProxyOrderVoteMsg, ProxyProposalMsg,
        ProxyVoteMsg,
    },
    proxy_sync_info::ProxySyncInfo,
};
use aptos_logger::prelude::*;
use aptos_types::account_address::AccountAddress;
use std::sync::Arc;

use crate::proxy_metrics;

/// Trait for sending proxy consensus messages.
///
/// This abstraction allows the proxy_primary crate to send network messages
/// without depending on the full consensus crate. The actual implementation
/// is provided by the consensus crate when initializing proxy consensus.
#[async_trait::async_trait]
pub trait TProxyNetworkSender: Send + Sync {
    /// Broadcast a proxy proposal to all proxy validators.
    async fn broadcast_proxy_proposal(&self, proposal_msg: ProxyProposalMsg);

    /// Broadcast an optimistic proxy proposal to all proxy validators.
    async fn broadcast_opt_proxy_proposal(&self, proposal_msg: OptProxyProposalMsg);

    /// Broadcast a proxy vote to all proxy validators.
    async fn broadcast_proxy_vote(&self, vote_msg: ProxyVoteMsg);

    /// Broadcast a proxy order vote to all proxy validators.
    async fn broadcast_proxy_order_vote(&self, order_vote_msg: ProxyOrderVoteMsg);

    /// Broadcast ordered proxy blocks to ALL primaries (not just proxies).
    /// This is the critical message that transfers proxy consensus results to primary consensus.
    async fn broadcast_ordered_proxy_blocks(&self, ordered_msg: OrderedProxyBlocksMsg);

    /// Broadcast proxy sync info to all proxy validators.
    async fn broadcast_proxy_sync_info(&self, sync_info: ProxySyncInfo);

    /// Send proxy vote to specific recipients (e.g., next proxy leader).
    async fn send_proxy_vote(&self, vote_msg: ProxyVoteMsg, recipients: Vec<Author>);

    /// Get our author address.
    fn author(&self) -> Author;

    /// Get list of proxy validators.
    fn proxy_validators(&self) -> &[AccountAddress];

    /// Get list of all primary validators.
    fn primary_validators(&self) -> &[AccountAddress];

    /// Check if we are a proxy validator.
    fn is_proxy_validator(&self) -> bool {
        self.proxy_validators().contains(&self.author())
    }
}

/// Wrapper around a TProxyNetworkSender that adds metrics and logging.
pub struct ProxyNetworkSender {
    inner: Arc<dyn TProxyNetworkSender>,
}

impl ProxyNetworkSender {
    /// Create a new proxy network sender.
    pub fn new(inner: Arc<dyn TProxyNetworkSender>) -> Self {
        Self { inner }
    }

    /// Broadcast a proxy proposal to all proxy validators.
    pub async fn broadcast_proxy_proposal(&self, proposal_msg: ProxyProposalMsg) {
        debug!(
            "Broadcasting proxy proposal: round {}, author {}",
            proposal_msg.proposal().round(),
            proposal_msg.proposer()
        );
        proxy_metrics::PROXY_CONSENSUS_PROPOSALS_SENT.inc();
        self.inner.broadcast_proxy_proposal(proposal_msg).await;
    }

    /// Broadcast an optimistic proxy proposal to all proxy validators.
    pub async fn broadcast_opt_proxy_proposal(&self, proposal_msg: OptProxyProposalMsg) {
        debug!(
            "Broadcasting opt proxy proposal: round {}, author {}",
            proposal_msg.block_data().round(),
            proposal_msg.proposer()
        );
        proxy_metrics::PROXY_CONSENSUS_PROPOSALS_SENT.inc();
        self.inner.broadcast_opt_proxy_proposal(proposal_msg).await;
    }

    /// Broadcast a proxy vote to all proxy validators.
    pub async fn broadcast_proxy_vote(&self, vote_msg: ProxyVoteMsg) {
        debug!(
            "Broadcasting proxy vote: round {}, voter {}",
            vote_msg.vote().vote_data().proposed().round(),
            vote_msg.vote().author()
        );
        proxy_metrics::PROXY_CONSENSUS_VOTES_SENT.inc();
        self.inner.broadcast_proxy_vote(vote_msg).await;
    }

    /// Send proxy vote to specific recipients.
    pub async fn send_proxy_vote(&self, vote_msg: ProxyVoteMsg, recipients: Vec<Author>) {
        debug!(
            "Sending proxy vote to {} recipients: round {}, voter {}",
            recipients.len(),
            vote_msg.vote().vote_data().proposed().round(),
            vote_msg.vote().author()
        );
        proxy_metrics::PROXY_CONSENSUS_VOTES_SENT.inc();
        self.inner.send_proxy_vote(vote_msg, recipients).await;
    }

    /// Broadcast a proxy order vote to all proxy validators.
    pub async fn broadcast_proxy_order_vote(&self, order_vote_msg: ProxyOrderVoteMsg) {
        debug!(
            "Broadcasting proxy order vote: round {}",
            order_vote_msg.quorum_cert().certified_block().round()
        );
        proxy_metrics::PROXY_CONSENSUS_ORDER_VOTES_SENT.inc();
        self.inner.broadcast_proxy_order_vote(order_vote_msg).await;
    }

    /// Broadcast ordered proxy blocks to ALL primaries.
    ///
    /// This is the key message that transfers proxy consensus results to primary consensus.
    /// All primaries will receive this message and use it to construct their primary blocks.
    pub async fn broadcast_ordered_proxy_blocks(&self, ordered_msg: OrderedProxyBlocksMsg) {
        let num_blocks = ordered_msg.proxy_blocks().len();
        let primary_round = ordered_msg.primary_round();

        info!(
            "Broadcasting {} ordered proxy blocks for primary round {}",
            num_blocks, primary_round
        );

        proxy_metrics::PROXY_CONSENSUS_BLOCKS_FORWARDED.inc_by(num_blocks as u64);
        proxy_metrics::PROXY_BLOCKS_PER_PRIMARY_ROUND.observe(num_blocks as f64);

        self.inner.broadcast_ordered_proxy_blocks(ordered_msg).await;
    }

    /// Broadcast proxy sync info to all proxy validators.
    pub async fn broadcast_proxy_sync_info(&self, sync_info: ProxySyncInfo) {
        debug!(
            "Broadcasting proxy sync info: proxy_qc_round {}, primary_qc_round {}",
            sync_info.highest_proxy_certified_round(),
            sync_info.highest_primary_certified_round()
        );
        self.inner.broadcast_proxy_sync_info(sync_info).await;
    }

    /// Get our author address.
    pub fn author(&self) -> Author {
        self.inner.author()
    }

    /// Get list of proxy validators.
    pub fn proxy_validators(&self) -> &[AccountAddress] {
        self.inner.proxy_validators()
    }

    /// Get list of all primary validators.
    pub fn primary_validators(&self) -> &[AccountAddress] {
        self.inner.primary_validators()
    }

    /// Check if we are a proxy validator.
    pub fn is_proxy_validator(&self) -> bool {
        self.inner.is_proxy_validator()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockNetworkSender {
        author: Author,
        proxy_validators: Vec<AccountAddress>,
        primary_validators: Vec<AccountAddress>,
        proposal_count: AtomicUsize,
        vote_count: AtomicUsize,
    }

    impl MockNetworkSender {
        fn new(author: Author) -> Self {
            let proxy_validators = vec![author, AccountAddress::random(), AccountAddress::random()];
            let primary_validators = vec![
                author,
                AccountAddress::random(),
                AccountAddress::random(),
                AccountAddress::random(),
                AccountAddress::random(),
            ];
            Self {
                author,
                proxy_validators,
                primary_validators,
                proposal_count: AtomicUsize::new(0),
                vote_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl TProxyNetworkSender for MockNetworkSender {
        async fn broadcast_proxy_proposal(&self, _proposal_msg: ProxyProposalMsg) {
            self.proposal_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn broadcast_opt_proxy_proposal(&self, _proposal_msg: OptProxyProposalMsg) {
            self.proposal_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn broadcast_proxy_vote(&self, _vote_msg: ProxyVoteMsg) {
            self.vote_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn broadcast_proxy_order_vote(&self, _order_vote_msg: ProxyOrderVoteMsg) {}

        async fn broadcast_ordered_proxy_blocks(&self, _ordered_msg: OrderedProxyBlocksMsg) {}

        async fn broadcast_proxy_sync_info(&self, _sync_info: ProxySyncInfo) {}

        async fn send_proxy_vote(&self, _vote_msg: ProxyVoteMsg, _recipients: Vec<Author>) {
            self.vote_count.fetch_add(1, Ordering::SeqCst);
        }

        fn author(&self) -> Author {
            self.author
        }

        fn proxy_validators(&self) -> &[AccountAddress] {
            &self.proxy_validators
        }

        fn primary_validators(&self) -> &[AccountAddress] {
            &self.primary_validators
        }
    }

    #[test]
    fn test_proxy_network_sender_creation() {
        let author = AccountAddress::random();
        let mock = Arc::new(MockNetworkSender::new(author));
        let sender = ProxyNetworkSender::new(mock);

        assert_eq!(sender.author(), author);
        assert_eq!(sender.proxy_validators().len(), 3);
        assert_eq!(sender.primary_validators().len(), 5);
        assert!(sender.is_proxy_validator());
    }
}
