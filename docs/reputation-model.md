# Reputation Model — Invoice Liquidity Network

## Overview

The reputation model is designed to reduce risk for liquidity providers (LPs) by tracking the historical behavior of participants in the network.

It assigns implicit credibility to:

* Payers (clients)
* Freelancers (invoice originators)

---

## Core Principles

### 1. Payer Reliability (Primary Risk Factor)

The most important signal in the system is:

> **Does the payer settle invoices on time?**

Each payer accumulates:

* Total invoices paid
* Total invoices defaulted
* Average payment delay

#### Derived Score

```
payer_score = paid_invoices / total_invoices
```

Enhancements:

* Time-weighted scoring
* Penalty for defaults
* Bonus for early payments

---

### 2. Freelancer Credibility

Freelancers are evaluated based on:

* % of invoices successfully funded
* % of invoices that defaulted
* Historical volume

This prevents:

* Fake invoices
* Low-quality counterparties

---

### 3. LP Risk Assessment

LPs use both scores:

```
risk = f(payer_score, freelancer_score, discount_rate)
```

Where:

* Higher discount_rate = higher perceived risk
* Lower payer_score = higher risk

---

## On-Chain vs Off-Chain

### On-Chain (Current Contract)

* Invoice lifecycle (Pending → Funded → Paid / Defaulted)
* Payment history
* Default events

### Off-Chain (Recommended)

* Score computation
* Risk dashboards
* LP decision engines

---

## Full Lifecycle Guide (Issues #854 — single cross-referenced narrative)

Reputation logic is currently split across
[`invoice_liquidity`](reputation.md) (protocol source of truth),
[`reputation_bonus`](adr/ADR-011-reputation-state-source-of-truth.md) (standalone
discount module), [ADR-004](adr/ADR-004-lazy-reputation-decay.md) (lazy decay),
[ADR-007](adr/adr-007-nft-invoice-representation.md) (NFT claims), and
[ADR-011](adr/ADR-011-reputation-state-source-of-truth.md) (source of truth).
This section ties them into one lifecycle an auditor or grant reviewer can
follow end to end.

```ascii
Address joins ──▶ INIT 50/50 (lazy, nothing stored)
    │
    ├─ submit_invoice ──▶ snapshot freelancer score into Invoice
    │                     + invoices_submitted (freelancer profile)
    │                     + NFT: none yet (no claim exists)
    │
    ├─ join_fund_queue ─▶ snapshot LP score into LpFundRequest (ordering)
    │
    ├─ fund_invoice ────▶ LP score +1 + NFT MINT (owner = funder / lead LP)
    │                     + min-payer-reputation gate enforced
    │
    ├─ mark_paid ───────▶ payer score +1 (cap 100)
    │                     + invoices_paid (payer + freelancer)
    │                     + NFT BURN (claim settled)
    │                     + distribution settlement accrual
    │
    ├─ claim_default ───▶ save pre_default_score; payer score −5 (floor 0)
    │                     + invoices_defaulted (payer)
    │                     + NFT KEPT (Defaulted still holds the claim)
    │
    ├─ appeal_default / resolve_appeal ─▶ upheld: restore pre_default_score
    │                                     rejected: penalty stands
    │
    └─ any read after silence ─▶ LAZY DECAY toward 0 (persisted on read)
```

### 1. Score initialization

Every address starts at **50/50** (`DEFAULT_PAYER_SCORE` /
`DEFAULT_LP_SCORE` in `contracts/invoice_liquidity/src/constants.rs`).
Initialization is **lazy**: no `PayerScore`/`LpScore` entry is persisted
until the score first differs from 50, and `set_payer_score` *removes* the
entry when the score returns to 50 — unknown addresses simply read as 50
(`invoice.rs: get_payer_score`, `get_lp_score`). The richer
`ReputationProfile` counters (`invoices_submitted/paid/defaulted`) are
likewise zero-initialized and unpersisted until first use
(`get_reputation` returns a zeroed profile for unknown addresses).

### 2. Accrual on settlement

| Transition | Score effect | Counters | Code |
|------------|--------------|----------|------|
| `submit_invoice` | none (snapshots freelancer's current score into `Invoice.submitter_reputation`) | `invoices_submitted +1` (freelancer) | `lib.rs` submit paths |
| `join_fund_queue` | none (snapshots LP score into `LpFundRequest` for queue ordering) | — | `lib.rs` queue docs |
| `fund_invoice` | LP `+1` (cap 100) | — | `lib.rs` fund path |
| `mark_paid` | payer `+1` (cap 100, `saturating_add`) | `invoices_paid +1` (payer **and** freelancer) | `lib.rs` settle path |
| `claim_default` | payer `−5` (`> 5 ? −5 : 0`, floor 0); pre-penalty score saved via `save_pre_default_payer_score` | `invoices_defaulted +1` (payer) | `lib.rs` default path |
| `resolve_appeal` (upheld) | payer restored to `pre_default_score` | — | `lib.rs` appeal path |
| `resolve_appeal` (rejected) | penalty stands | — | `lib.rs` appeal path |

Every score/counter change syncs the `ReputationProfile` and emits
`reputation_updated` (`invoice.rs: set_reputation`), and score changes feed
`top_payers` leaderboards. Funding additionally requires
`get_payer_score(payer) >= min_payer_reputation` when that gate is non-zero
(Issue #28).

### 3. Lazy decay

Per [ADR-004](adr/ADR-004-lazy-reputation-decay.md), decay is **lazy**: each
score stores `(value, last_activity_ledger)` and `get_payer_score` /
`get_lp_score` apply `periods = elapsed / decay_period_ledgers` rounds of
`score −= max(score·rate_bps/10_000, 1)` on read — **persisting** the decayed
value, the new activity ledger, the synced profile, and a
`reputation_updated` event. Guards: decay applies only after a full period
has elapsed; iteration is capped at `MAX_REPUTATION_DECAY_PERIODS = 1000`
periods (beyond that the score is 0, Issue #601); a minimum 1-point decay per
period guarantees convergence to 0. Defaults: `decay_rate_bps = 50` (0.5%),
`decay_period_ledgers = 10000`; both are governable via
`update_decay_params` directly or through a governance
`UpdateDecayParams(rate_bps, period_ledgers)` proposal.

### 4. NFT transfer interaction

Per [ADR-007](adr/adr-007-nft-invoice-representation.md) as now wired via
`nft::sync_nft_state`, called from every invoice write path in `lib.rs`: an
NFT exists **iff** the invoice is `Funded`, `PartiallyFunded`, `Defaulted`,
`Appealed`, or `Disputed`. It is **minted on funding** (owner = funder, or
the lead LP for partial funding), **transferred** when the holder changes,
and **burned** on settlement (`Paid`), cancellation, refund, or expiry.
Metadata (`amount`, `due_date`, `discount_rate`, `token`) is self-contained
so marketplaces can price claims without calling back; lifecycle events
(`invoice_nft_minted` / `transferred` / `burned`) and queries
(`query_nft_metadata`, `query_nft_owner`, SDK `getNftMetadata`) expose it.
Reputation and NFT state never contradict: the NFT says *who holds the
claim*, the score says *how the payer behaved* — funding/settlement handlers
update both atomically in the same transaction.

### 5. Source-of-truth resolution

Per [ADR-011](adr/ADR-011-reputation-state-source-of-truth.md) there is **no
shared reputation ledger**:

| Question | Answer |
|----------|--------|
| Protocol funding, defaults, appeals, min-payer gates | **Only** `invoice_liquidity` scores |
| Discount bonus on bonus-module invoices | **Only** `reputation_bonus` scores (own counters, own decay-free math) |
| Same address, different scores in the two contracts | Expected — label the source in UIs |
| Governance `UpdateReputationBonusParams` | Touches bonus-module **parameters** only, never ILN scores |

---

## Future Extensions

* ~~NFT-based reputation badges~~ — done via invoice-claim NFTs ([§4](#4-nft-transfer-interaction)); reputation *badges* (soulbound per-score tiers) remain future work
* Credit delegation
* Dynamic discount pricing based on score
* ZK-based private credit scoring

---

## Cross-references

* [`reputation.md`](reputation.md) — full mechanics, parameters, events, FAQ
* [ADR-004](adr/ADR-004-lazy-reputation-decay.md) — why decay is lazy
* [ADR-007](adr/adr-007-nft-invoice-representation.md) — NFT data model and wiring
* [ADR-011](adr/ADR-011-reputation-state-source-of-truth.md) — ILN vs bonus-module rule table
* [ADR-014](adr/ADR-014-pair-default-tracking.md) — per-pair default concentration signal (insurance pool)
* [`Architecture.md`](Architecture.md) — system diagram conventions used above
* [`scf-technical-narrative.md`](scf-technical-narrative.md) — grant-reviewer synthesis linking this guide

---

## Why This Matters

Without reputation:

* LPs cannot price risk
* Capital becomes inefficient
* Defaults increase

With reputation:

* Better pricing
* More liquidity
* Scalable credit markets

---

## Summary

The ILN reputation model transforms raw invoice data into:

> **Programmable creditworthiness**

This is the foundation for decentralized invoice financing at scale.
