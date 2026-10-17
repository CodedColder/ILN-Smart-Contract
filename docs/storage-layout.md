# Storage Layout

## 1. Overview
The ILN protocol is composed of **five independently deployed Soroban contracts**:

| Contract | Crate | Key Enum(s) |
| -------- | ----- | ----------- |
| Invoice Liquidity | `invoice_liquidity` | `DataKey` (`storage.rs`) |
| Insurance Pool | `insurance_pool` | `DataKey` (`lib.rs`) |
| Reputation Bonus | `reputation_bonus` | `ConfigKey`, `ReputationKey`, `InvoiceKey` |
| ILN Distribution | `iln_distribution` | `StorageKey` |
| ILN Governance | `iln_governance` | `StorageKey` |

Each contract has its own storage namespace on the Soroban host: all ledger
entries are keyed by `(contract_address, key)`, so **no two distinct deployed
contract instances can ever collide with each other**, even if they define an
identically-named key enum/variant (e.g. both `iln_distribution` and
`iln_governance` have a `StorageKey::GovToken`/`IlnContract`-style variant —
these live under different contract addresses and are fully isolated by the
host).

The collision surface that actually matters is therefore **within a single
contract's own key space** — i.e. do any of the enums/variants used *inside
one contract* serialize to the same storage key. This document maps every
storage key used by every contract and confirms there are no intra-contract
collisions.

### Why enum-keyed storage is collision-safe
Soroban's `#[contracttype]` derive serializes an enum value as an
`ScVal::Vec` whose first element is the `Symbol` of the variant name, followed
by any associated data:
- Unit variant: `Admin` → `[Symbol("Admin")]`
- Tuple variant: `Invoice(42)` → `[Symbol("Invoice"), U64(42)]`

Two keys collide only if both the variant name **and** all associated
arguments are identical. Since each contract uses a single family of key
enums with distinct variant names, and dynamic variants are always
parameterized by a unique id/address, no collisions occur.

---

## 2. `invoice_liquidity` — `DataKey` (`contracts/invoice_liquidity/src/storage.rs`)

### Instance Storage
| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `Admin` | `Address` | Contract administrator |
| `Config` | `Config` | System parameters (fees, decay logic, token addresses, oracle) |
| `FeeRate` | `u32` | Protocol fee rate |
| `MaxDiscountRate` | `u32` | Maximum allowed discount rate for an invoice |
| `DistributionContract` | `Address` | External distribution contract address |
| `Paused` | `bool` | Emergency pause flag |
| `MinPayerReputation` | `u32` | Minimum payer reputation required to fund an invoice (Issue #28) |
| `NextInvoiceId` | `u64` | Auto-incrementing invoice id counter |

### Persistent Storage
| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `Invoice(u64)` | `Invoice` | Individual invoice state |
| `InvoiceCount` | `u64` | Legacy invoice counter |
| `Token` | `Address` | Primary token address |
| `PayerScore(Address)` | `ReputationScore` | Payer reputation + last-activity ledger |
| `InvoiceFunders(u64)` | `Vec<(Address, i128)>` | LP funders for partial funding |
| `ApprovedToken(Address)` | `bool` | Whitelisted token flag |
| `TokenList` | `Vec<Address>` | Iterable list of approved tokens |
| `TokenDecimals(Address)` | `u32` | Decimal precision per allowlisted token |
| `Reputation(Address)` | reputation profile | Detailed reputation profile (Issue #26) |
| `Appeal(u64)` | `AppealRecord` | Evidence hash for a default appeal |
| `PreDefaultPayerScore(u64)` | `u32` | Snapshot of payer score before a default penalty |
| `LpScore(Address)` | `u32` | LP reputation score |
| `FundQueue(u64)` | `Vec<LpFundRequest>` | LPs waiting to fund an invoice |
| `QueueResolution(u64)` | `Address` | Selected LP from a funding queue |
| `FundQueueOpenedAt(u64)` | `u32` | Ledger the first LP joined the queue (MEV maturity anchor) |
| `QueueResolvedAt(u64)` | `u32` | Ledger the winner was locked in (stalled-winner recovery anchor, Issue #1020); additive key, no migration |
| `TotalInvoices` | `u64` | Global protocol stat |
| `TotalFunded` | `u64` | Global protocol stat |
| `TotalPaid` | `u64` | Global protocol stat |
| `TotalVolumeUsdc` | `i128` | Global protocol stat |
| `TotalVolumeEurc` | `i128` | Global protocol stat |
| `TotalVolumeXlm` | `i128` | Global protocol stat |
| `TokenVolume(Address)` | `i128` | Per-token volume stat |
| `ReferralCount(BytesN<32>)` | `u32` | Referral counts keyed by fixed-size code |
| `Dispute(u64)` | dispute record | Dispute record for an invoice |
| `SubmitterInvoices(Address)` | `Vec<u64>` | Invoice ids submitted by a freelancer |
| `LpInvoices(Address)` | `Vec<u64>` | Invoice ids funded by an LP |
| `TopPayersHeap` | fixed-size heap | Top payers by reputation score (Issue #77) |
| `InvoiceNft(u64)` | `InvoiceNftMetadata` | NFT metadata for a funded invoice (Issue #423) |
| `InvoiceNftOwner(u64)` | `Address` | NFT owner tracking (Issue #423) |

No two variants share the same name, and every parameterized variant
(`Invoice(u64)`, `PayerScore(Address)`, etc.) is disambiguated by its type and
value — a `u64` invoice id and an `Address` can never serialize identically.

### TTL Management and Upgrade Guard (Issue #1082)

Five persistent key categories carry **protocol-critical data** and have explicit
TTL management. Losing any of these entries to expiry mid-flight would cause
silent data loss without any on-chain indication:

| Key | TTL managed by | Threshold / Extend-to |
|-----|---------------|-----------------------|
| `InvoiceCore(u64)` | `storage::save_invoice` | 1,000,000 / 2,000,000 ledgers |
| `InvoiceMetadata(u64)` | `storage::save_invoice` | 1,000,000 / 2,000,000 ledgers |
| `FundQueue(u64)` | `invoice::save_fund_queue` | 1,000,000 / 2,000,000 ledgers |
| `FundQueueOpenedAt(u64)` | `invoice::try_set_fund_queue_opened_at` | 1,000,000 / 2,000,000 ledgers |
| `QueueResolution(u64)` | `invoice::save_queue_resolution` | 1,000,000 / 2,000,000 ledgers |

**Pre-upgrade TTL guard:** `upgrade()` calls
`storage::validate_ttl_fields_pre_upgrade()` before swapping the WASM binary.
The guard samples the most-recent `TTL_UPGRADE_SAMPLE_SIZE` (50) invoices and
checks each of the five categories above. If any existing entry has a remaining
TTL below `TTL_MIN_LEDGERS_BEFORE_UPGRADE` (500,000 ledgers ≈ 29 days), the
upgrade is **rejected fail-closed** with
`ContractError::UpgradeRemovesTtlFields` (code 50). The admin must refresh the
affected entries by re-invoking the relevant storage helper before retrying.

> **Note on the "TTL managed by" column:** the functions named above are the
> ones in `invoice.rs` that `lib.rs` actually imports and calls from
> `join_fund_queue` / `resolve_fund_queue`. Until Issue #1014, `storage.rs`
> also defined a second, identically-named set of `get_fund_queue` /
> `save_fund_queue` / `get_queue_resolution` / `save_queue_resolution` /
> `try_set_fund_queue_opened_at` / `get_fund_queue_opened_at` helpers (keyed
> off the same `DataKey` enum, re-exported as `StorageKey` in `invoice.rs`)
> that were **never called from anywhere** — an earlier, superseded draft of
> the Issue #1091/#1011 TTL-extension fix that landed in the wrong module.
> Issue #1014 removed that dead copy, so the `invoice.rs` functions above are
> now the only implementation. Extend TTL logic there only; do not
> reintroduce a parallel copy in `storage.rs`.

### Queued-entry retention guarantees (Issues #1011, #1012, #1013, #1014, #1015, #1016, #1018)

`resolve_fund_queue`'s MEV-mitigation delay (Issue #MEV-1, `QUEUE_DELAY_LEDGERS`
= 120 ledgers ≈ 10 minutes) depends entirely on `FundQueueOpenedAt(id)` still
being readable when someone calls `resolve_fund_queue`. The queue itself
(`FundQueue(id)`) and the eventual winner record (`QueueResolution(id)`) carry
the same requirement — losing any of the three mid-flight is silent data loss
for an invoice that may still be actively fundable.

**Before (pre-#1011):** `invoice::save_fund_queue`,
`invoice::try_set_fund_queue_opened_at`, and `invoice::save_queue_resolution`
— the versions actually wired into `join_fund_queue` / `resolve_fund_queue` —
wrote these three keys with Soroban's default persistent TTL and never called
`extend_ttl`. A TTL-safe version of the same three functions existed in
`storage.rs` (written for Issue #1091) but was never imported or called by
`lib.rs`, so it provided no protection in practice. A queue that sat open for
longer than the default TTL (e.g. an invoice nobody resolved promptly) could
have its `FundQueueOpenedAt` entry archived before `resolve_fund_queue` ran.
Because the maturity check was written as
`if let Some(opened_at) = get_fund_queue_opened_at(...) { ... }`, a missing
entry **silently skipped the delay check entirely** — fail-open — letting the
queue resolve immediately with no MEV protection, rather than surfacing the
lost record as an error.

**After:**
- **Retention (#1011):** `invoice::save_fund_queue`,
  `invoice::try_set_fund_queue_opened_at`, and `invoice::save_queue_resolution`
  each call `extend_ttl(key, FUND_QUEUE_TTL_THRESHOLD, FUND_QUEUE_TTL_EXTEND_TO)`
  = `(1_000_000, 2_000_000)` ledgers on every write — the same policy
  `save_invoice` already applies to `InvoiceCore`/`InvoiceMetadata`. Every LP
  join refreshes `FundQueueOpenedAt`'s TTL even though its *value* is only set
  once, so a queue that keeps receiving joins never drifts toward expiry, and
  even a queue frozen right after it opens has ~1,000,000+ ledgers (~58+ days
  at 5s/ledger) of headroom before the pre-upgrade threshold, let alone actual
  archival.
- **Fail-closed on loss anyway (#1013):** `resolve_fund_queue` no longer
  treats a missing `FundQueueOpenedAt` as "no gate required." Since the queue
  is already confirmed non-empty at that point in the function, a missing
  entry can only mean it was archived or otherwise lost — never that no LP
  ever joined. That case now returns
  `ContractError::QueueStateArchived` (code 75) and publishes a
  `queue_resolve_attempt` event with `success: false`, instead of silently
  granting an unprotected resolution.
- **Instrumentation (#1015):** a new read-only contract method,
  `get_fund_queue_ttl_margins(invoice_id) -> FundQueueTtlMargins`, reports the
  live TTL and the margin above `TTL_MIN_LEDGERS_BEFORE_UPGRADE` for each of
  the three keys (`None` when a key doesn't exist). Operators can poll this
  per-invoice to catch TTL drift before it ever reaches the upgrade guard
  (#1082) or the `QueueStateArchived` failure path (#1013) — see
  [`docs/exploit-pattern-monitoring.md`](exploit-pattern-monitoring.md).

**Regression coverage (#1012):**
[`contracts/invoice_liquidity/src/tests_queue_archival.rs`](../contracts/invoice_liquidity/src/tests_queue_archival.rs)
exercises all of the above: retention (TTL extension on `join_fund_queue` /
`resolve_fund_queue`, including that a second join refreshes
`FundQueueOpenedAt`'s TTL), the fail-closed `QueueStateArchived` path
(including that it's distinguished from the ordinary `QueueNotMature` and
`NotFunded` cases, so the new guard doesn't misfire), the
`get_fund_queue_ttl_margins` instrumentation (including the exact ledger at
which its margin crosses zero), and that neither of the touched entry points
gates on caller identity.
[`tests_queue_renewal_proptest.rs`](../contracts/invoice_liquidity/src/tests_queue_renewal_proptest.rs)
(#1018) generalizes the renewal and fail-closed-archival scenarios into two
properties checked across randomly-spaced, arbitrary-length join sequences,
rather than just the fixed scenarios above: renewal never loses the record
as long as gaps between joins stay under `FUND_QUEUE_TTL_THRESHOLD`, and the
fail-closed rejection fires regardless of how much renewal history or
post-loss delay preceded it.

**Guarantee going forward:** as long as at least one `join_fund_queue` call
has ever succeeded for an invoice, `FundQueue(id)` and `FundQueueOpenedAt(id)`
are retained for ≥1,000,000 ledgers from that call, and `QueueResolution(id)`
is retained for ≥1,000,000 ledgers from `resolve_fund_queue` succeeding. If an
entry is ever found missing despite this (e.g. stale state written before
this fix shipped), `resolve_fund_queue` fails closed rather than silently
dropping the MEV-mitigation delay.

**Migration note:** this is a behavior-only fix — no storage layout or key
format changed, so no data migration is required. The one operational
follow-up: any invoice whose fund queue was opened *before* this fix deployed
and has not received a new `join_fund_queue` call since will still be sitting
on its pre-fix (un-extended) TTL. Operators should run
`get_fund_queue_ttl_margins` across invoices with an open, unresolved queue
after deploying this fix and, for any with a low or negative margin, prompt a
new LP join (or have the admin re-invoke the relevant `invoice::save_*`
helper) to refresh the TTL before it actually expires.

### Resolving a queue after invoice cleanup (Issue #1017)

Distinct from the TTL/retention guarantees above: `resolve_fund_queue` had
no awareness of `invoice.status` at all — unlike `join_fund_queue`, which
gates every call on it. `cancel_invoice` and `expire_invoice` never touch
`FundQueue` / `FundQueueOpenedAt` / `QueueResolution`, so a queue that was
non-empty but not yet resolved when the invoice was cancelled or expired
stayed fully resolvable afterward: anyone could still call
`resolve_fund_queue` and have it pick and announce a reputation-weighted
"winning" LP (`FundQueueResolved`) for an invoice that could never actually
be funded.

**Before:** `resolve_fund_queue` succeeded (or failed only on
`QueueNotMature` / `QueueStateArchived` / empty-queue grounds) regardless of
`invoice.status`.

**After:** `resolve_fund_queue` loads the invoice and applies the same
status match `join_fund_queue` already uses. Once the invoice has left
`Pending` / `PartiallyFunded`, resolving a *not-yet-resolved* queue now
fails with the matching status-specific error (`AlreadyCancelled`,
`InvoiceExpired`, `AlreadyFunded`, `AlreadyPaid`, `InvoiceDefaulted`,
`InvoiceAppealed`, `InvoiceDisputed`) instead of finalizing dead economic
state. A queue resolved *before* cleanup is unaffected — the pre-existing
"already resolved" cache check still runs first and keeps returning that
historical winner. See `INVARIANT Q1`/`Q2` in
[`invoice.rs`](../contracts/invoice_liquidity/src/invoice.rs)'s formal
verification header, and
[`tests_queue_status_gate.rs`](../contracts/invoice_liquidity/src/tests_queue_status_gate.rs)
for the regression suite (cancelled/expired-with-unresolved-queue,
resolved-before-cleanup-is-preserved, and the ordinary success path).

**Migration note:** behavior-only change, no storage or key format impact.
Any invoice that was cancelled/expired in the past while its queue was
non-empty and unresolved will simply have its queue permanently
unresolvable now (it already was unfundable; this only stops a late,
misleading `resolve_fund_queue` call from pretending otherwise). No backfill
is needed — there is nothing to migrate in existing `QueueResolution` data,
since none could have been incorrectly written by the old code for an
*already-terminal* invoice in a way this fix needs to undo.

---

## 3. `insurance_pool` — `DataKey` (`contracts/insurance_pool/src/lib.rs`)

| Variant | Storage Type | Stored Value | Purpose |
| ------- | ------------ | ------------ | ------- |
| `Admin` | Instance | `Address` | Authorised admin (reports confirmed defaults) |
| `Balance` | Instance | `i128` | Total pool balance (premiums minus payouts) |
| `Coverage` | Instance | `i128` | Active flat per-claim coverage cap |
| `Enrolled(Address)` | Persistent | `bool` | LP enrollment flag |
| `Premiums(Address)` | Persistent | `i128` | Cumulative premium paid per LP |
| `Claimed(u64)` | Persistent | `bool` | Whether a claim has been processed for an invoice id |
| `PendingCoverage` | Instance | `i128` | Proposed new coverage cap awaiting timelock (Issue #542) |
| `PendingAdmin` | Instance | `Address` | Proposed new admin awaiting timelock (Issue #542) |
| `CoverageEta` | Instance | `u64` | Ledger timestamp at which pending coverage change becomes executable (Issue #542) |
| `AdminEta` | Instance | `u64` | Ledger timestamp at which pending admin transfer becomes executable (Issue #542) |

> **TTL note:** Persistent keys (`Enrolled`, `Premiums`, `Claimed`) use Soroban's default persistent TTL (min 1,000,000 / max 2,000,000 ledgers). Instance keys live for the contract's lifetime and are removed when the contract instance is deleted.

---

## 4. `reputation_bonus` — three key enums, one contract

This contract splits its keys across three small enums defined in separate
modules. Because all three are used by the **same** deployed contract, they
share one storage namespace, so the check that matters is whether their
variant names can serialize identically. They cannot: each enum has disjoint
variant names (`Config`/`Admin` vs. `Reputation(Address)` vs.
`Invoice(u64)`/`InvoiceCount`), so there is no cross-enum collision risk.

| Enum | Variant | Stored Value | Purpose |
| ---- | ------- | ------------ | ------- |
| `ConfigKey` (`config.rs`) | `Config` | `Config` | Bonus/discount configuration |
| `ConfigKey` (`config.rs`) | `Admin` | `Address` | Contract administrator |
| `ReputationKey` (`reputation.rs`) | `Reputation(Address)` | `ReputationScore` | Per-address reputation counters |
| `InvoiceKey` (`invoice.rs`) | `Invoice(u64)` | `Invoice` | Invoice record |
| `InvoiceKey` (`invoice.rs`) | `InvoiceCount` | `u64` | Invoice counter |

---

## 5. `iln_distribution` — `StorageKey` (`contracts/iln_distribution/src/lib.rs`)

| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `Initialized` | `bool` | One-shot init guard |
| `IlnContract` | `Address` | Address of the `invoice_liquidity` contract allowed to accrue |
| `GovToken` | `Address` | Governance token distributed as rewards |
| `LpFundedVolume(Address)` | `i128` | Cumulative volume funded per LP |
| `FreelancerSettled(Address)` | `i128` | Cumulative settled volume per freelancer |
| `PayerOnTimeSettled(Address)` | `i128` | Cumulative on-time settled volume per payer |
| `Claimed(Address)` | `i128` | Cumulative rewards already claimed per address |

---

## 6. `iln_governance` — `StorageKey` (`contracts/iln_governance/src/lib.rs`)

| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `IlnContract` | `Address` | Target contract that executed proposals invoke into |
| `GovToken` | `Address` | Governance token used for voting weight |
| `MinQuorumBps` | `u32` | Minimum participation (bps of total supply) for a proposal to pass |
| `Proposal(u64)` | `GovernanceProposal` | Proposal record |
| `ProposalCount` | `u64` | Proposal id counter |
| `VoteWeightSnapshot(u64, Address)` | `i128` | Snapshotted vote weight per proposal/voter |
| `HasVoted(u64, Address)` | `bool` | Whether an address has voted on a proposal (temporary) |
| `VoteReceipt(u64, Address)` | `i128` | Durable mirror of the applied vote weight — fail-closed double-vote guard if the temporary receipt is archived (Issue #1099, persistent) |
| `VetoApprovals(u64)` | `Vec<Address>` | Per-proposal veto approvals (temporary mirror) |
| `VetoApprovalReceipt(u64, Address)` | `bool` | Durable per-signer veto approval flag; the veto tally is derived from these so an archived approval list cannot reset a partially-collected veto (Issue #1099, persistent) |
| `Delegation(Address)` | `Address` | Forward delegation pointer (Issue #64) |
| `Delegators(Address)` | `Vec<Address>` | Reverse delegation adjacency — direct predecessors of `Address`; required to enforce the DEL2 depth bound for every insertion order (Issue #1098) |
| `DelegatedToMe(Address)` | `i128` | Running tally of weight delegated to an address (Issue #64) |
| `ExecutionDelay` | `u32` | Timelock delay (in ledgers) applied between a proposal passing and executing (Issue #62) |
| `Admin` | `Address` | Admin address (veto power) |
| `VetoPowerEnabled` | `bool` | Whether admin veto power is currently active (Issue #68) |
| `MinProposalBalance` | `i128` | Minimum token balance required to create a proposal |

`VoteWeightSnapshot` and `HasVoted` are both 2-tuples of `(u64, Address)` but
remain distinguishable because they are different enum variants — the
variant name is always the first serialized element, so
`VoteWeightSnapshot(1, addr)` and `HasVoted(1, addr)` never collide.

---

## 7. Cross-Contract Interaction Notes

Contracts call into each other via `env.invoke_contract` / generated clients
(e.g. `iln_governance` → `invoice_liquidity`, `invoice_liquidity` →
`iln_distribution`, `invoice_liquidity` → `insurance_pool`/oracle). These are
ordinary cross-contract calls, not shared storage — the callee always reads
and writes its **own** storage, addressed by its own contract id. There is no
mechanism in Soroban by which one contract's storage read/write can target
another contract's storage, so cross-contract calls cannot introduce
collisions regardless of how similarly the two contracts name their keys.

## 8. Collision Prevention Strategy (summary)
- Every contract's storage keys are centralized in one (or a small, disjoint
  set of) `#[contracttype]` enum(s) — no raw `symbol_short!`/string keys are
  used for structured state.
- Enum variant names are unique within each contract's key space.
- Parameterized variants are disambiguated by argument type/value, and by
  variant name when two variants share an argument shape (e.g.
  `VoteWeightSnapshot(u64, Address)` vs `HasVoded(u64, Address)` in
  `iln_governance`).
- Storage is isolated per deployed contract address at the host level, so
  identical variant names across different contracts (`Admin`, `IlnContract`,
  `GovToken`, etc.) never collide.

## 9. Upgrade/Migration Notes
`invoice_liquidity` previously used separate `ConfigKey`/`StorageKey` enums
which were merged into the single `DataKey` enum. Variant names were
preserved during the merge, so existing serialized data remains
byte-compatible.

### 9.1 Purely additive governance keys (Issues #1098, #1099)
`iln_governance::StorageKey` gained two new variants, both **appended at the end
of the enum** so no existing variant's ordinal changes:

- `Delegators(Address)` (#1098) — reverse delegation adjacency.
- `VoteReceipt(u64, Address)` and `VetoApprovalReceipt(u64, Address)` (#1099) —
  durable mirrors of the temporary vote/veto receipts.

Because these are additive and read with fail-safe defaults (`unwrap_or(0)`,
`unwrap_or(false)`, or an empty `Vec`), a contract upgrade requires **no data
migration**: pre-existing proposals, votes, delegations, and veto state remain
valid, and the new keys are simply absent until the first write on the new code.
This follows the additive-change pattern accepted by the storage freeze in §10.1.


## 10. Storage Layout Freeze — Sign-off Gate (Issue #651)

The "Storage layout frozen" item on the
[Mainnet Launch Checklist](mainnet-launch-checklist.md) requires an explicit,
recorded sign-off — not just the absence of open storage PRs — before mainnet
deployment. This section is that gate.

### 10.1 What is frozen

Once frozen, no PR may add, remove, rename, reorder, or retype a variant in
any of the key enums documented in sections 2–6 of this document
(`invoice_liquidity::DataKey`, `insurance_pool::DataKey`,
`reputation_bonus::{ConfigKey, ReputationKey, InvoiceKey}`,
`iln_distribution::StorageKey`, `iln_governance::StorageKey`) without first:

1. Reopening this freeze via a PR that updates this section (unfreezing is an
   explicit, reviewed action, not something that happens implicitly).
2. Getting the same sign-offs listed in §10.2 again after the change.
3. Updating sections 2–6 above and, if the change is not purely additive,
   the migration guidance in [Upgrade Guide](upgrade-guide.md).

Purely additive changes that a future upgrade's `migrate()` entrypoint
defaults for existing records (the pattern used by the
[v1→v2 migration](upgrade-guide.md#v1--v2-state-migration-script--issue-114))
remain possible post-launch without breaking this freeze — the freeze covers
*silent, unmigrated* schema drift, not the supported upgrade path itself.

### 10.2 Sign-off

Mainnet deployment must not proceed until every row below is signed:

| Area | Maintainer | Signed off | Date | Notes |
|------|------------|------------|------|-------|
| Contracts (storage authoring) | TBD | No | TBD | Confirms sections 2–6 match the code that will be deployed to mainnet. |
| Security (collision/migration review) | TBD | No | TBD | Confirms section 1 and 7's collision-safety argument and section 9's migration notes hold for the deployed code. |
| Release (freeze enforcement) | TBD | No | TBD | Confirms no open PR touches a frozen enum at deployment time. |

### 10.3 Enforcement until launch

Between sign-off and mainnet deployment, any PR that touches a frozen key
enum's definition must link back to this section in its description and
explain why the freeze does not apply (e.g. it is testnet-only, or it follows
the reopen procedure in §10.1). Reviewers should treat an unexplained diff to
a frozen enum as a launch-blocking finding.
