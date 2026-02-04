// Copyright (c) Aptos Foundation
// Licensed pursuant to the Innovation-Enabling Source Code License, available at https://github.com/aptos-labs/aptos-core/blob/main/LICENSE

//! Simple round-robin leader election for proxy consensus.
//!
//! Unlike primary consensus which uses reputation-based leader election,
//! proxy consensus uses simple deterministic round-robin rotation among
//! the proxy validators.

use aptos_consensus_types::common::{Author, Round};

/// Simple round-robin leader election among proxy validators.
///
/// This is NOT reputation-based (unlike primary consensus) since:
/// - Proxy validators are co-located and have similar performance
/// - Simpler deterministic rotation is sufficient
/// - Lower overhead without reputation tracking
#[derive(Clone, Debug)]
pub struct ProxyLeaderElection {
    /// Ordered list of proxy validators
    proxy_validators: Vec<Author>,
    /// This node's author address
    author: Author,
}

impl ProxyLeaderElection {
    /// Create a new proxy leader election with the given validators.
    ///
    /// # Arguments
    /// * `proxy_validators` - Ordered list of proxy validator addresses
    /// * `author` - This node's author address
    pub fn new(proxy_validators: Vec<Author>, author: Author) -> Self {
        assert!(
            !proxy_validators.is_empty(),
            "Proxy validators list cannot be empty"
        );
        Self {
            proxy_validators,
            author,
        }
    }

    /// Get the leader for a given round.
    ///
    /// Uses simple modulo rotation: `leader = validators[round % n]`
    pub fn get_leader(&self, round: Round) -> Author {
        let index = (round as usize) % self.proxy_validators.len();
        self.proxy_validators[index]
    }

    /// Check if this node is the leader for the given round.
    pub fn is_leader(&self, round: Round) -> bool {
        self.get_leader(round) == self.author
    }

    /// Get the total number of proxy validators.
    pub fn num_validators(&self) -> usize {
        self.proxy_validators.len()
    }

    /// Get all proxy validators.
    pub fn validators(&self) -> &[Author] {
        &self.proxy_validators
    }

    /// Get this node's author address.
    pub fn author(&self) -> Author {
        self.author
    }

    /// Check if the given author is a proxy validator.
    pub fn is_proxy_validator(&self, author: &Author) -> bool {
        self.proxy_validators.contains(author)
    }

    /// Get the round number for which the given author will next be leader.
    /// Returns None if the author is not a proxy validator.
    pub fn next_leader_round(&self, author: &Author, from_round: Round) -> Option<Round> {
        let position = self.proxy_validators.iter().position(|a| a == author)?;
        let n = self.proxy_validators.len() as u64;
        let current_position = from_round % n;

        if position as u64 >= current_position {
            Some(from_round + (position as u64 - current_position))
        } else {
            Some(from_round + (n - current_position + position as u64))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_authors(n: usize) -> Vec<Author> {
        (0..n)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[0] = i as u8;
                Author::new(bytes)
            })
            .collect()
    }

    #[test]
    fn test_round_robin_rotation() {
        let validators = make_authors(3);
        let election = ProxyLeaderElection::new(validators.clone(), validators[0]);

        assert_eq!(election.get_leader(0), validators[0]);
        assert_eq!(election.get_leader(1), validators[1]);
        assert_eq!(election.get_leader(2), validators[2]);
        assert_eq!(election.get_leader(3), validators[0]); // Wraps around
        assert_eq!(election.get_leader(4), validators[1]);
        assert_eq!(election.get_leader(5), validators[2]);
    }

    #[test]
    fn test_is_leader() {
        let validators = make_authors(3);
        let election = ProxyLeaderElection::new(validators.clone(), validators[1]);

        assert!(!election.is_leader(0)); // validators[0] is leader
        assert!(election.is_leader(1)); // validators[1] is leader
        assert!(!election.is_leader(2)); // validators[2] is leader
        assert!(!election.is_leader(3)); // validators[0] is leader
        assert!(election.is_leader(4)); // validators[1] is leader
    }

    #[test]
    fn test_num_validators() {
        let validators = make_authors(5);
        let election = ProxyLeaderElection::new(validators.clone(), validators[0]);

        assert_eq!(election.num_validators(), 5);
    }

    #[test]
    fn test_is_proxy_validator() {
        let validators = make_authors(3);
        let election = ProxyLeaderElection::new(validators.clone(), validators[0]);

        assert!(election.is_proxy_validator(&validators[0]));
        assert!(election.is_proxy_validator(&validators[1]));
        assert!(election.is_proxy_validator(&validators[2]));

        let non_proxy = make_authors(4)[3];
        assert!(!election.is_proxy_validator(&non_proxy));
    }

    #[test]
    fn test_next_leader_round() {
        let validators = make_authors(3);
        let election = ProxyLeaderElection::new(validators.clone(), validators[0]);

        // From round 0, validators[0] is next leader at round 0
        assert_eq!(election.next_leader_round(&validators[0], 0), Some(0));
        // From round 0, validators[1] is next leader at round 1
        assert_eq!(election.next_leader_round(&validators[1], 0), Some(1));
        // From round 0, validators[2] is next leader at round 2
        assert_eq!(election.next_leader_round(&validators[2], 0), Some(2));

        // From round 2, validators[0] is next leader at round 3
        assert_eq!(election.next_leader_round(&validators[0], 2), Some(3));
        // From round 2, validators[2] is current leader
        assert_eq!(election.next_leader_round(&validators[2], 2), Some(2));
    }

    #[test]
    #[should_panic(expected = "Proxy validators list cannot be empty")]
    fn test_empty_validators_panic() {
        let author = make_authors(1)[0];
        ProxyLeaderElection::new(vec![], author);
    }
}
