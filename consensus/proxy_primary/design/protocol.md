# Proxy Primary Consensus Protocol

## Block Format

**Primary blocks:**
```
B_G ←—— (B_1, QC_G) ←—— (B_2, QC_1) ←—— (B_3, QC_2) ←—— ...
```

**Proxy blocks (grouped by primary_round):**
```
B_G ←—— [b_1,...,(b_i, QC_G)] ←—— [b_j,...,(b_k, QC_1)] ←—— [b_l,...,(b_m, QC_2)] ←—— ...
         primary_round=1              primary_round=2              primary_round=3
```

- Each proxy block contains `primary_round` field
- `b_1,...,b_i` has `primary_round=1`, `b_j,...,b_k` has `primary_round=2`, etc.
- First proxy block of each primary block either:
  - Links to last proxy block of parent primary block, OR
  - Links to parent primary block directly (when initializing)

**Parent Block Linking (Lookup-based approach):**
- First proxy block after init: `parent.id` references the primary genesis block
- Subsequent proxy blocks: `parent.id` references a proxy block in ProxyBlockStore
- Verification: lookup `parent.id` in ProxyBlockStore; if not found, check if it's the known genesis primary block
- Timestamps are **independent** - proxy timestamps only need to increase within proxy chain (no constraint with primary timestamps)

---

## Proxy Consensus

### State Deltas

```
QC_primary   : QuorumCert      // highest primary QC
TC_primary   : TimeoutCert     // highest primary TC
primary_round: Round           // = max(QC_primary.round, TC_primary.round) + 1
```

### Protocol Deltas

#### Process Primary QC
```
fn process_primary_qc(qc: QuorumCert):
    if qc.round > QC_primary.round:
        QC_primary = qc
        primary_round = max(QC_primary.round, TC_primary.round) + 1
```

#### Process Primary TC
```
fn process_primary_tc(tc: TimeoutCert):
    if tc.round >= primary_round:
        // Shut down proxy round_manager
        shutdown_proxy_consensus()

    if tc.round > TC_primary.round:
        TC_primary = tc
        primary_round = max(QC_primary.round, TC_primary.round) + 1
```

#### Enter New Proxy Round (Leader)
```
fn enter_new_proxy_round():
    parent = block_from_highest_proxy_qc()

    if I_am_leader:
        // Determine primary_round for new block
        if parent.has_primary_qc():
            B.primary_round = parent.primary_round + 1
        else:
            B.primary_round = parent.primary_round

        // Attach primary QC if available for this primary round
        if QC_primary.round == B.primary_round - 1:
            B.primary_qc = QC_primary

        // ... Existing consensus logic (generate proposal, broadcast)
```

#### Enter New Opt Proxy Round (Leader)
```
fn enter_new_opt_proxy_round(parent: Block):
    if I_am_leader_of(parent.round + 1):
        // Determine primary_round for new block
        if parent.has_primary_qc():
            B.primary_round = parent.primary_round + 1
        else:
            B.primary_round = parent.primary_round

        // Attach primary QC if available for this primary round
        if QC_primary.round == B.primary_round - 1:
            B.primary_qc = QC_primary

        // ... Existing consensus logic (generate opt proposal, broadcast)
```

#### Process Proxy Block
```
fn process_proxy_block(B: Block):
    // Validate primary_round consistency
    if B.parent.has_primary_qc():
        ensure B.primary_round == B.parent.primary_round + 1
    else:
        ensure B.primary_round == B.parent.primary_round

    // Validate and process attached primary QC
    if B.has_primary_qc():
        ensure B.primary_qc.round == B.primary_round - 1
        process_primary_qc(B.primary_qc)

    // ... Existing consensus logic (verify, vote)
```

#### Process Opt Proxy Block
```
fn process_opt_proxy_block(B: Block):
    // Validate primary_round consistency
    if B.parent.has_primary_qc():
        ensure B.primary_round == B.parent.primary_round + 1
    else:
        ensure B.primary_round == B.parent.primary_round

    // Validate and process attached primary QC
    if B.has_primary_qc():
        ensure B.primary_qc.round == B.primary_round - 1
        process_primary_qc(B.primary_qc)

    // ... Existing consensus logic (buffer, convert when parent QC arrives)
```

#### Process Ordered Proxy Block
```
fn process_ordered_proxy_block(B: Block):
    if B.has_primary_qc():
        // Cut happens IMMEDIATELY when ordered (order cert formed), not when committed
        // Collect all ordered proxy blocks for this primary round
        proxy_blocks = get_ordered_proxy_blocks(primary_round = B.primary_qc.round + 1)

        // Forward to ALL primaries (broadcast for reliability)
        forward_to_primaries(proxy_blocks)  // Every proxy broadcasts this

    // NOTE: Proxy blocks are NOT executed - only ordered
```

**QC Attachment Rule:** Proxy leader attaches primary_qc to the **first proxy block proposed after QC is received** (simple deterministic rule).

**QC Ordering Guarantee:** QC_R cannot arrive before QC_{R-1} is attached due to causality:
- QC_R requires primary block B_R to exist
- B_R requires proxy blocks with QC_{R-1} to be ordered and forwarded
- This naturally enforces QC ordering - no buffering needed

#### State Sync
```
fn state_sync():
    // Use primary consensus to sync to latest committed round
    primary_consensus.sync_to_latest()

    // Fetch remaining proxy consensus blocks
    fetch_proxy_blocks_since(last_committed_primary_round)
```

---

## Primary Consensus

### State Deltas

```
// TBD - may need to track proxy consensus state
```

### Protocol Deltas

#### Process Ordered Proxy Blocks
```
fn process_ordered_proxy_blocks(proxy_blocks: Vec<Block>):
    // Verify proxy block chain
    ensure all_linked_by_hashes(proxy_blocks)
    ensure all_ordered(proxy_blocks)
    ensure proxy_blocks.last().has_primary_qc()

    let primary_round = proxy_blocks.last().primary_qc.round + 1
    ensure all proxy_blocks have primary_round == primary_round

    // Verify linkage to parent
    let parent_primary_block = get_parent_primary_block()
    if parent_primary_block.consists_of_proxy_blocks():
        ensure proxy_blocks.first() extends parent_primary_block.last_proxy_block()
    else:
        ensure proxy_blocks.first() extends parent_primary_block

    // Form primary block by aggregating proxy blocks
    primary_block = aggregate_proxy_blocks(proxy_blocks, primary_round)

    // ... Existing consensus logic (insert block, vote)
```

#### Enter New Primary Round
```
fn enter_new_primary_round():
    if leader_is_proxy() and previous_leader_was_non_proxy():
        // Initialize proxy consensus with parent as genesis
        initialize_proxy_consensus(parent_primary_block = genesis)

    // ... Existing consensus logic
```

---

## Existing AptosBFT Reference

For reference, existing AptosBFT consensus flow:

```
Block time: 1 message delay (optimistic proposals)
Block ordering: 3 message delays (order votes)

t:   Leader broadcasts PROPOSAL(r)
t+1: Validators broadcast VOTE(r)
t+2: QC formed → broadcast ORDER_VOTE(r); Leader broadcasts PROPOSAL(r+1)
t+3: Order Cert formed → block r ORDERED
```

**Two vote types:**
1. Vote → QC (Quorum Certificate) = block certified
2. Order Vote → Order Certificate = block ordered

Both proxy consensus and primary consensus use this same AptosBFT protocol.

---

## Randomness and Decryption

- Both added **during primary consensus** (after proxy consensus)
- Proxy consensus forms block proposal (user txns + aggregated validator txns)
- Primary consensus adds randomness + decryption → votes/orders/commits

---

## Safety Rules

- Same key pair for proxy and primary voting
- **Completely separate** highest_voted_round for proxy vs primary
- Independent safety state (separate persistent storage)
