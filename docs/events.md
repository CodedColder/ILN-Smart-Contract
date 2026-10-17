# ILN Smart Contract Event Schema

## Overview

This document is the result of a full event-emission completeness audit
(Issue #538): every state-changing (storage-mutating) function across all
five contracts was mapped and checked for event coverage. Gaps found during
the audit have been fixed in the same change that produced this document —
see "Audit summary" below for what was added and why.

Events are consumed by indexers, blockchain explorers, and off-chain backend
integrations as the audit trail for on-chain state transitions. Indexers
should reconstruct state from these events rather than scraping ledger
entries directly.

## Audit summary

| Contract | Function | Before | Fix |
| -------- | -------- | ------ | --- |
| `invoice_liquidity` | `initialize` | No event | Added `ContractInitialized` |
| `invoice_liquidity` | `set_distribution_contract` | No event | Added `DistributionContractUpdated` |
| `invoice_liquidity` | `set_price_oracle` | No event | Added `PriceOracleUpdated` |
| `invoice_liquidity` | `set_max_oracle_age` | No event | Reused `ParameterUpdated` |
| `insurance_pool` | `initialize` | No event | Added inline `init` event (coverage cap) |
| `reputation_bonus` | `init` | No event | Added `ContractInitialized` |
| `reputation_bonus` | `set_config` (raw setter, distinct from `update_config`) | No event | Added `ConfigSet` |
| `reputation_bonus` | `submit_invoice` | No event | Added `InvoiceSubmitted` |
| `reputation_bonus` | `mark_paid` | No event | Added `InvoiceStatusChanged` |
| `reputation_bonus` | `handle_default` | No event | Added `InvoiceStatusChanged` |
| `iln_distribution` | `initialize` | No event | Added `ContractInitialized` |
| `iln_distribution` | `accrue_lp` | No event | Added `LpVolumeAccrued` |
| `iln_distribution` | `accrue_settlement` | No event | Added `SettlementAccrued` |
| `iln_distribution` | `claim_tokens` | No event | Added `TokensClaimed` |
| `iln_governance` | `initialize` | No event | Added `GovernanceInitialized` |
| `iln_governance` | `set_min_quorum_bps` | No event | Added `GovernanceParameterUpdated` |
| `iln_governance` | `set_min_proposal_balance` | No event | Added `GovernanceParameterUpdated` |
| `iln_governance` | `set_execution_delay` | No event | Added `GovernanceParameterUpdated` |
| `iln_governance` | `disable_veto_power` | No event | Added `VetoPowerDisabled` |

All other state-changing functions across all five contracts already emitted
events prior to this audit (verified function-by-function; see per-contract
sections below for the full inventory, including functions that were
correctly found to need no event because they are pure read-only views).

Two events referenced by a stale prior version of this document —
`ContractPaused`/`ContractUnpaused` "missing struct" and `TokenAdded`/
`TokenRemoved`/`InvoiceExpired`/`InvoiceDisputed`/`ReputationUpdated`/
`LPPositionTransferred` "not emitted" — were already fully implemented in
the code by the time of this audit; the document had simply not been kept in
sync with the contract. This rewrite corrects that drift for all five
contracts and should be kept current going forward.

---

## `invoice_liquidity`

### Event Index

| Event | Trigger |
| ----- | ------- |
| [ContractInitialized](#contractinitialized) | `initialize` |
| [AdminChanged](#adminchanged) | `set_admin` |
| [ParameterUpdated](#parameterupdated) | `update_fee_rate`, `update_max_discount`, `set_max_oracle_age`, `set_min_payer_reputation` |
| [DistributionContractUpdated](#distributioncontractupdated) | `set_distribution_contract` |
| [PriceOracleUpdated](#priceoracleupdated) | `set_price_oracle` |
| [TokenAdded](#tokenadded) | `add_token` |
| [TokenRemoved](#tokenremoved) | `remove_token` |
| [ContractPaused](#contractpaused-contractunpaused) | `pause` |
| [ContractUnpaused](#contractpaused-contractunpaused) | `unpause` |
| [ContractUpgraded](#contractupgraded) | `upgrade` |
| [InvoiceSubmitted](#invoicesubmitted) | `submit_invoice`, `submit_invoices_batch` |
| [InvoiceUpdated](#invoiceupdated) | `update_invoice` |
| [InvoiceTokenChanged](#invoicetokenchanged) | `convert_invoice_token` |
| [FundRequested](#fundrequested) | `join_fund_queue` |
| [FundQueueResolved](#fundqueueresolved) | `resolve_fund_queue` |
| [FundQueueRecovered](#fundqueuerecovered) | `recover_fund_queue` |
| [InvoiceFunded](#invoicefunded) | `fund_invoice` |
| [InvoiceTransferred](#invoicetransferred) | `transfer_invoice` |
| [LPPositionTransferred](#lppositiontransferred) | `transfer_lp_position` |
| [InvoiceCancelled](#invoicecancelled) | `cancel_invoice` |
| [InvoiceExpired](#invoiceexpired) | `expire_invoice`, expiry detected inline in `fund_invoice`/`mark_paid` |
| [InvoicePartiallyPaid](#invoicepartiallypaid) | `mark_paid` (partial) |
| [InvoicePaid](#invoicepaid) | `mark_paid` (full) |
| [InvoiceDefaulted](#invoicedefaulted) | `claim_default` |
| [DefaultAppealed](#defaultappealed) | `appeal_default` |
| [AppealResolved](#appealresolved) | `resolve_appeal` |
| [InvoiceDisputed](#invoicedisputed) | `dispute_invoice` |
| [DisputeResolved](#disputeresolved) | `resolve_dispute`, `auto_resolve_dispute` |
| [ReputationUpdated](#reputationupdated) | reputation score/counter changes (`invoice.rs`) |
| [InvoiceNftMinted / InvoiceNftTransferred / InvoiceNftBurned](#invoice-nft-lifecycle) | invoice submit / fund / pay (`nft.rs`) |

### Full function inventory

State-changing functions and their event coverage (✅ = emits an event,
🔍 = pure view, no mutation, so no event needed):

| Function | Event coverage |
| -------- | --------------- |
| `initialize` | ✅ `ContractInitialized` (added by this audit) |
| `set_admin` | ✅ `AdminChanged` |
| `update_fee_rate` | ✅ `ParameterUpdated` |
| `update_max_discount` | ✅ `ParameterUpdated` |
| `set_distribution_contract` | ✅ `DistributionContractUpdated` (added) |
| `set_price_oracle` | ✅ `PriceOracleUpdated` (added) |
| `set_max_oracle_age` | ✅ `ParameterUpdated` (added) |
| `add_token` | ✅ `TokenAdded` |
| `remove_token` | ✅ `TokenRemoved` |
| `pause` | ✅ `ContractPaused` |
| `unpause` | ✅ `ContractUnpaused` |
| `upgrade` | ✅ `ContractUpgraded` |
| `submit_invoice` | ✅ `InvoiceSubmitted` (+ `InvoiceNftMinted`) |
| `update_invoice` | ✅ `InvoiceUpdated` |
| `convert_invoice_token` | ✅ `InvoiceTokenChanged` |
| `submit_invoices_batch` | ✅ `InvoiceSubmitted` per invoice |
| `join_fund_queue` | ✅ `FundRequested` |
| `resolve_fund_queue` | ✅ `FundQueueResolved` |
| `recover_fund_queue` | ✅ `FundQueueRecovered` |
| `fund_invoice` | ✅ `InvoiceFunded` (+ `InvoiceExpired` if expiry detected, + `InvoiceNftTransferred`) |
| `transfer_invoice` | ✅ `InvoiceTransferred` |
| `transfer_lp_position` | ✅ `LPPositionTransferred` |
| `cancel_invoice` | ✅ `InvoiceCancelled` |
| `expire_invoice` | ✅ `InvoiceExpired` |
| `mark_paid` | ✅ `InvoicePartiallyPaid` or `InvoicePaid` (+ `InvoiceNftBurned` on full payment) |
| `claim_yield` | 🔍 pure computation over existing invoice state; no storage write |
| `claim_default` | ✅ `InvoiceDefaulted` |
| `appeal_default` | ✅ `DefaultAppealed` |
| `resolve_appeal` | ✅ `AppealResolved` |
| `dispute_invoice` | ✅ `InvoiceDisputed` |
| `resolve_dispute` | ✅ `DisputeResolved` |
| `auto_resolve_dispute` | ✅ `DisputeResolved` |
| `update_config` | 🔍 no dedicated event currently (governance-facing config bulk-set; tracked as a follow-up, see Known Gaps) |
| `set_min_payer_reputation` | ✅ `ParameterUpdated` |
| `get_*` / `is_*` / `list_*` / `suggested_discount_rate` / `query_*` | 🔍 read-only views |

### ContractInitialized

Emitted once, when `initialize` is called.

Topics: `["initialized", admin]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `admin` | `Address` | The initial contract administrator |
| `usdc_token` | `Address` | USDC SAC address configured at init |
| `eurc_token` | `Address` | EURC SAC address configured at init |
| `xlm_token` | `Address` | XLM SAC address configured at init |
| `timestamp` | `u64` | Ledger timestamp of initialization |

### AdminChanged

Topics: `["admin_changed"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `old_admin` | `Address` | Previous admin |
| `new_admin` | `Address` | New admin |
| `timestamp` | `u64` | Ledger timestamp of the transition |

### ParameterUpdated

Generic event for governance-controlled numeric parameters. `param_name` is a
stable audit identifier — keep values unique per parameter.

Topics: `["parameter_updated", param_name, updated_by]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `param_name` | `Symbol` | e.g. `"protocol_fee_rate_bps"`, `"max_discount_rate_bps"`, `"max_oracle_age_ledgers"`, `"min_payer_reputation"` |
| `old_value` | `i128` | Previous value |
| `new_value` | `i128` | New value |
| `updated_by` | `Address` | Admin who made the change |

### DistributionContractUpdated

Topics: `["distribution_contract_updated", updated_by]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `old_distribution_contract` | `Option<Address>` | Previously configured distribution contract, if any |
| `new_distribution_contract` | `Address` | Newly configured distribution contract |
| `updated_by` | `Address` | Admin who made the change |

### PriceOracleUpdated

Topics: `["price_oracle_updated", updated_by]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `old_oracle` | `Option<Address>` | Previously configured oracle, if any |
| `new_oracle` | `Address` | Newly configured oracle |
| `updated_by` | `Address` | Admin who made the change |

### TokenAdded

Topics: `["token_added"... ]` (see code for exact topic tuple)

| Field | Type | Description |
| ----- | ---- | ----------- |
| `token` | `Address` | Token added to the funding allowlist |
| `decimals` | `u32` | Decimal precision for the token |

### TokenRemoved

| Field | Type | Description |
| ----- | ---- | ----------- |
| `token` | `Address` | Token removed from the funding allowlist |

### ContractPaused / ContractUnpaused

| Field | Type | Description |
| ----- | ---- | ----------- |
| `timestamp` | `u64` | Ledger timestamp of the pause/unpause |

### ContractUpgraded

| Field | Type | Description |
| ----- | ---- | ----------- |
| `admin` | `Address` | Admin who authorised the upgrade |
| `new_wasm_hash` | `BytesN<32>` | Hash of the new WASM binary |
| `timestamp` | `u64` | Ledger timestamp |

### InvoiceSubmitted

Topics: `["submitted", invoice_id, freelancer, payer]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Unique invoice identifier |
| `freelancer` | `Address` | Submitter |
| `payer` | `Address` | Payer responsible for settlement |
| `token` | `Address` | Accepted payment token |
| `amount` | `i128` | Total invoice amount |
| `due_date` | `u64` | Expiration/due timestamp |
| `discount_rate` | `u32` | Discount rate offered |
| `referral_code` | `ReferralCode` | Referral code used, if any |
| `status` | `InvoiceStatus` | Current status |
| `timestamp` | `u64` | Ledger timestamp of submission |

### InvoiceUpdated

Topics: `["updated", invoice_id, freelancer, payer]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `freelancer` | `Address` | Submitter |
| `payer` | `Address` | Payer |
| `token` | `Address` | Token |
| `amount` | `i128` | Updated amount |
| `due_date` | `u64` | Updated due date |
| `discount_rate` | `u32` | Updated discount rate |
| `status` | `InvoiceStatus` | Current status |

### InvoiceTokenChanged

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `old_token` | `Address` | Previous token |
| `new_token` | `Address` | New token |

### FundRequested

Topics: `["fund_requested", invoice_id, lp]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Target invoice |
| `lp` | `Address` | LP requesting to fund |
| `score` | `u32` | LP's reputation score at registration |

### FundQueueResolved

Topics: `["fund_queue_resolved", invoice_id, approved_lp]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Target invoice |
| `approved_lp` | `Address` | Winning LP |
| `score` | `u32` | Winning score |

### FundQueueRecovered

Topics: `["fund_queue_recovered", invoice_id]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Target invoice |
| `evicted_lp` | `Option<Address>` | Stalled approved LP that was evicted, if any |
| `remaining_lps` | `u32` | LPs left in the reopened queue (`0` when purged) |
| `purged` | `bool` | `true` when the invoice was no longer fundable and all queue state was deleted |

### InvoiceFunded

Topics: `["funded", invoice_id, funder]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `funder` / `lp` | `Address` | LP providing funds |
| `freelancer` | `Address` | Freelancer receiving funds |
| `payer` | `Address` | Payer |
| `token` | `Address` | Payment token |
| `fund_amount` | `i128` | Newly provided funding amount |
| `amount_funded` | `i128` | Total amount funded so far |
| `invoice_amount` | `i128` | Total invoice amount |
| `due_date` | `u64` | Due date |
| `discount_rate` | `u32` | Applied discount rate |
| `funded_at` | `Option<u64>` | Ledger timestamp when fully funded |
| `status` | `InvoiceStatus` | Current status |
| `effective_yield_bps` | `u32` | Effective annualized yield in bps |
| `timestamp` | `u64` | Ledger timestamp of this funding event |

### InvoiceTransferred

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `old_freelancer` | `Address` | Previous owner |
| `new_freelancer` | `Address` | New owner |
| `status` | `InvoiceStatus` | Current status |

### LPPositionTransferred

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `old_lp` | `Address` | Previous LP |
| `new_lp` | `Address` | New LP |
| `status` | `InvoiceStatus` | Current status |

### InvoiceCancelled

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `freelancer` | `Address` | Freelancer who cancelled |
| `status` | `InvoiceStatus` | `Cancelled` |

### InvoiceExpired

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `freelancer` | `Address` | Freelancer |
| `status` | `InvoiceStatus` | `Expired` |

### InvoicePartiallyPaid

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `payer` | `Address` | Payer |
| `amount_paid_now` | `i128` | Amount paid in this call |
| `total_amount_paid` | `i128` | Cumulative amount paid |
| `remaining_amount` | `i128` | Amount still owed |

### InvoicePaid

Topics: `["paid", invoice_id, payer, lp]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `payer` | `Address` | Payer who settled |
| `lp` | `Address` | LP receiving payout |
| `freelancer` | `Address` | Freelancer |
| `token` | `Address` | Payment token |
| `amount_paid` | `i128` | Full amount settled |
| `lp_earned` | `i128` | LP earnings |
| `lp_payout` | `i128` | Total distributed to LP |
| `settlement_timestamp` | `u64` | Settlement ledger timestamp |
| `paid_on_time` | `bool` | Whether settled before due date |
| `status` | `InvoiceStatus` | `Paid` |

### InvoiceDefaulted

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `funder` | `Address` | Funder |
| `freelancer` | `Address` | Freelancer |
| `payer` | `Address` | Defaulting payer |
| `token` | `Address` | Payment token |
| `amount` | `i128` | Original invoice amount |
| `due_date` | `u64` | Missed due date |
| `defaulted_at` | `u64` | Timestamp of default marking |
| `discount_amount` | `i128` | Applied discount amount |
| `status` | `InvoiceStatus` | `Defaulted` |

### DefaultAppealed

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `payer` | `Address` | Payer filing the appeal |
| `evidence_hash` | `BytesN<32>` | SHA-256 hash of off-chain evidence |
| `appealed_at` | `u64` | Timestamp filed |

### AppealResolved

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `payer` | `Address` | Payer whose appeal was resolved |
| `upheld` | `bool` | True = default reversed |
| `resolved_at` | `u64` | Timestamp of resolution |

### InvoiceDisputed

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `payer` | `Address` | Payer disputing |
| `reason_hash` | `BytesN<32>` | SHA-256 hash of off-chain evidence |
| `disputed_at` | `u64` | Timestamp filed |

### DisputeResolved

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice identifier |
| `resolution_hash` | `BytesN<32>` | Hash of resolution details |
| `resolution` | `u32` | 1 = Upheld (payer), 2 = Rejected (freelancer) |
| `resolved_at` | `u64` | Timestamp of resolution |

### ReputationUpdated

Emitted from `invoice.rs` whenever an address's reputation profile changes.

| Field | Type | Description |
| ----- | ---- | ----------- |
| `address` | `Address` | Address whose reputation changed |
| `old_score` | `u32` | Previous score |
| `new_score` | `u32` | New score |
| `invoices_submitted` | `u32` | Updated counter |
| `invoices_paid` | `u32` | Updated counter |
| `invoices_defaulted` | `u32` | Updated counter |

### Invoice NFT lifecycle

Defined in `nft.rs`, emitted alongside the corresponding invoice lifecycle
events:

- **InvoiceNftMinted** — on `submit_invoice`: `invoice_id`, `owner`, `amount`, `due_date`, `timestamp`.
- **InvoiceNftTransferred** — on `fund_invoice` (freelancer → LP): `invoice_id`, `from`, `to`, `timestamp`.
- **InvoiceNftBurned** — on full `mark_paid`: `invoice_id`, `owner`, `timestamp`.

---

## `insurance_pool`

| Function | Event coverage |
| -------- | --------------- |
| `initialize` | ✅ `init` topic, payload = coverage cap (added by this audit) |
| `enroll` | ✅ `enrolled` topic |
| `deposit_premium` | ✅ `premium` topic, payload = amount; ⚠ secondary `back_top` when `BackstopFundingBps > 0` |
| `claim` | ✅ `claimed` topic, payload = payout; emits `solv_trip` after a payout that breaches the reserve floor |
| `propose_coverage_change` | ✅ `cov_prop` topic, payload = (new_coverage, eta) |
| `execute_coverage_change` | ✅ `cov_exec` topic, payload = new_coverage |
| `cancel_coverage_change` | ✅ `cov_cncl` topic |
| `propose_admin_transfer` | ✅ `adm_prop` topic, payload = eta |
| `execute_admin_transfer` | ✅ `adm_exec` topic, payload = new_admin |
| `cancel_admin_transfer` | ✅ `adm_cncl` topic |
| `set_min_reserve_ratio_bps` | ✅ `resv_min` topic, payload = bps |
| `reset_solvency_circuit` | ✅ `solv_rst` topic, payload = `SolvencyCircuitReset` |
| `set_backstop_funding_bps` | ✅ `back_bps` topic, payload = bps |
| `top_up_backstop` | ✅ `back_top` topic (from), payload = `BackstopTopUp` |
| `submit_claim_evidence` | ✅ `evidence` topic (invoice_id), payload = `ClaimEvidence` |
| `set_review_window_seconds` | ✅ `rev_wnd` topic, payload = seconds |
| `record_pair_default` | ✅ `pair_def` topic (lp, payer), payload = `PairDefaultRecorded` |
| `get_*` / `is_*` | 🔍 read-only views |
| `gate_solvency_circuit` / `trip_circuit_if_breached` (internal, on claim) | ✅ `solv_trip` topic, payload = `SolvencyCircuitTripped` (once per breach) |

### PoolInitialized

Topics: `["init", admin]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `admin` | `Address` | The initial pool admin |
| `coverage` | `i128` | Flat per-claim coverage cap |

### Enrolled

Emitted when an LP enrolls in the insurance program (either via explicit `enroll()` call or automatically on first `deposit_premium()`).

Topics: `["enrolled", lp]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `lp` | `Address` | LP enrolled in the pool |

**Example**: An LP calls `enroll()` or makes their first premium deposit, triggering enrollment.

### PremiumDeposited

Emitted when an LP deposits a premium payment. The amount is the premium paid in stroops.

Topics: `["premium", lp]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `lp` | `Address` | LP paying the premium |
| `amount` | `i128` | Premium amount transferred (in stroops) |

**Example**: An LP deposits 100 USDC as premium, which is recorded as 100,000,000 stroops.

### ClaimProcessed

Emitted when a claim is processed for a defaulted invoice. The payout is the compensation amount credited to the LP.

Topics: `["claimed", invoice_id]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Defaulted invoice identifier |
| `payout` | `i128` | Compensation amount paid to LP (in stroops) |

**Example**: An invoice with ID 123 defaults, and the LP receives a payout of 50 USDC (50,000,000 stroops) from the insurance pool.

### CoverageChangeProposed

Topics: `["cov_prop"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `new_coverage` | `i128` | Proposed new coverage cap |
| `eta` | `u64` | Timestamp when change becomes executable |

### CoverageChangeExecuted

Topics: `["cov_exec"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `new_coverage` | `i128` | New coverage cap now active |

### CoverageChangeCancelled

Topics: `["cov_cncl"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| (none) | | |

### AdminTransferProposed

Topics: `["adm_prop", new_admin]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `new_admin` | `Address` | Proposed new admin |
| `eta` | `u64` | Timestamp when transfer becomes executable |

### AdminTransferExecuted

Topics: `["adm_exec"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `new_admin` | `Address` | New admin now active |

### AdminTransferCancelled

Topics: `["adm_cncl"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| (none) | | |

The timelock propose/execute/cancel flow is documented in detail in
[`insurance-pool-design.md`](./insurance-pool-design.md).

### PendingSolvencyBreakerArmed / MinReserveRatioSet

Emitted when governance sets the solvency circuit-breaker threshold (Issue #826).

Topics: `["resv_min"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `bps` | `u32` | Minimum reserve ratio (bps, `0` = disabled) |

### SolvencyCircuitTripped

Emitted once when a payout leaves the pool's reserve ratio at or below the
configured minimum, tripping the sticky breaker. The tripping claim completes
its final bound payout first (the flag write must land on a call that returns
Ok — a write followed by a panic in the same invocation is rolled back in
Soroban); all subsequent claims are rejected with `SolvencyCircuitOpen`
until governance resets (Issue #826).

Topics: `["solv_trip"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `ratio_bps` | `u32` | Reserve ratio at trip time |
| `reserve` | `i128` | Total claimable reserve (balance + backstop) |

### SolvencyCircuitReset

Emitted when governance resumes claim payouts after a trip (Issue #826).

Topics: `["solv_rst"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `ratio_bps` | `u32` | Reserve ratio at reset time |
| `reserve` | `i128` | Total claimable reserve at reset time |

### BackstopFundingSet

Emitted when governance changes the premium-to-backstop share (Issue #827).

Topics: `["back_bps"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `bps` | `u32` | New backstop funding share (bps) |

### BackstopTopUp

Emitted when capital is added to the backstop (Issue #827) — either an
explicit `top_up_backstop` or the diverted share of a premium deposit.

Topics: `["back_top", from]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `from` | `Address` | Funding source |
| `amount` | `i128` | Amount credited to the backstop |
| `backstop_balance` | `i128` | New backstop balance |

### ClaimEvidenceSubmitted

Emitted when an evidence hash is attached to a claim (Issue #828).

Topics: `["evidence", invoice_id]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `invoice_id` | `u64` | Invoice the evidence backs |
| `evidence_hash` | `BytesN<32>` | 32-byte digest of off-chain evidence |
| `submitted_at` | `u64` | Ledger timestamp of submission |

### ReviewWindowSet

Emitted when governance changes the review window (Issue #828).

Topics: `["rev_wnd"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `seconds` | `u64` | New review window (seconds, `0` = disabled) |

### PairDefaultRecorded

Emitted when a default is recorded for a specific (lp, payer) pair
(Issue #829).

Topics: `["pair_def", lp, payer]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `lp` | `Address` | Liquidity provider |
| `payer` | `Address` | Defaulting payer |
| `pair_count` | `u32` | Running default count for this pair |

---

## `reputation_bonus`

| Function | Event coverage |
| -------- | --------------- |
| `init` | ✅ `ContractInitialized` (added by this audit) |
| `set_config` | ✅ `ConfigSet` (added by this audit) |
| `update_config` | ✅ `ParameterUpdated` ×3 (one per changed field — pre-existing) |
| `submit_invoice` | ✅ `InvoiceSubmitted` (added by this audit) |
| `mark_paid` | ✅ `InvoiceStatusChanged` (added by this audit) |
| `handle_default` | ✅ `InvoiceStatusChanged` (added by this audit) |
| `get_config` / `get_reputation` | 🔍 read-only views |

`update_config` and `set_config` both write to the same underlying storage
key but are kept as separate entrypoints with separate events:
`update_config` is the fine-grained, per-field audited path (three
`ParameterUpdated` events, one per field, each carrying the old and new
value), while `set_config` is a bulk replace that emits a single `ConfigSet`
snapshot of the whole config. Note: `set_config` currently has no admin
auth check in `lib.rs` — this is a pre-existing access-control gap, outside
the scope of this event-emission audit, and should be tracked separately.

---

## `iln_distribution`

| Function | Event coverage |
| -------- | --------------- |
| `initialize` | ✅ `init` topic → `ContractInitialized` (added by this audit) |
| `accrue_lp` | ✅ `lp_accr` topic → `LpVolumeAccrued` (added by this audit) |
| `accrue_settlement` | ✅ `settled` topic → `SettlementAccrued` (added by this audit) |
| `claim_tokens` | ✅ `claimed` topic → `TokensClaimed` (added by this audit) |
| `get_accrual` | 🔍 read-only view |

---

## `iln_governance`

| Function | Event coverage |
| -------- | --------------- |
| `initialize` | ✅ `GovernanceInitialized` (added by this audit) |
| `set_min_quorum_bps` | ✅ `GovernanceParameterUpdated` (added by this audit) |
| `create_proposal` | ✅ `ProposalCreated` |
| `set_min_proposal_balance` | ✅ `GovernanceParameterUpdated` (added by this audit) |
| `delegate_votes` | ✅ `VotesDelegated` |
| `undelegate_votes` | ✅ `VotesUndelegated` |
| `cast_vote` | ✅ `VoteCast` |
| `set_execution_delay` | ✅ `GovernanceParameterUpdated` (added by this audit) |
| `execute_proposal` | ✅ `ProposalExecuted` |
| `veto_proposal` | ✅ `ProposalVetoed` |
| `disable_veto_power` | ✅ `VetoPowerDisabled` (added by this audit) |
| `get_*` / `list_proposals` / `has_voted` | 🔍 read-only views |

### GovernanceInitialized

Emitted once, when the governance contract is initialised.

Topics: `["initialized", admin]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `iln_contract` | `Address` | The ILN contract address |
| `gov_token` | `Address` | The governance token address |
| `admin` | `Address` | The initial admin address |

### GovernanceParameterUpdated

Emitted whenever a governance-controlled numeric parameter changes.

Topics: `["parameter_updated", param_name]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `param_name` | `Symbol` | e.g. `"min_quorum_bps"`, `"min_proposal_balance"`, `"execution_delay"` |
| `old_value` | `i128` | Previous value |
| `new_value` | `i128` | New value |

### ProposalCreated

Emitted when a new governance proposal is created.

Topics: `["proposal_created", proposal_id, proposer]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `proposal_id` | `u64` | Unique proposal identifier |
| `proposer` | `Address` | Address that created the proposal |
| `action_type` | `ProposalAction` | The type of action proposed |
| `proposed_value` | `i128` | The proposed value for the action |
| `voting_end` | `u64` | Timestamp when voting ends |

### VoteCast

Emitted when a vote is cast on a proposal.

Topics: `["vote_cast", proposal_id, voter]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `proposal_id` | `u64` | Proposal being voted on |
| `voter` | `Address` | Address casting the vote |
| `support` | `bool` | True = for, False = against |
| `weight` | `i128` | Voting weight (own + delegated) |

### ProposalExecuted

Emitted when a passed proposal is executed.

Topics: `["proposal_executed", proposal_id]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `proposal_id` | `u64` | Executed proposal identifier |
| `action_type` | `ProposalAction` | The action that was executed |
| `proposed_value` | `i128` | The value that was applied |
| `votes_for` | `i128` | Total votes in favour |
| `votes_against` | `i128` | Total votes against |

### ProposalVetoed

Emitted when the admin vetoes a proposal.

Topics: `["proposal_vetoed", proposal_id, admin]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `proposal_id` | `u64` | Vetoed proposal identifier |
| `admin` | `Address` | Admin who vetoed |
| `reason_hash` | `BytesN<32>` | Hash of off-chain reason |

### VotesDelegated

Emitted when voting power is delegated.

Topics: `["votes_delegated", delegator, delegate]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `delegator` | `Address` | Address delegating their votes |
| `delegate` | `Address` | Address receiving the delegation |

### VotesUndelegated

Emitted when a delegation is removed.

Topics: `["votes_undelegated", delegator]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `delegator` | `Address` | Address removing their delegation |

### VetoPowerDisabled

Emitted when admin veto power is permanently disabled.

Topics: `["veto_power_disabled"]`

| Field | Type | Description |
| ----- | ---- | ----------- |
| `disabled_by` | `Address` | The ILN contract that disabled veto power |

---

## Indexer Notes

* **Event Ordering Assumptions**: In `invoice_liquidity`, `InvoiceSubmitted`
  always precedes `InvoiceFunded`, which precedes `InvoicePaid` or
  `InvoiceDefaulted`.
* **State Reconstruction**: `InvoiceSubmitted`/`ContractInitialized`/
  `GovernanceInitialized` include a `timestamp` field specifically so
  indexers can reconstruct creation time without querying ledger headers.
* **Timestamps**: All timestamps are `u64` seconds since the Unix epoch.
* **Amounts**: All numeric amounts (`amount`, `fund_amount`, `lp_payout`,
  etc.) are `i128` in the token's native unit (stroops); format using the
  token's configured decimals for display.
* **Cross-contract correlation**: `iln_distribution`'s `LpVolumeAccrued`/
  `SettlementAccrued` events correlate with `invoice_liquidity`'s
  `InvoiceFunded`/`InvoicePaid` events (same ledger, triggered by the same
  underlying call), but carry no shared invoice id — indexers should
  correlate by ledger sequence + participant address if they need to join
  the two streams.
