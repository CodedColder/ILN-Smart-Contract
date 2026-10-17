# Rate Limiting Matrix — All Five Contracts (Issue #859)

## Overview

This document is the audit matrix required by Issue #859 (`Closes #55`):
every privileged (admin/governance-gated) function across all five contracts,
with either its rate limit (cooldown) or an explicit, documented reason it
does not need one. It supersedes ad-hoc notes; the invoice_liquidity design
rationale lives in [access-control.md §9](./access-control.md#9-rate-limiting-design-issue-541).

Mechanism (invoice_liquidity only): `check_rate_limit(env, fn_name, cooldown)`
in `contracts/invoice_liquidity/src/access.rs`, keyed per function name in
instance storage.

Semantics:

- The **first-ever** call of a rate-limited function always succeeds (no
  prior call exists to space out from). The earlier `unwrap_or(0)` default
  rejected a cold start whenever the ledger sequence was still below the
  cooldown — only observable on low-sequence test networks (mainnet's
  sequence has always been far above every cooldown). Rate limiting spaces
  *consecutive* calls; it never gates a function's first use.
- A repeat call before `cooldown` ledgers have elapsed since the last
  **successful** call returns `ContractError::RateLimited`. Failed
  transactions roll back, so they do not consume the cooldown.
- Cooldowns are counted in ledgers (~5s each):

| Constant | Ledgers | ≈ Time |
|---|---|---|
| `DEFAULT_RATE_LIMIT_LEDGERS` | 120 | 10 min |
| `ECONOMIC_PARAM_COOLDOWN_LEDGERS` | 360 | 30 min |
| `ADMIN_CHANGE_COOLDOWN_LEDGERS` | 720 | 1 h |
| `UPGRADE_COOLDOWN_LEDGERS` | 1440 | 2 h |

## `invoice_liquidity`

### Rate-limited functions

| Function | Cooldown | Category |
|---|---|---|
| `set_admin` | 720 (~1 h) | Admin transfer |
| `upgrade` | 1440 (~2 h) | Contract upgrade |
| `update_fee_rate` | 360 (~30 min) | Economic parameter |
| `update_max_discount` | 360 (~30 min) | Economic parameter |
| `update_decay_params` | 360 (~30 min) | Economic parameter |
| `update_fee_tiers` | 360 (~30 min) | Economic parameter |
| `update_config` | 360 (~30 min) | Economic parameter — **added by this audit** |
| `set_min_payer_reputation` | 360 (~30 min) | Economic parameter |
| `set_max_invoice_amount` | 360 (~30 min) | Economic parameter |
| `set_token_volume_cap` | 360 (~30 min) | Economic parameter |
| `set_distribution_contract` | 120 (~10 min) | Infrastructure |
| `set_price_oracle` | 120 (~10 min) | Oracle infrastructure |
| `set_max_oracle_age` | 120 (~10 min) | Oracle infrastructure |
| `set_insurance_pool` | 120 (~10 min) | Infrastructure |
| `add_token` | 120 (~10 min) | Token allowlist |
| `remove_token` | 120 (~10 min) | Token allowlist |
| `register_oracle` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `remove_oracle` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `register_token_oracle` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `remove_token_oracle` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `reset_oracle_circuit` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `add_price_source` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `remove_price_source` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `set_max_price_deviation_bps` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `set_twap_enabled` | 120 (~10 min) | Oracle registry — **added by this audit** |
| `set_twap_window_ledgers` | 120 (~10 min) | Oracle registry — **added by this audit** |

The cooldown constant for each function mirrors its category siblings
(e.g. all oracle-registry config setters share the `set_price_oracle`
`DEFAULT` cooldown), so an attacker who compromises the admin key gains no
faster configuration surface through one setter than another.

### Documented exemptions (no rate limit)

| Function(s) | Why no rate limit is needed |
|---|---|
| `pause`, `unpause` | Emergency circuit breakers — must work on the first call, immediately, during an attack. Delaying them would be the risk. Audited instead via `record_admin_action` + `paused`/`unpaused` events. |
| `resolve_appeal`, `resolve_dispute` | Time-boxed dispute-resolution actions that must land within their response window; double-action is prevented by state transitions (already-resolved → error), not by cooldowns. |
| `migrate` | One-shot, version-gated storage migration (a second call at the current version is a no-op). Migrations typically must run *immediately* after an upgrade; clamping them to the upgrade cooldown would block time-critical post-upgrade repairs. Every call is `require_admin`-gated and audited via `record_admin_action`. |
| `record_twap_sample` | High-frequency keeper operation: the TWAP accumulator only produces a meaningful windowed average if samples land regularly across the window. Clamping it to one call per cooldown would leave the window sparsely sampled and easily skewed by a single manipulated sample — gating it weakens, not strengthens, the TWAP path. |
| `execute_proposal` (multisig) | Replaces per-function rate limits with stronger controls: N-of-M threshold signatures plus the `MULTISIG_WINDOW_LEDGERS` proposal expiry. A rate limit would additionally block legitimate rapid multi-step emergency responses already vetted by threshold. |
| `initialize`, `initialize_multisig_admin` | One-shot bootstraps; a repeat call fails on the already-initialized guard. |
| `check_oracle_health` | Keeper/verification path — it *records* the health data the circuit breaker consumes; limiting it would starve the breaker. Its mutation emits `oracle_health_recorded`/`oracle_circuit_tripped`. |
| User flows (`submit_invoice`, `fund_invoice`, `mark_paid`, `cancel_invoice`, `join_fund_queue`, `transfer_lp_position`, `pay_*`, `claim_*`, …) | Permissionless or role-gated by design (funder/payer/LP balances and invoice state machine provide the economic limits); they are not admin functions. |
| All read-only views | No state mutation to protect. |

## `iln_governance`

All privileged setters are gated by `iln_contract.require_auth()` — callable
only by the ILN core contract, i.e. only through governance proposal
execution. That path is itself rate-limited by the proposal lifecycle:
minimum proposal deposit, `VOTING_PERIOD_SECS` (3 days), and the execution
delay. No per-function cooldown is needed; the timelock is the control.

| Function | Gate | Rate limit |
|---|---|---|
| `set_max_delegation_depth` | ILN core auth (proposal execution: 3-day vote + execution delay) | Exempt — timelocked by lifecycle |
| `set_gov_token_total_supply` | same | Exempt — timelocked by lifecycle |
| `set_min_quorum_bps` | same | Exempt — timelocked by lifecycle |
| `set_min_proposal_deposit` | same | Exempt — timelocked by lifecycle |
| `set_proposal_deposit_sink` | same | Exempt — timelocked by lifecycle |
| `set_quadratic_voting_enabled` | same | Exempt — timelocked by lifecycle |
| `set_min_proposal_balance` | same | Exempt — timelocked by lifecycle |
| `set_execution_delay` | same | Exempt — timelocked by lifecycle |
| `disable_veto_power` | same | Exempt — timelocked by lifecycle (one-way) |

## `iln_distribution`

Reward-rate setters are gated by `require_iln_invoker` — the ILN core
contract only — so they are reachable exclusively through the same
governance proposal lifecycle described above.

| Function | Gate | Rate limit |
|---|---|---|
| `set_lp_reward_rate` | ILN core auth (proposal execution) | Exempt — timelocked by lifecycle |
| `set_freelancer_reward_rate` | ILN core auth (proposal execution) | Exempt — timelocked by lifecycle |
| `set_payer_reward_rate` | ILN core auth (proposal execution) | Exempt — timelocked by lifecycle |

## `insurance_pool`

The crate has no `check_rate_limit` infrastructure (it uses a different
design: admin auth plus explicit on-chain timelocks). Coverage that already
has a stronger control is marked accordingly; the remainder are documented
acceptances on the admin gate alone.

| Function | Gate | Rate limit |
|---|---|---|
| Admin transfer / resign flow | `require_admin` + `TIMELOCK_DELAY_SECONDS` (3-day) cancel-resubmit timelock | Exempt — timelocked (3 days) |
| Coverage change (cap/enrollment params) | `require_admin` + 3-day timelock with cancel/resubmit anti-bypass | Exempt — timelocked (3 days) |
| `set_base_premium_rate_bps` | `require_admin`, applies to **future** enrollments only (no retroactive effect) | Exempt — admin gate + prospective-only; readable via `get_base_premium_rate_bps`. Accepted gap: per-function cooldown could be added for symmetry; see residual risk below. |
| `set_risk_multiplier` | `require_admin`, prospective-only | Exempt — same as above; readable via `get_risk_multiplier_*` |
| `increment_default_count` | Internal bookkeeping invoked during default processing, not a standalone admin entrypoint | Exempt — not directly callable as a privileged op |
| `pause`/`unpause` equivalents (solvency circuit) | Circuit trips automatically; reset is admin-gated | Exempt — circuit reset is deliberate recovery action, audited by `solvency_circuit_*` events |

## `reputation_bonus`

| Function | Gate | Rate limit |
|---|---|---|
| `init` | One-shot bootstrap | Exempt — repeat rejected by init guard |
| `set_config` / `update_config` | Admin address check (ILN core / governance in production) | Exempt — admin gate only; config is prospective. Accepted gap (same class as insurance_pool setters). |
| Reward-claim / score-update flows | User `require_auth` + state machine | Exempt — not admin |

## Residual / accepted risk

- `insurance_pool` and `reputation_bonus` config setters rely on their admin
  gate (plus, for insurance, 3-day timelocks on the disruptive operations)
  without per-function cooldowns — accepted because neither crate ships rate
  limit infrastructure and adding it is a design change beyond this audit's
  scope. Flagged for reviewers: if symmetry is wanted, port the
  `check_rate_limit` pattern from `invoice_liquidity/src/access.rs`.
- All cooldowns are ledger-based; a sustained high-ledger-rate period shortens
  wall-clock cooldowns proportionally (accepted, matches original Issue #541
  design).

## Verification

- Regression tests: `tests_oracle_registry::test_oracle_admin_functions_rate_limited`,
  `test::test_update_config_rate_limited` (invoice_liquidity).
- Enumerate the live matrix any time:
  `grep -rn "check_rate_limit(" contracts/*/src/` — every hit above must
  appear in the rate-limited table, and every privileged function *without*
  a hit must appear under a documented exemption.
