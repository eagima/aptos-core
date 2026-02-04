# Proxy Primary Implementation Details

This document contains concrete Rust data structures, message types, interfaces, and implementation guidance for the proxy primary consensus protocol.

---

## Proxy Block Data Structure

Proxy blocks support **optimistic proposals** (1 message delay block time), following the existing `OptBlockData` pattern.

**Optimistic Proxy Block** (primary block type, 1 message delay):
```rust
/// Matches existing OptBlockData structure with proxy-specific fields
pub struct OptProxyBlockData {
    pub epoch: u64,
    pub round: Round,                    // Proxy round
    pub timestamp_usecs: u64,
    pub parent: BlockInfo,               // Parent proxy block info (NOT parent QC)
    pub block_body: OptProxyBlockBody,
}

pub enum OptProxyBlockBody {
    V0 {
        // Block content (includes validator_txns for aggregated DKG/JWK)
        validator_txns: Vec<ValidatorTransaction>,  // Aggregated DKG/JWK submissions
        payload: Payload,
        author: Author,
        grandparent_qc: QuorumCert,      // QC on grandparent proxy block (round - 2)

        // Primary consensus linkage (NEW fields for proxy)
        primary_round: Round,
        primary_qc: Option<QuorumCert>,  // Attached if QC.round == primary_round - 1
    },
}
```

**Verification rules** (matching existing OptBlockData):
```rust
fn verify_well_formed(&self) -> Result<()> {
    // Same as existing OptBlockData
    ensure!(grandparent_qc.round() + 1 == parent.round());
    ensure!(parent.round() + 1 == self.round());
    ensure!(same epoch for all);
    ensure!(!grandparent_qc.has_reconfiguration());
    ensure!(strictly increasing timestamps);
    ensure!(not too far in future - 5 min);

    // NEW: primary_round validation
    if parent.has_primary_qc() {
        ensure!(primary_round == parent.primary_round + 1);
    } else {
        ensure!(primary_round == parent.primary_round);
    }

    // NEW: primary_qc validation
    if let Some(qc) = primary_qc {
        ensure!(qc.round() == primary_round - 1);
    }
}
```

**Conversion:** `OptProxyBlockData` + parent proxy QC → regular `ProxyBlock` (in BlockType enum)

**Timing (same as AptosBFT):**
- Proxy block time: 1 message delay (optimistic proposals)
- Proxy block ordering: 3 message delays (order votes)

**Key differences from existing OptBlockData:**
- `validator_txns` limited to **aggregated DKG transcripts and JWK updates** (these have aggregated signatures and can be submitted by any validator)
- NEW `primary_round` field to track which primary block this belongs to
- NEW `primary_qc` optional field for attaching primary QC

---

## Proxy Block Type

Extend existing `BlockType` enum:

```rust
pub enum BlockType {
    // ... existing variants ...

    /// Proxy block for proxy primary consensus
    ProxyBlock {
        validator_txns: Vec<ValidatorTransaction>,  // Aggregated DKG/JWK
        payload: Payload,
        author: Author,
        failed_authors: Vec<(Round, Author)>,  // Proxy leaders who failed/timed out (same concept as primary)
        primary_round: Round,
        primary_qc: Option<QuorumCert>,
        quorum_cert: QuorumCert,           // QC on parent proxy block
        order_cert: Option<OrderCert>,     // Present when block is ordered
    },
}
```

---

## Message Types

Based on existing consensus messages, proxy consensus needs these message types:

**1. ProxyProposalMsg** (like ProposalMsg):
```rust
pub struct ProxyProposalMsg {
    proposal: ProxyBlock,            // Proxy block with parent QC
    sync_info: ProxySyncInfo,
}
```

**2. OptProxyProposalMsg** (like OptProposalMsg):
```rust
pub struct OptProxyProposalMsg {
    block_data: OptProxyBlockData,   // Optimistic proxy block (grandparent QC)
    sync_info: ProxySyncInfo,
}
```

**3. ProxyVoteMsg** (like VoteMsg):
```rust
pub struct ProxyVoteMsg {
    vote: Vote,                      // Reuse existing Vote type
    sync_info: ProxySyncInfo,
}
```

**4. ProxyOrderVoteMsg** (like OrderVoteMsg):
```rust
pub struct ProxyOrderVoteMsg {
    order_vote: OrderVote,           // Reuse existing OrderVote type
    quorum_cert: QuorumCert,         // QC on the block being ordered
}
```

**5. ProxyRoundTimeoutMsg** (like RoundTimeoutMsg):
```rust
pub struct ProxyRoundTimeoutMsg {
    timeout: RoundTimeout,           // Reuse existing RoundTimeout type
    sync_info: ProxySyncInfo,
}
```

**6. ProxySyncInfo** (like SyncInfo, tracks both proxy and primary):
```rust
pub struct ProxySyncInfo {
    // Proxy consensus state (NO commit cert - proxy doesn't execute/commit)
    highest_proxy_qc: QuorumCert,
    highest_proxy_ordered_cert: Option<WrappedLedgerInfo>,  // Uses LedgerInfo with default execution state
    highest_proxy_timeout_cert: Option<TwoChainTimeoutCertificate>,

    // Primary consensus state (received from primaries via internal channel)
    highest_primary_qc: QuorumCert,
    highest_primary_tc: Option<TwoChainTimeoutCertificate>,
}
```

**Note:** Proxy order certificate uses existing **LedgerInfo** structure but with **default execution/commit state values** (serves as ordering proof without execution results).

**7. OrderedProxyBlocksMsg** (NEW - forward to primaries):
```rust
pub struct OrderedProxyBlocksMsg {
    proxy_blocks: Vec<ProxyBlock>,   // Ordered proxy blocks for one primary round (FULL blocks)
    primary_round: Round,
    primary_qc: QuorumCert,          // QC that cut the proxy blocks
}
```

**Note:** Contains **full proxy blocks** (with optimistic batch hashes). Actual transaction data is in QuorumStore - primaries fetch/verify separately. Every proxy node broadcasts this message for reliability (primaries may receive multiple copies).

**8. ProxyBlockRetrievalRequest** (like BlockRetrievalRequest):
```rust
pub struct ProxyBlockRetrievalRequest {
    block_id: HashValue,             // Starting block ID
    num_blocks: u64,                 // Number of blocks to fetch
    target_round: Round,             // Target round (for V2 style)
}
```

**9. ProxyBlockRetrievalResponse** (like BlockRetrievalResponse):
```rust
pub struct ProxyBlockRetrievalResponse {
    status: BlockRetrievalStatus,
    blocks: Vec<ProxyBlock>,
}
```

**Message Summary:**

| Existing Message | Proxy Equivalent | Notes |
|------------------|------------------|-------|
| ProposalMsg | ProxyProposalMsg | With ProxyBlock |
| OptProposalMsg | OptProxyProposalMsg | With OptProxyBlockData |
| VoteMsg | ProxyVoteMsg | Reuses Vote type |
| OrderVoteMsg | ProxyOrderVoteMsg | Reuses OrderVote type |
| RoundTimeoutMsg | ProxyRoundTimeoutMsg | Reuses RoundTimeout type |
| SyncInfo | ProxySyncInfo | Tracks both proxy + primary state |
| BlockRetrievalRequest | ProxyBlockRetrievalRequest | Fetch missing proxy blocks |
| BlockRetrievalResponse | ProxyBlockRetrievalResponse | Return proxy blocks |
| (none) | OrderedProxyBlocksMsg | NEW: forward to primaries |

---

## Key Interfaces

**ProxyBlockReader** - Extends BlockReader for proxy blocks:
```rust
pub trait ProxyBlockReader: Send + Sync {
    fn get_proxy_block(&self, block_id: HashValue) -> Option<Arc<PipelinedBlock>>;
    fn highest_proxy_qc(&self) -> Arc<QuorumCert>;
    fn highest_primary_qc(&self) -> Arc<QuorumCert>;
    fn highest_primary_tc(&self) -> Option<Arc<TwoChainTimeoutCertificate>>;
    fn current_primary_round(&self) -> Round;
    fn proxy_sync_info(&self) -> ProxySyncInfo;

    // Get ordered proxy blocks for a primary round
    fn get_ordered_proxy_blocks(&self, primary_round: Round) -> Vec<Arc<PipelinedBlock>>;
}
```

**ProxySafetyRules** - Proxy-specific safety:
```rust
pub trait ProxySafetyRules: Send + Sync {
    /// Sign proxy vote (uses SEPARATE highest_voted_round from primary)
    fn construct_and_sign_proxy_vote(
        &mut self,
        vote_proposal: &VoteProposal,
    ) -> Result<Vote, Error>;

    /// Sign proxy order vote
    fn construct_and_sign_proxy_order_vote(
        &mut self,
        order_vote_proposal: &OrderVoteProposal,
    ) -> Result<OrderVote, Error>;

    /// Check if should shutdown due to primary TC
    fn should_shutdown(&self, primary_tc: &TwoChainTimeoutCertificate) -> bool;
}
```

**Note:** Same key pair as primary, but **completely separate persistent storage** for voting state. This prevents cross-layer interference and allows independent safety tracking for proxy vs primary consensus.

---

## ProxyRoundManager

Key methods extending RoundManager pattern:

```rust
impl ProxyRoundManager {
    /// Process incoming proxy proposal
    pub async fn process_proxy_proposal_msg(&mut self, msg: ProxyProposalMsg) -> Result<()> {
        // Validate primary_round consistency
        // Validate and process attached primary_qc if present
        // Insert block, vote if valid
    }

    /// Process primary QC received from primary consensus
    pub async fn process_primary_qc(&mut self, qc: QuorumCert) -> Result<()> {
        // Update QC_primary state
        // May trigger attaching QC to next proxy block
    }

    /// Process primary TC - may trigger shutdown
    pub async fn process_primary_tc(&mut self, tc: TwoChainTimeoutCertificate) -> Result<()> {
        // Check if tc.round >= primary_round
        // If yes, shutdown proxy consensus
    }

    /// Enter new proxy round (leader)
    async fn enter_new_proxy_round(&mut self, event: NewRoundEvent) -> Result<()> {
        // Determine primary_round based on parent
        // Attach primary_qc if available
        // Generate and broadcast proxy proposal
    }

    /// Process ordered proxy block
    async fn process_ordered_proxy_block(&mut self, block: Arc<PipelinedBlock>) -> Result<()> {
        // If block has primary_qc, forward ordered blocks to primaries
    }
}
```

---

## Primary Consensus Extensions

**PrimaryBlockFromProxy** - Aggregated primary block:
```rust
pub struct PrimaryBlockFromProxy {
    proxy_blocks: Vec<Arc<PipelinedBlock>>,  // Ordered proxy blocks (with full content)
    primary_round: Round,
    primary_qc: QuorumCert,                   // QC that cut the proxy blocks
}

impl PrimaryBlockFromProxy {
    /// Verify proxy block chain integrity
    fn verify(&self) -> Result<()> {
        // All linked by hashes
        // All ordered (have order certs)
        // All have valid proxy block signatures
        // Last has primary_qc
        // All have same primary_round
        // First extends parent correctly
        // Conflicting messages for same primary_round → evidence of Byzantine behavior → trigger fallback
    }

    /// Aggregate into primary block
    /// NOTE: All primaries independently form the SAME block (deterministic aggregation)
    fn to_primary_block(&self) -> Block {
        // Combine user transactions from all proxy blocks
        // Combine validator txns from all proxy blocks (aggregated DKG/JWK)
        // Add randomness, decryption (during primary consensus)
    }
}
```

**Key insight:** Primary block formation is **independent** - all primaries receiving the same OrderedProxyBlocksMsg will form the identical primary block. No leader proposal needed for this step.

---

## Network Layer

**ProxyNetworkSender** - Wrapper that sends only to proxy validators:
```rust
pub struct ProxyNetworkSender {
    inner: NetworkSender,
    proxy_validators: Vec<AccountAddress>,  // From EpochState
}

impl ProxyNetworkSender {
    pub fn new(inner: NetworkSender, epoch_state: &EpochState) -> Self {
        let proxy_validators = epoch_state.proxy_validators().to_vec();
        Self { inner, proxy_validators }
    }

    pub async fn broadcast_proxy_proposal(&self, msg: ProxyProposalMsg) {
        let consensus_msg = ConsensusMsg::ProxyProposalMsg(Box::new(msg));
        self.inner.send(consensus_msg, self.proxy_validators.clone()).await
    }

    pub async fn broadcast_proxy_vote(&self, msg: ProxyVoteMsg) {
        let consensus_msg = ConsensusMsg::ProxyVoteMsg(Box::new(msg));
        self.inner.send(consensus_msg, self.proxy_validators.clone()).await
    }

    // ... similar methods for other proxy message types

    /// Broadcast ordered proxy blocks to ALL primaries (not just proxies)
    pub async fn broadcast_ordered_proxy_blocks(&self, msg: OrderedProxyBlocksMsg) {
        let consensus_msg = ConsensusMsg::OrderedProxyBlocksMsg(Box::new(msg));
        self.inner.broadcast(consensus_msg).await  // Broadcast to all validators
    }
}
```

**ConsensusMsg extensions** (in network_interface.rs):
```rust
pub enum ConsensusMsg {
    // ... existing variants ...

    // Proxy consensus messages
    ProxyProposalMsg(Box<ProxyProposalMsg>),
    OptProxyProposalMsg(Box<OptProxyProposalMsg>),
    ProxyVoteMsg(Box<ProxyVoteMsg>),
    ProxyOrderVoteMsg(Box<ProxyOrderVoteMsg>),
    ProxyRoundTimeoutMsg(Box<ProxyRoundTimeoutMsg>),
    OrderedProxyBlocksMsg(Box<OrderedProxyBlocksMsg>),
    ProxyBlockRetrievalRequest(Box<ProxyBlockRetrievalRequest>),
    ProxyBlockRetrievalResponse(Box<ProxyBlockRetrievalResponse>),
}
```

---

## Metrics

Separate namespace for proxy consensus metrics:
```rust
// Core consensus metrics
pub static PROXY_CONSENSUS_PROPOSALS_SENT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("aptos_proxy_consensus_proposals_sent", "...").unwrap()
});

pub static PROXY_CONSENSUS_VOTES_SENT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("aptos_proxy_consensus_votes_sent", "...").unwrap()
});

pub static PROXY_CONSENSUS_BLOCKS_ORDERED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("aptos_proxy_consensus_blocks_ordered", "...").unwrap()
});

pub static PROXY_CONSENSUS_PRIMARY_QC_LATENCY: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!("aptos_proxy_consensus_primary_qc_latency_seconds", "...").unwrap()
});

pub static PROXY_CONSENSUS_SHUTDOWN_COUNT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("aptos_proxy_consensus_shutdown_count", "...").unwrap()
});

// Proxy-specific metrics
pub static PROXY_BLOCKS_PER_PRIMARY_ROUND: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!("aptos_proxy_blocks_per_primary_round", "...").unwrap()
});

pub static PROXY_STATE_TRANSITIONS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "aptos_proxy_state_transitions",
        "...",
        &["from_state", "to_state"]
    ).unwrap()
});

pub static PROXY_TRIAL_SUCCESS_COUNT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("aptos_proxy_trial_success_count", "...").unwrap()
});

pub static PROXY_TRIAL_FAILURE_COUNT: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("aptos_proxy_trial_failure_count", "...").unwrap()
});

pub static PROXY_BACKPRESSURE_EVENTS: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("aptos_proxy_backpressure_events", "...").unwrap()
});

pub static PROXY_BLOCK_ORDERING_LATENCY: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!("aptos_proxy_block_ordering_latency_seconds", "...").unwrap()
});

pub static PROXY_READY_SIGNATURES_COLLECTED: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!("aptos_proxy_ready_signatures_collected", "...").unwrap()
});
```

---

## Expected Performance

- **10+ proxy blocks per primary block** in happy path
- Proxy consensus runs much faster due to co-location
- Block time: 1 message delay (optimistic proposals)
- Block ordering: 3 message delays (order votes)

---

## Testing Strategy

1. **Unit tests first**, then integration tests
2. Test individual components:
   - ProxyBlockStore
   - ProxyRoundManager
   - ProxySafetyRules
   - ProxySyncInfo
3. Then E2E proxy-primary flow tests

---

## File Structure

```
consensus/proxy_primary/
├── design/
│   ├── architecture.md
│   ├── protocol.md
│   └── implementation.md
└── src/
    ├── mod.rs
    ├── proxy_block.rs               # OptProxyBlockData, BlockType::ProxyBlock
    ├── proxy_block_store.rs         # ProxyBlockStore, ProxyBlockTree
    ├── proxy_coordinator.rs         # ProxyCoordinator (state machine)
    ├── proxy_round_manager.rs       # ProxyRoundManager (event loop)
    ├── proxy_safety_rules.rs        # ProxySafetyRules impl
    ├── proxy_proposal_generator.rs  # Proposal generation with QS
    ├── proxy_sync_info.rs           # ProxySyncInfo
    ├── proxy_network_sender.rs      # ProxyNetworkSender wrapper
    ├── proxy_block_retriever.rs     # ProxyBlockRetriever
    ├── proxy_leader_election.rs     # Simple round-robin
    ├── proxy_health_monitor.rs      # ProxyHealthMonitor
    ├── proxy_metrics.rs             # Metrics
    ├── proxy_error.rs               # ProxyConsensusError
    └── primary_integration.rs       # PrimaryBlockFromProxy
```

---

## ProxyBlockStore

```rust
pub struct ProxyBlockStore {
    inner: Arc<RwLock<ProxyBlockTree>>,
    genesis_primary_block_id: HashValue,  // Epoch start primary block
}

struct ProxyBlockTree {
    // Primary index
    id_to_block: HashMap<HashValue, Arc<PipelinedBlock>>,

    // Secondary indices
    round_to_id: BTreeMap<Round, HashValue>,
    ordered_by_primary_round: HashMap<Round, Vec<HashValue>>,

    // Certificates
    highest_proxy_qc: Arc<QuorumCert>,
    highest_proxy_ordered_cert: Option<WrappedLedgerInfo>,
    highest_proxy_timeout_cert: Option<Arc<TwoChainTimeoutCertificate>>,

    // Primary consensus state (from internal channel)
    highest_primary_qc: Arc<QuorumCert>,
    highest_primary_tc: Option<Arc<TwoChainTimeoutCertificate>>,
}

impl ProxyBlockStore {
    /// Genesis is ALWAYS epoch start primary block (never changes after state sync)
    pub fn new(genesis_primary_block_id: HashValue) -> Self { ... }

    /// Insert block, update indices
    pub fn insert_block(&self, block: Arc<PipelinedBlock>) -> Result<()> { ... }

    /// Get ordered blocks for a primary round
    pub fn get_ordered_blocks(&self, primary_round: Round) -> Vec<Arc<PipelinedBlock>> { ... }

    /// Count blocks for backpressure check
    pub fn count_blocks_for_primary_round(&self, primary_round: Round) -> u64 { ... }

    /// Clear all blocks (on shutdown, txns return to mempool naturally)
    pub fn clear(&self) { ... }
}
```

**Note:** In-memory only (no persistence). Safety handled by ProxySafetyRules (separate persistent storage). Recovery via peer fetch is fast (proxies are co-located).

---

## State Machine / Lifecycle

**ProxyCoordinator** manages proxy state on ALL validators:

```
States: Stopped ←→ Trial ←→ Active

State Transitions:
┌─────────────────────────────────────────────────────────────────┐
│                                                                 │
│   Stopped ──────────────────────────────────────────> Active    │
│      │         (epoch change with ProxyReady txn)               │
│      │                                                          │
│      │         (TC: recovery after cooldown)                    │
│      └──────────────────────────> Trial ─────────────> Active   │
│                                     │    (success)      │       │
│                                     │                   │       │
│                                     │    (TC failure)   │       │
│                                     └───────────────────┘       │
│                                                   │              │
│   Active <────────────────────────────────────────┘              │
│      │              (TC.round >= primary_round)                 │
│      └──────────────────────────> Stopped                       │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘

Enable: ProxyReady validator txn → epoch change → Active (no Trial)
Disable: TC.round >= primary_round → Stopped (no epoch change)
Recovery: After cooldown → Trial → Active (if successful)
```

**ProxyReady Validator Transaction:**
```rust
struct ProxyReadyMessage {
    target_epoch: u64,
}

struct ProxyConsensusReady {
    target_epoch: u64,
    aggregate_signature: AggregateSignature,  // 2f+1 proxy signatures
}
```

- Proxies sign ProxyReadyMessage via reliable broadcast
- When 2f+1 signatures collected → ProxyConsensusReady txn submitted
- Epoch change includes ProxyReady → next epoch starts in Active state

**Trial Mode:**
- ProxyRoundManager runs normally, produces proxy blocks
- Broadcasts OrderedProxyBlocksMsg so all primaries can verify
- Primary leader proposes directly (doesn't use proxy blocks)
- Trial success: N consecutive blocks ordered within latency threshold
- Trial failure: TC during trial → back to Stopped

**Components by Validator Type:**

| Component | Proxy Validator | Non-Proxy Primary |
|-----------|-----------------|-------------------|
| ProxyCoordinator | Yes | Yes |
| ProxyRoundManager | Yes | No |
| ProxyBlockStore | Yes | No |
| ProxySafetyRules | Yes | No |
| ProxyHealthMonitor | Yes | Yes |

---

## Concurrency Model

**Channel Types:**
```rust
// Primary → Proxy communication
enum PrimaryToProxyEvent {
    NewQC(Arc<QuorumCert>),
    NewTC(Arc<TwoChainTimeoutCertificate>),
    Shutdown,
    BackpressureOn,
    BackpressureOff,
}

// Proxy → Primary communication
enum ProxyToPrimaryEvent {
    StateChanged(ProxyState),
    OrderedProxyBlocks(OrderedProxyBlocksMsg),
    ProxyReadyTxn(ProxyConsensusReady),
}

// Channel setup
type PrimaryToProxyTx = mpsc::UnboundedSender<PrimaryToProxyEvent>;
type PrimaryToProxyRx = mpsc::UnboundedReceiver<PrimaryToProxyEvent>;
type ProxyToPrimaryTx = mpsc::UnboundedSender<ProxyToPrimaryEvent>;
type ProxyToPrimaryRx = mpsc::UnboundedReceiver<ProxyToPrimaryEvent>;
```

**Event Loop (ProxyRoundManager):**
```rust
loop {
    tokio::select! {
        biased;

        // Highest priority: shutdown and primary updates
        event = primary_rx.recv() => {
            match event {
                PrimaryToProxyEvent::Shutdown => break,
                PrimaryToProxyEvent::NewQC(qc) => self.process_primary_qc(qc).await,
                PrimaryToProxyEvent::NewTC(tc) => self.process_primary_tc(tc).await,
                PrimaryToProxyEvent::BackpressureOn => self.pause_proposing(),
                PrimaryToProxyEvent::BackpressureOff => self.resume_proposing(),
            }
        }

        // Network messages
        msg = network_rx.recv() => {
            self.process_proxy_message(msg).await;
        }

        // Round timeout
        _ = round_timeout.tick() => {
            self.process_round_timeout().await;
        }
    }
}
```

**Network Routing:**
- ProxyCoordinator receives ALL consensus messages
- Routes proxy messages to ProxyRoundManager (if active)
- Routes primary messages to primary RoundManager
- Feature-gated for backward compatibility

---

## On-Chain Configuration

**Extend OnChainConsensusConfig (backward compatible):**
```rust
// In types/src/on_chain_config/consensus_config.rs

pub enum OnChainConsensusConfig {
    // existing V1-V5...
    V6 {
        alg: ConsensusAlgorithmConfig,
        vtxn: ValidatorTxnConfig,
        window_size: Option<u64>,
        rand_check_enabled: bool,
        proxy_config: Option<ProxyConsensusConfig>,  // NEW
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProxyConsensusConfig {
    pub proxy_validators: Vec<ProxyValidatorInfo>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProxyValidatorInfo {
    pub address: AccountAddress,
    pub voting_power: u64,  // Custom voting power for proxy consensus
}

impl OnChainConsensusConfig {
    pub fn proxy_config(&self) -> Option<&ProxyConsensusConfig> {
        match self {
            Self::V6 { proxy_config, .. } => proxy_config.as_ref(),
            _ => None,
        }
    }

    pub fn is_proxy_enabled(&self) -> bool {
        self.proxy_config().map_or(false, |c| !c.proxy_validators.is_empty())
    }
}
```

**Runtime Config (not on-chain):**
```rust
// In config/src/config/consensus_config.rs
pub struct ProxyRuntimeConfig {
    pub restart_cooldown_rounds: u64,           // default: 10
    pub trial_blocks_required: u64,             // default: 10
    pub max_ordering_latency_ms: u64,           // default: 500
    pub proxy_block_timeout_ms: u64,            // default: 500
    pub warmup_rounds: u64,                     // default: 3-5
    pub max_proxy_blocks_per_primary_round: u64, // default: 10
}
```

**Backward Compatibility:**
- V1-V5: No proxy support, continue as normal
- V6 with proxy_config: None → proxy disabled
- V6 with proxy_config: Some → proxy enabled
- Governance proposal upgrades V5 → V6

---

## Initialization Sequence

**At Epoch Start:**
```
1. Check ProxyConsensusConfig from EpochState
2. If proxy_validators is non-empty AND current validator is proxy:
   a. Create ProxyBlockStore with genesis = epoch start primary block
   b. Create ProxySafetyRules with fresh SafetyData (epoch, round=0)
   c. Create channels (primary_rx, primary_tx)
   d. Spawn ProxyRoundManager tokio task
   e. Enter Warmup phase (warmup_rounds)

3. Warmup Phase:
   - ProxyRoundManager runs but blocks NOT used by primary
   - Primary leader proposes directly during warmup
   - After warmup_rounds: transition to Active

4. Primary RoundManager sends QC/TC updates via primary_tx
```

**Note:** Warmup prevents race condition at epoch start where proxy might not be ready.

---

## Shutdown Sequence

**Triggered by TC.round >= primary_round OR epoch end:**
```
1. ProxyCoordinator receives TC from primary RoundManager
2. If TC.round >= current primary_round:
   a. Send Shutdown via primary_tx channel
   b. Transition to Stopped state

3. ProxyRoundManager receives Shutdown:
   a. Stop accepting new proxy messages
   b. Abort pending futures
   c. Clear ProxyBlockStore (blocks discarded, txns return to mempool naturally)
   d. Exit event loop
   e. Task terminates

4. On epoch end:
   a. Same shutdown sequence
   b. All proxy state cleared
   c. New epoch may or may not have proxy enabled
```

**Note:** Uncommitted transactions follow same path as transactions cut by block gas limit during execution - they return to mempool naturally.

---

## Error Types

```rust
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

    #[error(transparent)]
    SafetyRules(#[from] SafetyRulesError),

    #[error(transparent)]
    Network(#[from] NetworkError),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

---

## Invariants

```
Safety Invariants:
1. primary_round = max(QC_primary.round, TC_primary.round) + 1           [ALWAYS]
2. proxy_block.primary_qc.round == proxy_block.primary_round - 1 (if present)  [QC_ATTACHMENT]
3. ProxySafetyRules.highest_voted_round independent of primary SafetyRules    [ISOLATION]
4. Never vote for proxy round <= highest_voted_round                          [SAFETY]

Liveness Invariants:
5. Shutdown triggered when TC.round >= primary_round                          [LIVENESS]
6. Only one ProxyRoundManager active per epoch per validator                  [SINGLETON]

Ordering Invariants:
7. All proxy blocks with same primary_round form a chain                      [ORDERING]
8. First proxy block of primary_round N extends last of primary_round N-1     [CONTINUITY]
9. Genesis is always epoch start primary block                                [GENESIS]

Backpressure Invariants:
10. proxy_blocks_count(primary_round) <= max_proxy_blocks_per_primary_round   [BACKPRESSURE]
```

---

## Backpressure

**Problem:** Proxy consensus runs faster than primary; need to prevent unbounded growth.

**Mechanism:**
```rust
// Configuration
const MAX_PROXY_BLOCKS_PER_PRIMARY_ROUND: u64 = 10;

// In ProxyRoundManager
impl ProxyRoundManager {
    fn should_propose(&self) -> bool {
        if self.backpressure_active {
            return false;  // Primary signaled backpressure
        }

        let count = self.block_store.count_blocks_for_primary_round(self.primary_round);
        count < MAX_PROXY_BLOCKS_PER_PRIMARY_ROUND
    }

    fn pause_proposing(&mut self) {
        self.backpressure_active = true;
    }

    fn resume_proposing(&mut self) {
        self.backpressure_active = false;
    }
}
```

**Two Mechanisms:**
1. **Block count limit:** Pause proposing when max blocks reached for current primary_round
2. **Primary backpressure signal:** Primary sends BackpressureOn/Off based on its processing capacity

**Behavior:** When backpressure active, proxy leader skips proposing. Simple on/off, no graduated levels.

---

## Proxy Leader Election

**Simple round-robin among proxy validators:**
```rust
impl ProxyLeaderElection {
    pub fn get_leader(&self, round: Round) -> Author {
        let proxy_validators = &self.epoch_state.proxy_validators;
        let index = (round as usize) % proxy_validators.len();
        proxy_validators[index].address
    }

    pub fn is_leader(&self, round: Round) -> bool {
        self.get_leader(round) == self.author
    }
}
```

**Note:** Not reputation-based (unlike primary consensus). Simple deterministic rotation.

---

## State Sync / Block Retrieval

**ProxyBlockRetriever** (follows existing BlockRetriever pattern):
```rust
pub struct ProxyBlockRetriever {
    network: ProxyNetworkSender,
    block_store: Arc<ProxyBlockStore>,
    epoch_state: Arc<EpochState>,
}

impl ProxyBlockRetriever {
    /// Fetch missing proxy blocks from peers
    pub async fn retrieve_blocks(
        &self,
        target_qc: &QuorumCert,
    ) -> Result<Vec<ProxyBlock>, ProxyConsensusError> {
        // 1. Check if we have the block locally
        if self.block_store.get_block(target_qc.certified_block_id()).is_some() {
            return Ok(vec![]);
        }

        // 2. Send retrieval request to random proxy peer
        let peer = self.select_random_proxy_peer();
        let request = ProxyBlockRetrievalRequest {
            block_id: target_qc.certified_block_id(),
            num_blocks: 10,  // Fetch chain
            target_round: target_qc.certified_block_round(),
        };

        // 3. Fetch and verify
        let response = self.network.send_rpc(peer, request).await?;
        self.verify_retrieved_blocks(&response.blocks, target_qc)?;

        Ok(response.blocks)
    }
}
```

**Catching Up After State Sync:**
```
1. State sync to latest primary block (normal state sync)
2. ProxyBlockStore genesis = epoch start primary block (always)
3. For blocks between genesis and current:
   - Trust QCs (they're certified by quorum)
   - Don't need to re-execute (proxy doesn't execute)
4. Fetch recent proxy blocks via ProxyBlockRetriever
5. Resume normal proxy consensus
```

**Key Design Decision:** Genesis is ALWAYS epoch start primary block, never changes. After state sync, trust QCs for blocks before fetched state.

---

## QuorumStore Interaction

**Transaction Flow:**
```
Client sends to: at least one proxy + one non-proxy (for reliability)

Happy Path (proxy consensus active):
- Proxies pull from local mempool → OptQS (uncertified batches) → proxy block
- Non-proxy primaries receive batches (store them) but don't create new batches
- Primary block formed from aggregated proxy blocks

Fallback (proxy consensus shutdown):
- Primary leader pulls from local mempool → OptQS (inline + opt + proof)
- Standard AptosBFT QuorumStore flow
```

**Batch Creation Responsibility:**

| Mode | Batch Creators | Batch Users |
|------|----------------|-------------|
| Happy path | Proxies only | Proxy blocks |
| Fallback | Any validator | Primary blocks |

**QuorumStore Mode:**
- Proxy blocks: Uncertified batches only (OptQuorumStore, no proof wait)
- Fallback blocks: Same as current AptosBFT (inline + opt + proof)

**Note:** Mempool gossip is disabled when QS enabled, so clients must send to validators that will create batches.
