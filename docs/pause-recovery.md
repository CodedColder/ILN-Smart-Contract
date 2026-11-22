# Pause / Unpause Recovery Behavior

Issue #1184 — audit of the protocol's emergency stop (`pause`) and resume
(`unpause`) levers, with emphasis on **recovery**: that a halt is engaged
consistently, that rejecting work while halted leaves no residue that could
prevent the resume, and that resuming fully restores normal operation.

This is an **implementation-only audit**. No contract behavior is changed; the
deliverable is verified findings plus focused regression tests.

Code paths:

* `contracts/invoice_liquidity/src/lib.rs` — `pause`, `unpause`,
  `propose_pause`, `propose_unpause`, `execute_proposal`, the per-entry-point
  gated guards, and the read-only views.
* `contracts/invoice_liquidity/src/invoice.rs` — `is_paused`, `set_paused`,
  `get_last_pause_timestamp`.
* `contracts/invoice_liquidity/src/access.rs` — `require_admin`, rate limiter,
  `lock_reentrancy` / `unlock_reentrancy`.
* `contracts/invoice_liquidity/src/multisig.rs` — threshold-gated admin path.
* Tests: `contracts/invoice_liquidity/src/tests_pause_recovery.rs`.

## Storage and events

| Item | Key / topic | Notes |
| --- | --- | --- |
| Halt flag | `StorageKey::Paused` (instance, `bool`) | Read via `is_paused`; default `false`. |
| Paused-since marker | `StorageKey::LastPauseTimestamp` (instance, `u64`) | Written **only** when `set_paused(true)`. Never cleared by `unpause`. |
| Halt event | topic `paused`, data `ContractPaused { timestamp }` | Emitted on every `pause()` / multisig pause. |
| Resume event | topic `unpaused`, data `ContractUnpaused { timestamp }` | Emitted on every `unpause()` / multisig unpause. |

`set_paused(true)` sets the flag **and** stamps the marker; `set_paused(false)`
clears only the flag. Both the single-admin and the multisig path funnel through
this one helper, so they share a single source of truth for pause state.

## Authorization

* `pause()` and `unpause()` are **admin-only** (`require_admin` →
  `admin.require_auth()`). The authorization check runs **before** the pause
  gate, so the resume lever stays callable while the protocol is halted.
* The multisig path (`propose_pause` / `propose_unpause` → `sign_proposal` →
  `execute_proposal`) drives the **same** `set_paused` helper and emits the
  **same** events. Bootstrapping the multisig (`initialize_multisig_admin`) is
  the recommended operational configuration; when it is not configured, the
  single admin key remains the sole halt/resume authority.

## Observable behavior

| Case | `pause()` | `unpause()` |
| --- | --- | --- |
| Normal (running) | Halt engaged, `paused` event, marker stamped | No-op success, `unpaused` event, marker preserved |
| Repeated | Succeeds again, second `paused` event, marker **re-stamped** | Succeeds again, second `unpaused` event |
| Unauthorized caller | Rejected (auth error); flag untouched | Rejected (auth error); flag untouched |
| Stale state | n/a (not gated on current state) | n/a (not gated on current state) |
| Interrupted / rejected | Failed invocation rolls back entirely (flag, marker, event) | Same |

While halted, every *mutating* entry point fails closed with
`ContractError::ContractPaused` (code 26), evaluated before any auth or
validation, so no partial execution and no residue occurs:

`submit_invoice`, `update_invoice`, `submit_invoices_batch`,
`join_fund_queue`, `resolve_fund_queue`, `recover_fund_queue`, `fund_invoice`,
`transfer_invoice`, `transfer_lp_position`, `cancel_invoice`, `expire_invoice`,
`mark_paid`, `claim_default`, `appeal_default`, `dispute_invoice`,
`claim_pending_payout`.

Read-only views are **not** gated and stay available to operators/indexers:
`is_paused`, `get_protocol_status`, `get_invoice`, `get_invoice_count`,
`lp_score`, `get_contract_stats`, `get_version`. `claim_yield` is likewise
intentionally **not** gated (see INV-PR7).

## Invariants

* **INV-PR1 (authorization).** Only the configured admin — or a multisig
  proposal that has met its threshold — can halt or resume. A rejected
  `pause`/`unpause` leaves the flag unchanged.
* **INV-PR2 (no state gate).** `pause` / `unpause` are not conditioned on the
  current pause state: a redundant call is a success, not an error.
* **INV-PR3 (timestamped events).** Each lever publishes its event with the
  current ledger timestamp, so an indexer can reconstruct incident timing.
* **INV-PR4 (fail-closed gating).** Every pause-gated mutating entry point
  returns `ContractPaused` while halted and performs no state change.
* **INV-PR5 (read availability).** Read-only views remain callable while
  halted.
* **INV-PR6 (recovery).** Any operation rejected while halted succeeds after
  `unpause()`. Recovery is total.
* **INV-PR7 (yield exit path).** `claim_yield` is deliberately not pause-gated;
  an LP can still observe/realise yield while halted.
* **INV-PR8 (rate-limit exemption).** `pause` / `unpause` bypass the rate
  limiter; back-to-back toggles succeed with no ledger advance.
* **INV-PR9 (no residue).** A rejected call never leaves the one-shot
  reentrancy lock engaged; a subsequent call reports the same typed error, not
  `Reentrancy`.
* **INV-PR10 (paused-since marker).** `pause` stamps `LastPauseTimestamp` to the
  current ledger timestamp; `unpause` preserves it as a "last halted at"
  reference; it is never cleared.
* **INV-PR11 (multisig parity).** The multisig path drives the same pause flag
  and enforces the same gate as the single-admin entry points.

## Verified findings

* **VF-PR1 (INV-PR2, INV-PR10).** There is **no idempotency guard**. A repeated
  `pause()` succeeds, emits a *second* `paused` event, and re-stamps
  `LastPauseTimestamp` to the newer ledger — moving the "paused since" marker
  forward even though the contract was already halted. Indexers must treat the
  latest `paused` event's timestamp as authoritative, not the first.
* **VF-PR2 (INV-PR2).** `unpause()` on an already-running contract is accepted
  as a no-op success and still emits an `unpaused` event. It is not gated on the
  paused state, so an erroneous resume during an incident re-opens operations
  with no error signal.
* **VF-PR3 (INV-PR9).** The one-shot reentrancy lock cannot wedge recovery. A
  rejected call that acquires the lock and then returns an error *without* an
  explicit unlock (e.g. `fund_invoice` on a missing invoice, which returns
  `InvoiceNotFound` after `lock_reentrancy`) does **not** leak the lock: a second
  identical call reports the same typed error rather than `Reentrancy`. This is
  a consequence of Soroban rolling back the entire failed invocation, including
  the lock write.

## Residual risks

* **RR-PR1 (single-key liveness).** When the multisig is not bootstrapped, both
  halting and resuming depend on the single admin key. Loss or unavailability of
  that key while halted can strand the protocol in a paused state.
* **RR-PR2 (no auto-expiry).** The pause flag has no on-chain duration/TTL and
  is stored in instance storage, so it survives upgrades. A pause persists until
  an explicit `unpause`; there is no self-healing timeout.
* **RR-PR3 (ungated resume).** Because `unpause` is not gated on state and has no
  confirmation step or cooldown, it can be called accidentally/mistakenly during
  an active incident, immediately re-enabling all mutating entry points.
* **RR-PR4 (no fund freeze).** Pausing blocks transitions but does not lock
  funds; in-flight state (e.g. a Funded invoice pending payout) is unchanged,
  and `claim_yield` remains callable by design. Operators must reason about exit
  paths separately from the halt.
* **RR-PR5 (unlimited toggling).** The rate-limit exemption (INV-PR8) means a
  compromised admin/signer can toggle pause/unpause rapidly; only the
  authorization check — not rate limiting — bounds this.

## Migration / rollout

Additive audit only: **no** storage keys, error codes, entry points, or events
change; no storage migration is required. Existing paused instances created by
older code continue to read/write the same `StorageKey::Paused` and
`StorageKey::LastPauseTimestamp` and behave identically.
