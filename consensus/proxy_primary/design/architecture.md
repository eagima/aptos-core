# Proxy Primary Architecture

## System Structure

- **N primaries**: Geo-distributed validators satisfying BFT assumption (>2/3 honest stake)
- **n proxies**: Subset of primaries forming a proxy group (n << N), may be co-located

**Common Case (proxy leader):**
```
┌─────────────────────────────────────────────────────────────────┐
│                       PROXY GROUP (n nodes)                      │
│                                                                  │
│  Proxy Consensus (AptosBFT) → Proxy Blocks                      │
│                                     │                            │
│                          Primary QC cuts                         │
│                                     ↓                            │
│                            Primary Block                         │
└─────────────────────────────────────────────────────────────────┘
                                     ↓ forward
┌─────────────────────────────────────────────────────────────────┐
│                      PRIMARIES (N nodes)                         │
│                                                                  │
│  Primary Consensus (AptosBFT) → Primary Pipeline                │
└─────────────────────────────────────────────────────────────────┘
```

**Fallback Case (non-proxy leader):**
```
┌─────────────────────────────────────────────────────────────────┐
│                      PRIMARIES (N nodes)                         │
│                                                                  │
│  Non-proxy leader proposes → Primary Consensus → Primary Pipeline│
│  (Standard AptosBFT, no proxy involvement)                       │
└─────────────────────────────────────────────────────────────────┘
```

## Flow Diagrams

### Happy Path (Proxy Leader)

Proxy consensus and primary consensus run **in parallel** (not blocking each other).

```
Proxy Consensus (continuous)             Primary Consensus (parallel)
────────────────────────────             ───────────────────────────

b_1 proposed → voted → ordered
        │
b_2 proposed → voted → ordered
        │
b_3 proposed ─────────────────────────── QC_G available
        │                                     │
        ↓                                     │
b_3 includes QC_G ←───────────────────────────┘
        │
        ↓
b_3 ordered (has QC_G)
        │
        ├─────────────────────────────→ [b_1, b_2, b_3] forwarded
        │                                     │
b_4 proposed → voted → ordered                ↓
        │                              Form primary block B_1
b_5 proposed → voted → ordered         from [b_1, b_2, b_3]
        │                                     │
b_6 proposed ─────────────────────────── QC_1 available (QC on B_1)
        │                                     │
        ↓                                     │
b_6 includes QC_1 ←───────────────────────────┘
        │
        ↓
b_6 ordered (has QC_1)
        │
        ├─────────────────────────────→ [b_4, b_5, b_6] forwarded
        │                                     │
       ...                                    ↓
                                       Form primary block B_2
                                       from [b_4, b_5, b_6]
                                              │
                                             ...
```

**Key points:**
- Proxy consensus runs continuously (b_1, b_2, b_3, ...)
- When primary QC is available, proxy leader attaches it to current proxy block
- Ordered proxy block with QC "cuts" previous proxy blocks → forms primary block
- Primary consensus does NOT block proxy consensus

### Unhappy Path (Timeout → Fallback)

```
Proxy Consensus                          Primary Consensus
─────────────────                        ─────────────────

1. Proxy leader proposes proxy block
        │
        ↓
2. [TIMEOUT - no progress]
        │                                           │
        │                                           ↓
        │                                 3. Primary TC formed
        │                                    (timeout certificate)
        │                                           │
        ←──────────────────────────────────────────┘
        │
        ↓
4. Proxy receives TC where
   TC.round >= primary_round
        │
        ↓
5. SHUTDOWN proxy consensus
                                                    │
                                                    ↓
                                          6. View change to
                                             non-proxy leader
                                                    │
                                                    ↓
                                          7. Non-proxy proposes
                                             primary block directly
                                                    │
                                                    ↓
                                          8. Standard AptosBFT
                                             (no proxy involvement)
```

### Recovery Path (Back to Proxy)

```
Primary Consensus                        Proxy Consensus
─────────────────                        ─────────────────

1. On-chain shows proxies healthy
        │
        ↓
2. Leader rotation back to proxy
        │
        ├──────────────────────────────→ 3. Initialize proxy consensus
        │                                    with current primary block
        │                                    as genesis
        │                                           │
        │                                           ↓
        │                                 4. Proxy consensus starts
        │                                    new proxy rounds
        │
        ↓
5. Resume happy path flow
```

## What Proxies Do (Common Case)

Based on the happy path flow:

1. **Run proxy consensus continuously** (b_1, b_2, b_3, ...)
   - Propose → vote → order proxy blocks independently of primary consensus

2. **Attach primary QC when available**
   - When primary QC arrives, proxy leader includes it in current proxy block
   - Example: b_3 includes QC_G, b_6 includes QC_1

3. **Forward ordered proxy blocks to primaries**
   - When ordered proxy block has QC, forward all proxy blocks for that primary round
   - Example: b_3 ordered with QC_G → forward [b_1, b_2, b_3] to primaries

**Proxy Pipeline:** Proxies do NOT run pipeline other than consensus.

## What Primaries Do (Common Case)

Based on the happy path flow:

1. **Run primary consensus in parallel** with proxy consensus
   - Vote and order primary blocks
   - Produce QCs (QC_G, QC_1, QC_2, ...)

2. **Receive ordered proxy blocks** from proxies
   - Example: receive [b_1, b_2, b_3] when b_3 is ordered with QC_G

3. **Form primary block** by aggregating proxy blocks
   - Example: B_1 formed from [b_1, b_2, b_3]

4. **Run full pipeline** (execution, certification, commit)

## Fallback (Timeout → Non-Proxy Leader)

Based on the unhappy path flow:

1. **Timeout occurs** - proxy consensus makes no progress
2. **Primary TC formed** - timeout certificate in primary consensus
3. **Proxy receives TC** where `TC.round >= primary_round`
4. **Proxy consensus SHUTS DOWN**
5. **View change** to non-proxy leader
6. **Non-proxy proposes** primary block directly (standard AptosBFT)

## Recovery (Back to Proxy Leader)

Based on the recovery path flow:

1. **On-chain shows proxies healthy**
2. **Leader rotation** back to proxy
3. **Initialize proxy consensus** with current primary block as genesis
4. **Resume happy path** - proxy consensus starts new rounds

## Block Contents

| Content | Proxy Block | Primary Block |
|---------|-------------|---------------|
| User transactions | ✓ | ✓ (from proxy blocks) |
| Validator transactions | ✓ (aggregated DKG/JWK) | ✓ (from proxy blocks) |
| Randomness | ✗ | ✓ |
| Decryption | ✗ | ✓ |

**Note on Validator Transactions:**
- **DKG transcripts**: Aggregated off-chain, any validator can submit
- **JWK updates**: Have aggregated signatures (AggregateSignature), any validator can submit
- Including validator txns in proxy blocks enables **all primaries to independently form the same primary block** from OrderedProxyBlocksMsg

## Component Overview

**Proxy nodes (n) run:**
- ProxyRoundManager (proxy consensus)
- Primary RoundManager (primary consensus) - participates in both layers
- ProxySafetyRules (separate persistent state from primary)
- ProxyBlockStore (separate from primary BlockStore)

**Non-proxy primaries (N-n) run:**
- Primary RoundManager only
- Primary SafetyRules
- Primary BlockStore
- Full Pipeline (execution, certification, commit)
- Randomness generation
- Decryption

---

## Implementation Details

### Proxy Membership
- Determined by **on-chain config** (EpochState has proxy validator list)
- Changes at epoch boundaries

### Leader Election
- **Separate schedules** for proxy and primary consensus
- Proxy consensus: **Simple round-robin rotation** among n proxies only (not reputation-based)
- Primary consensus: Leader rotation among all N primaries (includes non-proxies even in happy path)

### Message Routing
- **Proxy messages** (ProxyProposalMsg, ProxyVoteMsg, etc.): Only between n proxy nodes
- **Primary messages**: Between all N primaries
- **OrderedProxyBlocksMsg**: Broadcast from proxies to all N primaries

### Manager Coordination
- Two separate managers (ProxyRoundManager + primary RoundManager)
- ProxyRoundManager runs as **separate tokio task** (parallel with primary RoundManager)
- Primary QC/TC updates to proxy: **Internal channel** (proxies run both consensus layers - no network messages needed)
- Ordered proxy blocks to primaries: OrderedProxyBlocksMsg broadcast (**every proxy node broadcasts** for reliability)

### Primary Leader Behavior
- **Non-proxy primary leader**: Waits for OrderedProxyBlocksMsg from proxies
  - If proxies don't deliver, primary times out → TC → fallback
- **Proxy primary leader**: Uses local proxy blocks directly (no network wait)
- All primaries **independently form the same primary block** from OrderedProxyBlocksMsg (deterministic aggregation)

### Safety Rules
- Same key pair for proxy and primary voting
- Separate persistent storage for voting state (prevents cross-layer interference)

### Storage
- ProxyBlockStore: Completely separate from primary BlockStore
- Stores proxy blocks, QCs, and order certificates
- **Initially in-memory only** (no persistence)
  - Safety handled by ProxySafetyRules (separate persistent storage)
  - Avoids 10x I/O overhead (10+ proxy blocks per primary)
  - Recovery via peer fetch is fast (proxies are co-located)
  - Can add persistence later if needed
- Pruned/discarded when proxy consensus shuts down (txns return to mempool)

### Epoch Transitions
- ProxyRoundManager shutdown and recreated at epoch boundary (like primary)
- Proxy validator list refreshed from new EpochState
- Proxy consensus **stops when reconfiguration primary block commits**
- Maximum proxy blocks included before epoch end

### Validator Transaction Censorship (DEFERRED)
- **Issue**: If proxies censor all validator txns, DKG cannot finish and JWK cannot be updated
- **Solution needed**: Primary consensus needs a second timeout to detect validator txn censorship and trigger fallback
- **Status**: Deferred to later implementation stage

### Payload
- Optimistic QuorumStore batches (no proof verification for speed)
- Primaries verify QS proofs before executing

### Proxy Genesis
- **No special proxy genesis block** - uses primary block directly
- When proxy consensus initializes: `primary_round = genesis_primary_block.round + 1`
- First proxy block references primary block via `parent: BlockInfo`
- Lookup-based parent verification handles both cases:
  - If `parent.id` found in ProxyBlockStore → parent is a proxy block
  - If `parent.id` not found → check if it's the known genesis primary block

### State Sync (Catching Up)
- **Proxy consensus has NO separate state sync** (doesn't execute)
- Catching up approach:
  1. **Large gap**: State sync to latest primary consensus first, then fetch missing proxy blocks
  2. **Small gap**: Directly fetch missing proxy blocks via ProxyBlockRetrievalRequest
- **Fixed threshold** determines "large" vs "small" gap (configurable N rounds/blocks)

### Proxy → Proxy Transition
- When primary leadership goes from proxy A to proxy B, proxy consensus **continues without restart**
- Only restarts when transitioning from non-proxy to proxy
