# Access Control Matrix

## 1. Overview

> **Machine-checked matrix:** the authoritative Instruction → gate table for all
> five contracts is generated from `#[contractimpl]` source by
> [`scripts/generate-access-control-matrix.py`](../scripts/generate-access-control-matrix.py)
> into [`access-control-matrix.generated.md`](access-control-matrix.generated.md).
> CI fails on drift. The tables below remain as narrative/role documentation;
> if they disagree with the generated file on a function's gate, update the
> narrative.

> **Parameter safety:** For safe ranges of all adjustable parameters (fee rates,
> discount rates, reputation thresholds, oracle windows, etc.), see
> [Governance Operations Playbook § Adjustable Parameter Safe Ranges](governance-operations-playbook.md#adjustable-parameter-safe-ranges).


The ILN-Smart-Contract implements a centralized access-control architecture to guarantee that all protocol operations are properly authorized. By centralizing permissions into shared guards, we achieve:
- **Consistency**: All similar checks behave exactly the same way across different endpoints.
- **Audibility**: Clear, easily reviewable access annotations on every public instruction.
- **Maintainability**: Reduced code duplication by eliminating inline authorization checks.

Security goals include enforcing the principle of least privilege, preventing unauthorized state mutations, and ensuring that any authorization failure immediately returns a deterministic contract error.

## 2. Role Definitions

### Submitter
Represents a freelancer or service provider who submits invoices to the protocol.
- **Can**: Create invoices, update invoices before funding, cancel un-funded invoices, and transfer invoice ownership.
- **Cannot**: Modify another user's invoice, force funding, or alter protocol configuration.

### Payer
The client who owes payment on the submitted invoice.
- **Can**: Pay the invoice (mark paid), file an appeal if a default occurs unfairly.
- **Cannot**: Create an invoice on behalf of a submitter, modify invoice terms, or claim yields.

### LP (Liquidity Provider)
Entities providing liquidity to fund pending invoices.
- **Can**: Join funding queues, fund approved invoices, claim yields, and claim default refunds.
- **Cannot**: Approve themselves without queue resolution, modify invoice terms, or appeal a default.

### Admin
The protocol administrator.
- **Can**: Update fee rates, maximum discount rates, distribution contracts, manage allowed tokens, pause/unpause the protocol, and resolve default appeals.
- **Cannot**: Arbitrarily modify invoice ownership, submit invoices as users without explicit authorization, or drain funds.

### Governance
Reserved for future DAO or multisig control over core parameter changes. Currently delegates to Admin functionality.

### Insurance Pool Admin
Authorized to process default claims against the insurance pool (typically the liquidity contract).
- **Can**: File claims on behalf of defaulted invoices, trigger compensation payouts, and query pool state.
- **Cannot**: Modify enrollment, adjust premium rates, or drain the pool directly.

### Anyone
Publicly accessible read or state-transition functions that do not require specific authorization.
- **Can**: Read contract stats, query scores, resolve fund queues, and expire timed-out invoices.

## 3. Invoice Liquidity Contract — Permission Matrix

### Core Invoice Operations

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `initialize` | Anyone | Initializes the contract once |
| `get_contract_stats` | Anyone | Reads protocol statistics |
| `submit_invoice` | Submitter | Submits a new invoice |
| `update_invoice` | Submitter | Updates an existing un-funded invoice |
| `submit_invoices_batch` | Submitter | Submits multiple invoices at once |
| `convert_invoice_token` | Submitter | Converts an invoice's token to another approved token |
| `transfer_invoice` | Submitter | Transfers ownership of an invoice |
| `cancel_invoice` | Submitter | Cancels an un-funded invoice |
| `get_referral_stats` | Anyone | Reads referral code usage statistics |
| `join_fund_queue` | LP | Enqueues intent to fund an invoice |
| `resolve_fund_queue` | Anyone | Selects the LP with highest reputation from queue |
| `fund_invoice` | LP | Funds a pending invoice (with oracle verification checks) |
| `expire_invoice` | Anyone | Marks a pending expired invoice as Expired |
| `mark_paid` | Payer | Pays off a funded invoice |
| `claim_yield` | LP | Claims yield earnings for a paid invoice |
| `claim_default` | LP | Claims refund for a defaulted invoice |
| `appeal_default` | Payer | Appeals an unfair default |
| `resolve_appeal` | Admin | Approves or rejects a default appeal |
| `payer_score` | Anyone | Reads a payer's reputation score |
| `lp_score` | Anyone | Reads an LP's reputation score |
| `suggested_discount_rate` | Anyone | Calculates discount rate based on payer score |
| `get_invoice` | Anyone | Reads invoice details |
| `list_invoices_by_submitter` | Anyone | Lists invoices submitted by a user (paginated) |
| `list_invoices_by_lp` | Anyone | Lists invoices funded by an LP (paginated) |
| `get_invoice_count` | Anyone | Reads total invoice count |

### Admin Configuration

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `set_admin` | Admin | Updates the contract administrator address (rate-limited: ~1h) |
| `update_fee_rate` | Admin | Sets the protocol fee rate (rate-limited: ~30min) |
| `update_max_discount` | Admin | Updates the maximum allowed discount rate (rate-limited: ~30min) |
| `update_decay_params` | Admin | Updates reputation decay rate and period |
| `set_distribution_contract` | Admin | Updates the distribution contract address (rate-limited: ~10min) |
| `update_fee_tiers` | Admin | Updates tiered fee structure for invoices by size |
| `get_fee_tiers` | Anyone | Reads the current tiered fee configuration |
| `set_min_payer_reputation` | Admin | Sets minimum payer reputation threshold (rate-limited: ~30min) |
| `add_token` | Admin | Adds a supported token to the protocol (rate-limited: ~10min) |
| `remove_token` | Admin | Removes a supported token (rate-limited: ~10min) |
| `get_token_decimals` | Anyone | Reads decimal places for a supported token |
| `pause` | Admin | Pauses the protocol for emergency (not rate-limited) |
| `unpause` | Admin | Resumes protocol operations (not rate-limited) |
| `upgrade` | Admin | Emits upgrade event for WASM hash change (rate-limited: ~2h) |
| `get_version` | Anyone | Reads contract version string |
| `get_storage_version` | Anyone | Reads storage schema version for migrations |
| `migrate` | Admin | Executes storage migration logic for upgrades (not rate-limited: one-shot version-gated op — see docs/rate-limiting.md) |
| `update_config` | Admin | Updates core economic config in one call (rate-limited: ~30min, Issue #859) |
| `max_invoice_amount` | Anyone | Reads the per-invoice size cap for the staged mainnet rollout (Issue #655; 0 = uncapped) |
| `set_max_invoice_amount` | Admin (routed through governance once `admin` = the governance contract) | Sets the per-invoice size cap; raised over time as rollout confidence grows (rate-limited: ~30min) |
| `token_volume_cap` | Anyone | Reads the cumulative funded-volume cap for a token (Issue #655; 0 = uncapped) |
| `set_token_volume_cap` | Admin (routed through governance once `admin` = the governance contract) | Sets the cumulative funded-volume cap for a token (rate-limited: ~30min) |

### Oracle Registry & Price Oracle

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `set_price_oracle` | Admin | Updates the primary price oracle address (rate-limited: ~10min) |
| `get_price_oracle` | Anyone | Reads the current price oracle address |
| `set_max_oracle_age` | Admin | Updates the maximum acceptable oracle data age in ledgers (rate-limited: ~10min) |
| `get_max_oracle_age` | Anyone | Reads the current max oracle age setting |
| `register_oracle` | Admin | Registers an oracle for a specific feed type (rate-limited: ~10min, Issue #859) |
| `remove_oracle` | Admin | Removes oracle registration for a feed type (rate-limited: ~10min, Issue #859) |
| `register_token_oracle` | Admin | Registers an oracle for a specific token (rate-limited: ~10min, Issue #859) |
| `remove_token_oracle` | Admin | Removes oracle registration for a token (rate-limited: ~10min, Issue #859) |
| `get_oracle_for_token` | Anyone | Reads the registered oracle address for a given token |
| `get_oracle_health` | Anyone | Reads cached oracle health status (accessible while contract is paused) |
| `check_oracle_health` | Anyone | Performs live oracle health check and caches result (accessible while paused) |
| `is_oracle_circuit_tripped` | Anyone | Checks if an oracle's circuit breaker is activated |
| `reset_oracle_circuit` | Admin | Resets a circuit-tripped oracle's status (rate-limited: ~10min, Issue #859) |
| `add_price_source` | Admin | Adds a price feed source for an oracle feed type (rate-limited: ~10min, Issue #859) |
| `remove_price_source` | Admin | Removes a price feed source (rate-limited: ~10min, Issue #859) |
| `get_price_sources` | Anyone | Reads all registered price sources for a feed type |
| `set_max_price_deviation_bps` | Admin | Sets maximum acceptable price deviation in basis points (rate-limited: ~10min, Issue #859)
| `set_twap_enabled` | Admin | Opts a feed into TWAP-windowed price reads (rate-limited: ~10min, Issue #859) |
| `set_twap_window_ledgers` | Admin | Sets the TWAP averaging window in ledgers (rate-limited: ~10min, Issue #859) |
| `record_twap_sample` | Admin/keeper | Pushes one TWAP price sample (not rate-limited: high-frequency keeper op — see docs/rate-limiting.md) | |
| `get_max_price_deviation_bps` | Anyone | Reads the max price deviation setting |
| `get_verified_price` | Anyone | Queries and verifies price data from oracle (with deviation checks) |

### Insurance Pool Integration

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `set_insurance_pool` | Admin | Updates the insurance pool contract address (rate-limited: ~10min) |
| `get_insurance_pool` | Anyone | Reads the current insurance pool contract address |

### Insurance Pool Contract

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `insurance_pool_claim` | Insurance Pool Admin (liquidity contract) | Files a claim for a defaulted invoice |
| `insurance_pool_enroll` | LP | Opts into default-protection insurance |
| `insurance_pool_deposit_premium` | LP | Pays premium to pool (auto-enrolls) |
| `insurance_pool_get_balance` | Anyone | Reads current pool balance |
| `insurance_pool_get_coverage` | Anyone | Reads per-claim coverage cap |
| `insurance_pool_is_enrolled` | Anyone | Checks LP enrollment status |
| `insurance_pool_get_premiums_paid` | Anyone | Reads cumulative premiums paid by an LP |

## 7. Insurance Pool Access Control

The insurance pool operates as a separate contract with its own authorization model:

- **Pool Enrollment**: LPs call `enroll()` or implicitly auto-enroll on first premium deposit.
- **Premium Deposits**: Any LP can call `deposit_premium()` to add funds to the pool (requires LP signature).
- **Claims**: Only the configured pool admin (the liquidity contract in production) can call `claim()` to trigger compensation for a confirmed default.
- **Queries**: Pool state (balance, coverage, enrollment, premiums) is publicly readable to support analytics and integrations.

This isolation ensures that the insurance pool cannot be drained except through claims authorized by the main contract, and that no single LP can block others from withdrawing coverage.

### Additional Admin Functions (Dispute Resolution)

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `resolve_appeal` | Admin | Approves or rejects a default appeal (must call `require_admin`) |
| `resolve_dispute` | Admin | Resolves a dispute on an invoice |
| `auto_resolve_dispute` | Anyone | Auto-resolves a dispute after timeout elapsed |
| `set_min_payer_reputation` | Admin | Sets minimum payer reputation threshold |
| `set_price_oracle` | Admin | Updates the price oracle address |
| `set_max_oracle_age` | Admin | Updates the maximum oracle age |
| `upgrade` | Admin | Emits upgrade event for WASM hash change |
| `update_config` | Admin | Updates reputation and token configuration |

## 4. Governance Contract — Permission Matrix

### Proposal & Voting Operations

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `initialize` | Anyone | Initializes the governance contract once |
| `create_proposal` | Anyone (min balance check) | Creates a new governance proposal |
| `cast_vote` | Anyone (with voting power) | Casts a vote on an active proposal |
| `delegate_votes` | Token Holder | Delegates voting power to another address |
| `undelegate_votes` | Delegator | Revokes delegated voting power |
| `get_delegate` | Anyone | Reads vote delegation target for an address |
| `execute_proposal` | Anyone | Executes a passed proposal after timelock expires |
| `veto_proposal` | Admin | Vetoes an active or passed proposal |
| `disable_veto_power` | Admin | Permanently removes admin veto capability |
| `is_veto_power_enabled` | Anyone | Checks if veto power is still active |

### Proposal Query & Status

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `get_proposal` | Anyone | Reads full proposal details by ID |
| `list_proposals` | Anyone | Lists all proposals (paginated) |
| `has_voted` | Anyone | Checks if a voter has already voted on a proposal |
| `get_applied_vote_weight` | Anyone | Reads final voting weight applied by a voter on a proposal |

### Governance Configuration

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `set_min_quorum_bps` | Admin (ILN Contract) | Updates quorum threshold in basis points |
| `get_min_quorum_bps` | Anyone | Reads current quorum threshold |
| `set_min_proposal_balance` | Admin (ILN Contract) | Updates minimum balance required to create proposal |
| `get_min_proposal_balance` | Anyone | Reads minimum proposer balance requirement |
| `set_max_delegation_depth` | Admin | Updates maximum delegation chain depth |
| `get_max_delegation_depth` | Anyone | Reads max delegation depth setting |
| `set_gov_token_total_supply` | Admin (ILN Contract) | Updates total supply of governance token |
| `get_gov_token_total_supply` | Anyone | Reads current governance token total supply |
| `set_execution_delay` | Admin | Sets timelock delay before proposal execution (in ledgers) |
| `get_execution_delay` | Anyone | Reads current execution delay setting |
| `set_quadratic_voting_enabled` | Admin | Toggles quadratic voting mode on/off |
| `is_quadratic_voting_enabled` | Anyone | Checks if quadratic voting is currently enabled |

## 5. Distribution Contract (iln_distribution) — Permission Matrix

### Reward Accrual & Claims

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `initialize` | Anyone | Initializes the distribution contract once (requires ILN contract & gov token) |
| `accrue_lp` | ILN Contract Only | Accrues LP yield rewards (called internally by invoice_liquidity) |
| `accrue_settlement` | ILN Contract Only | Accrues freelancer/payer settlement rewards (called internally) |
| `claim_tokens` | Any Participant | Claims accumulated reward tokens (requires participant signature) |
| `get_accrual` | Anyone | Reads accumulated rewards for a participant |

### Reward Rate Configuration

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `set_lp_reward_rate` | Admin (ILN Contract) | Sets the LP reward rate in tokens per unit |
| `get_lp_reward_rate` | Anyone | Reads current LP reward rate |
| `set_freelancer_reward_rate` | Admin (ILN Contract) | Sets the freelancer reward rate |
| `get_freelancer_reward_rate` | Anyone | Reads current freelancer reward rate |
| `set_payer_reward_rate` | Admin (ILN Contract) | Sets the payer reward rate |
| `get_payer_reward_rate` | Anyone | Reads current payer reward rate |

## 6. Reputation Bonus Contract — Permission Matrix

| Instruction | Allowed Role(s) | Description |
| ----------- | --------------- | ----------- |
| `initialize` | Anyone | Initializes the reputation bonus contract once |
| `check_payer_reputation` | Anyone | Verifies a payer's reputation and returns applicable bonuses |
| `check_lp_reputation` | Anyone | Verifies an LP's reputation and returns applicable bonuses |

## 8. Audit Findings (Issue #540)

The following findings were identified and resolved during the access control audit:

### Finding AC-01: Missing `require_admin` in `resolve_appeal`
- **Severity:** High
- **Location:** `contracts/invoice_liquidity/src/lib.rs:resolve_appeal`
- **Description:** The function lacked an explicit `require_admin` guard. Although only the payer of the specific invoice could trigger appeals, the resolution function could be called by anyone, allowing unauthorized state transitions from `Appealed` to `Defaulted`.
- **Resolution:** Added `require_admin(&env)?;` as the first statement in the function body.
- **Commit:** This commit.

### Finding AC-02: All other admin functions properly guarded
- All admin-privileged functions in the Invoice Liquidity, Insurance Pool, and Governance contracts include explicit authorization checks at entry. No additional missing guards were found.

## 9. Rate Limiting Design (Issue #541)

### Rationale

Certain admin operations are sensitive to high-frequency invocation — an attacker who compromises an admin key could rapidly toggle economic parameters to extract value or disrupt protocol operations. Rate limiting introduces a time-based cooldown between successive calls to mitigate this risk.

### Functions with Rate Limiting

| Function | Cooldown | Rationale |
|---|---|---|
| `set_admin` | 720 ledgers (~1h) | Admin key rotation must be slow to allow detection |
| `upgrade` | 1440 ledgers (~2h) | Contract upgrade is the most sensitive operation |
| `update_fee_rate` | 360 ledgers (~30min) | Economic parameter manipulation |
| `update_max_discount` | 360 ledgers (~30min) | Economic parameter manipulation |
| `set_min_payer_reputation` | 360 ledgers (~30min) | Economic parameter manipulation |
| `set_max_invoice_amount` | 360 ledgers (~30min) | Economic parameter manipulation (Issue #655 staged rollout cap) |
| `set_token_volume_cap` | 360 ledgers (~30min) | Economic parameter manipulation (Issue #655 staged rollout cap) |
| `set_distribution_contract` | 120 ledgers (~10min) | Infrastructure change |
| `set_price_oracle` | 120 ledgers (~10min) | Infrastructure change |
| `set_max_oracle_age` | 120 ledgers (~10min) | Infrastructure change |
| `add_token` | 120 ledgers (~10min) | Token allowlist change |
| `remove_token` | 120 ledgers (~10min) | Token allowlist change |
| `update_config` | 360 ledgers (~30min) | Economic parameter manipulation (Issue #859) |
| `update_fee_tiers` | 360 ledgers (~30min) | Economic parameter manipulation |
| `register_oracle` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `remove_oracle` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `register_token_oracle` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `remove_token_oracle` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `reset_oracle_circuit` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `add_price_source` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `remove_price_source` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `set_max_price_deviation_bps` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `set_twap_enabled` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |
| `set_twap_window_ledgers` | 120 ledgers (~10min) | Oracle registry configuration (Issue #859) |

### Exempt Functions

Emergency functions are not rate-limited so they can be used immediately when a threat is detected:
- `pause`
- `unpause`
- `resolve_appeal`
- `resolve_dispute`

Other documented exemptions (full matrix and rationale: [docs/rate-limiting.md](./rate-limiting.md)):
- `migrate` — one-shot, version-gated; blocking it behind the upgrade cooldown would delay post-upgrade repairs
- `record_twap_sample` — high-frequency keeper op; limiting it would starve the TWAP window
- multisig `execute_proposal` — replaced by N-of-M threshold + proposal-expiry timelock
- `check_oracle_health` — feeds the circuit breaker itself; limiting it would starve the breaker

### Implementation

Rate limiting is implemented in `contracts/invoice_liquidity/src/access.rs`:

- `check_rate_limit(env, fn_name, cooldown_ledgers)` checks the last ledger when the function was called. If insufficient ledgers have elapsed, it returns `ContractError::RateLimited`. Otherwise, it records the current ledger as the last call time.
- A function's first-ever call always passes (no prior record exists to space out from); only *consecutive* calls are spaced. (Issue #859 fixed a cold-start quirk where `unwrap_or(0)` rejected the first call on low-sequence networks.)
- Cross-contract coverage matrix for all five contracts: [docs/rate-limiting.md](./rate-limiting.md).
- Storage key: `DataKey::RateLimit(Symbol::new(env, fn_name))` — per-function, instance storage.
- The cooldown is measured in ledgers (not timestamps) to align with Soroban's deterministic execution model.
- At ~5 seconds per ledger: 120 ledgers ≈ 10 min, 360 ≈ 30 min, 720 ≈ 1h, 1440 ≈ 2h.

### Audit Finding RL-01

- **Severity:** Medium
- **Finding:** Several admin functions (`update_fee_rate`, `set_admin`, `upgrade`, etc.) lacked any rate-limiting mechanism, allowing rapid successive calls that could be used to grief the protocol or confuse indexers.
- **Resolution:** Added `check_rate_limit` guard to all sensitive admin functions with appropriate cooldown periods.

## 10. Pause Behavior & Cross-Contract Scope

### Scope of `pause()`

`pause()` sets a single `Paused` flag (see `docs/storage-layout.md`) read by the *invoice_liquidity* contract's own state-changing entry points — `submit_invoice`, `fund_invoice`, `mark_paid`, `claim_yield`, `claim_default`, `appeal_default`, `expire_invoice`, etc. Each checks `is_paused` and returns `ContractError::ContractPaused` before performing any state mutation.

Two categories of operation are explicitly **not** affected by this flag:

1. **Read-only views**, e.g. `get_contract_stats`, `get_invoice`, `payer_score`/`lp_score`, and — the subject of this section — the oracle registry's read surface: `get_oracle_for_token`, `get_oracle_health`, and `check_oracle_health`. These carry no `is_paused` guard by design, so monitoring/keeper tooling can keep observing protocol and oracle state throughout an incident. `check_oracle_health` in particular performs a live cross-contract call to the resolved oracle and persists a health snapshot — this succeeds identically whether the contract is paused or not.
2. **Oracle registry governance mutations** — `register_oracle`, `remove_oracle`, `register_token_oracle`, `remove_token_oracle` (Issue #532). These are gated only by `require_admin`, not `is_paused`. This is intentional: repointing or clearing a misbehaving oracle is itself a common *response* to an incident, and gating it behind an unpause would create a chicken-and-egg problem for governance.

### The cross-contract boundary

The oracle registry tracked here is bookkeeping *inside* invoice_liquidity (which oracle address to query for a given feed type/token) — the oracles it points at are themselves separate deployed contracts, and governance itself typically operates through the separate Governance contract, executing proposals that call back into `register_oracle`/`remove_oracle`. Pausing invoice_liquidity has no effect on either:

- The external oracle contracts continue to serve `get_payer_data` / price queries exactly as before — pausing the consumer does not pause the provider.
- The Governance contract's own proposal lifecycle (`propose`, `vote`, `execute`) is entirely unaware of invoice_liquidity's `Paused` flag; only the *effect* of an executed oracle-config proposal (a call to `register_oracle`/`remove_oracle`) lands here, and, per above, that call itself is not blocked by pause.

In short: **pause halts invoice_liquidity's own funding/settlement mutations only** — it is not a kill switch for oracle reads, oracle governance, or any other contract in the system. `fund_invoice` reflects this correctly: it checks `is_paused` as its very first step, so a paused call never reaches the oracle registry at all (no query, no health write) rather than querying it and silently discarding the result. See `contracts/invoice_liquidity/src/tests_oracle_registry.rs` (`test_get_oracle_for_token_readable_while_paused`, `test_check_oracle_health_readable_while_paused`, `test_fund_invoice_paused_never_reaches_oracle_registry`, `test_oracle_registry_mutations_unaffected_by_core_contract_pause`).

## 11. CI Verification for Undocumented Functions

To prevent function-documentation drift, a grep-based CI check should verify that no new public contract functions are added without corresponding documentation in this matrix.

**Suggested implementation** (`.github/workflows/access-control-ci.yml`):

```bash
#!/bin/bash
set -e

# Extract all public function names from each contract
for contract_dir in contracts/invoice_liquidity contracts/iln_governance contracts/iln_distribution contracts/insurance_pool contracts/reputation_bonus; do
  if [ ! -f "$contract_dir/src/lib.rs" ]; then
    continue
  fi
  
  functions=$(grep -E '^\s+pub fn ' "$contract_dir/src/lib.rs" | \
              sed 's/.*pub fn \([a-z_]*\).*/\1/' | \
              sort | uniq)
  
  doc_file="docs/access-control.md"
  for func in $functions; do
    # Skip test functions and internal helpers
    if [[ $func == *"test"* ]] || [[ $func == *"internal"* ]]; then
      continue
    fi
    
    # Check if function is documented in the matrix
    if ! grep -q "\`$func\`" "$doc_file"; then
      echo "ERROR: Public function '$func' in $contract_dir is not documented in access-control.md"
      exit 1
    fi
  done
done

echo "✓ All public contract functions are documented in access-control.md"
```

This check should run on every PR to enforce that new functions and their access requirements are documented before merge.

## 12. Re-Verification Log (Issue #676)

**Date:** 2026-08-29  
**Scope:** All five contract crates (`invoice_liquidity`, `iln_governance`, `iln_distribution`, `insurance_pool`, `reputation_bonus`) plus `fuzz`

### Verification Summary

- ✅ **invoice_liquidity**: All public functions verified against code. Added 20+ oracle registry and configuration functions.
- ✅ **iln_governance**: All 21 public functions documented. Covers proposal lifecycle, voting, delegation, execution, veto, and configuration.
- ✅ **iln_distribution**: All 9 public functions documented. Cross-contract authorization (ILN-only) for internal accrual functions verified.
- ✅ **insurance_pool**: Confirmed 7 functions documented. Pool enrollment, premium deposits, claims, and queries included.
- ✅ **reputation_bonus**: Confirmed 3 functions documented. Reputation checking functions included.
- ✅ **No undocumented public functions found** across all crates.
- ✅ **No unauthorized function access** — all admin paths properly guarded with `require_admin`.
- ✅ **Rate limiting properly applied** to sensitive admin operations with documented cooldown periods.
- ✅ **Cross-contract boundaries** clearly documented (distribution contract authorization, governance contract roles).

### Access Control Gaps Found & Resolved

1. ✅ **Oracle registry functions** were missing from the matrix — now fully documented with access levels.
2. ✅ **Governance contract** was completely absent from previous version — now comprehensively added.
3. ✅ **Distribution contract** reward rate management was undocumented — now fully added.
4. ✅ **Rate-limiting annotations** on admin functions were missing from descriptions — now explicitly marked.
5. ✅ **Pause behavior** documentation updated to clarify oracle registry exceptions.

**Conclusion:** All public functions across all five contract crates are now documented with their access requirements, authorized roles, and rate-limiting status. The matrix is authoritative and CI-verifiable.

## 13. Security Notes

- **Principle of Least Privilege**: Each instruction relies only on the minimal authority required to execute.
- **Centralized Verification**: Extracted inline logic ensures uniform verification logic and robust testing.
- **Auditability Improvements**: Every guard clearly emits a deterministic `Unauthorized` error instead of panicking, enhancing tracing.
- **Rejection Behavior**: If authorization fails, the protocol safely rejects the mutation without consuming extra gas or altering contract state.

## 14. Mainnet Admin Signer Verification (Issue #647)

Once the production admin is configured as a multi-sig account (see "Multi-sig admin
configured" in the [Mainnet Launch Checklist](mainnet-launch-checklist.md)), the set of
Stellar keys authorized to sign as that account must stay in sync with who
[CODEOWNERS](../.github/CODEOWNERS) says is on the contracts team — otherwise a
maintainer could retain signing power after leaving the team, or a key could be added
on-chain that no one off-chain can account for, with no way to notice either.

- **Mapping**: [`.github/mainnet-admin-signers.json`](../.github/mainnet-admin-signers.json)
  records which GitHub identity controls each on-chain signer key, and which CODEOWNERS
  team it should match (`@Keengfk/contracts-team`). It is itself CODEOWNERS-protected so
  changes require contracts-team and security-lead review.
- **Check**: [`scripts/verify-admin-signers.ts`](../scripts/verify-admin-signers.ts) fetches
  the admin account's signers from Horizon and confirms every on-chain key has a mapping
  entry (and vice versa), and — when a `GITHUB_TOKEN` is available — that every mapped
  signer is still a current member of the CODEOWNERS team.
- **CI**: [`.github/workflows/admin-signer-check.yml`](../.github/workflows/admin-signer-check.yml)
  runs this on every change to CODEOWNERS or the signer mapping, plus a daily schedule to
  catch drift introduced directly on-chain.
- Before the multi-sig admin is configured, the check is a no-op (`ADMIN_ADDRESS` is
  unset), so it does not block CI ahead of launch.

## 15. On-Chain Admin Action Audit Log (Issue #645)

For SCF review and community trust, every admin-gated call that actually executes in
`invoice_liquidity` (`set_admin`, `pause`/`unpause`, `add_token`/`remove_token`,
`update_fee_rate`, `update_max_discount`, `update_decay_params`, `update_fee_tiers`,
`set_min_payer_reputation`, `set_price_oracle`, `set_max_oracle_age`,
`set_distribution_contract`, `set_insurance_pool`, `upgrade`, `migrate`,
`resolve_appeal`, `resolve_dispute`) is appended to a bounded on-chain ring buffer,
independent of the per-action events (`AdminChanged`, `ParameterUpdated`, `TokenAdded`,
...) that were already published.

- **Query**: `get_recent_admin_actions(limit: u32) -> Vec<AdminActionRecord>` — a public,
  unauthenticated view returning up to `limit` entries (capped at
  `ADMIN_ACTION_LOG_CAPACITY = 50`), newest first. Each `AdminActionRecord` carries a
  monotonic `seq`, the `action` name, the `admin` address that authorized it, and the
  `ledger`/`timestamp` at which it executed.
- **Why a view instead of replaying events**: reconstructing "what has the admin done
  recently" from the Horizon event stream requires paging through the full history and
  filtering by topic; this view answers the same question in a single contract call, with
  no off-chain indexing dependency — useful for reviewers or dashboards that just need a
  recent-activity snapshot.
- **Recording point**: entries are written in `access::record_admin_action`, called
  immediately after each function's `require_admin` check succeeds. Because Soroban
  transactions are atomic, a log entry is rolled back along with the rest of the
  transaction if the function later errors (e.g. `RateLimited`, `InvoiceNotFound`) — so
  the log only ever reflects actions that actually took effect, not merely
  authorized-but-failed attempts.
- **Bounded, not a replacement**: the ring buffer only retains the most recent 50 entries
  and is not a substitute for the unbounded event log — it is a cheap on-chain
  "recent activity" view layered on top of it. Long-horizon audit trails should still be
  built from indexed events (see [event-types.md](event-types.md)).
