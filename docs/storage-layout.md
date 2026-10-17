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
| `FundQueue(u64)` | `Vec<LpFundRequest>` | LPs waiting to fund an invoice. TTL kept alive with the other queue records (see below) |
| `QueueResolution(u64)` | `Address` | Selected LP from a funding queue. TTL kept alive with the other queue records |
| `FundQueueOpenedAt(u64)` | `u32` | Ledger the first LP joined; anchors the MEV maturity delay. TTL kept alive with the other queue records |

**Fund-queue TTL (Issues #1009 / #1010).** `FundQueue`, `FundQueueOpenedAt` and
`QueueResolution` are persistent entries that are read at different times
(join, resolve, and a deferred `fund_invoice`). Every write *and* every read of
any of them now calls `extend_fund_queue_ttl`, which bumps all existing sibling
entries to `QUEUE_TTL_EXTEND_TO_LEDGERS` (2,000,000) whenever their remaining
TTL is at or below `QUEUE_TTL_THRESHOLD_LEDGERS` (1,000,000) — the same window
`save_invoice` gives the invoice. No new keys or per-read writes beyond an
extension that is skipped while TTL is above the threshold; entries for queues
that are never used are never created.
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
| `HasVoted(u64, Address)` | `bool` | Whether an address has voted on a proposal |
| `Delegation(Address)` | `Address` | Forward delegation pointer (Issue #64) |
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
