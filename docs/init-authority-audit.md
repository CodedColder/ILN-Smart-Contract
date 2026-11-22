# Initialization and authority audit: distribution and insurance pool

Issues #1264 (`contracts/iln_distribution/src/lib.rs`) and #1265
(`contracts/insurance_pool/src/lib.rs`).

Findings are split into **verified** (read from the current `dev` source)
and **residual risk** (not fixable without an API/deploy change, or needs a
maintainer decision). Nothing here assumes a vulnerability that the code
does not show.

## `iln_distribution` — mint authority (#1264)

### Source of truth

| State | Set by | Can change after init? |
| ----- | ------ | ---------------------- |
| `IlnContract` (accrual and reward-rate authority) | `initialize` | No — second `initialize` returns `AlreadyInitialized` |
| `GovToken` | `initialize` | No |
| Governance token SAC admin | Token issuer via `set_admin`, checked by `initialize` (#861) | Only by the SAC admin, i.e. this contract — which exposes no `set_admin` path |

Only `claim_tokens` mints, only to `claimer`, only with `claimer.require_auth()`,
and only `total_earned − already_claimed` (never negative).

### Verified findings

1. **Rebinding is impossible after init.** A second `initialize` — with any
   `iln_contract` — fails with `DistributionError::AlreadyInitialized`, and
   accruals keep using the original binding. Covered by
   `initialize_rejects_double_init`.
2. **The previous double-init test asserted the wrong thing.** It expected a
   panic containing "already initialized", but `panic_with_error!` surfaces
   `Error(Contract, #1)`. It now asserts the typed error.
3. **A rejected init writes nothing.** A foreign-admin SAC makes `initialize`
   panic before any storage write; the contract stays uninitialized and a
   correct retry succeeds (`rejected_initialize_leaves_contract_retryable`).
4. **Minting needs the claimer.** Without the claimer's auth `claim_tokens`
   fails and nothing is minted
   (`claim_requires_claimer_auth_and_mints_nothing_without_it`).
5. **Reward-rate setters reject unauthorized callers** and leave rates
   unchanged (`reward_rate_setters_reject_unauthorized_caller`).

### Residual risk (maintainer decision)

- **Initialization front-run window — high.** `initialize` has no caller
  check, and #861 requires the governance token's SAC admin to already be
  this contract. The deploy flow is therefore two transactions: (1) the
  issuer moves the SAC admin to the distribution contract, (2) someone calls
  `initialize`. Anyone who calls `initialize` between (1) and (2) with their
  own `iln_contract` permanently controls accruals and can mint the real
  governance token, and because this contract has no `set_admin`, the SAC
  can never be recovered. None of the deploy scripts initialize this
  contract today, so the window is open for as long as it takes an operator
  to do it by hand.
  - Recommended fix: replace `initialize` with a `__constructor` (atomic with
    deployment). The contract address is deterministic (deployer + salt), so
    the issuer can move the SAC admin to the precomputed address *before*
    deploying. This changes the deploy API, so it is not done here.
  - Interim procedure: after `initialize`, read the instance storage (or the
    `init` event) and confirm `iln_contract` is the ILN core contract before
    wiring it with `set_distribution_contract`. If it is not, the token is
    compromised and must be replaced.
- **Reward-rate authority mismatch.** `require_governance_invoker` checks the
  stored `IlnContract`, but `iln_governance` is the contract that calls
  `set_*_reward_rate`. Unless the governance contract's address was passed as
  `iln_contract`, governance-driven rate changes fail authorization. Fixing
  it needs a separate stored governance address (a storage/API change).
- **Unbounded rates.** Setters accept any `i128`. Negative rates cannot mint
  (claims clamp to 0), but a very large rate lets the authority mint large
  amounts through ordinary claims.

## `insurance_pool` — initialization and admin recovery (#1265)

### Source of truth

| State | Set by | Authorization |
| ----- | ------ | ------------- |
| `Admin` | `initialize`, then `execute_admin_transfer` | `admin.require_auth()` at init; pending admin's auth at transfer |
| `PendingAdmin` / `AdminEta` | `propose_admin_transfer` | current admin; 3-day timelock |
| Claims, premiums, solvency circuit, parameters | admin-gated entry points | `require_admin` |

### Verified findings

1. **Admin transfer had no acceptance step (fixed).** `execute_admin_transfer`
   was callable by anyone once the timelock expired and installed
   `PendingAdmin` without its signature. A typo, an unfunded key or a
   contract with no auth path would become admin, leaving claim filing,
   premium parameters and `reset_solvency_circuit` with no authorized owner
   and no way back. It now calls `new_admin.require_auth()`. Until the
   pending admin signs, the current admin keeps full authority and can
   cancel. Tests: `admin_transfer_execution_requires_pending_admin_auth`,
   `admin_transfer_without_pending_admin_signature_keeps_current_admin`.
2. **`initialize` requires the admin's signature and is one-shot.** An
   unsigned init fails and writes nothing; a second init returns
   `AlreadyInitialized`
   (`initialize_requires_admin_auth_and_rejects_reinitialization`).
3. **The deploy script never initialized the pool (fixed).**
   `scripts/deploy-insurance-pool.sh` invoked `initialize` without the
   mandatory `--token` argument, so the call failed and every pool it
   deployed was left uninitialized. Anyone could then initialize it with
   themselves as admin. The script now requires `INSURANCE_POOL_TOKEN`,
   passes `--token`, requires the admin to be the signing source account,
   and verifies `get_token_address` afterwards.

### Behavior matrix — `execute_admin_transfer`

| Case | Result |
| ---- | ------ |
| No proposal | `NoPendingProposal` |
| Before eta | `TimelockNotExpired` |
| After eta, pending admin signs | Admin replaced, proposal cleared, `adm_exec` event |
| After eta, pending admin does not sign | Auth failure; proposal kept; current admin unchanged |
| Repeated after success | `NoPendingProposal` |

### Residual risk

- **Init front-run window — medium.** Deploy and `initialize` are separate
  transactions, so a watcher can initialize a fresh pool first. The deploy
  script now fails loudly if the stored token differs, and a pool initialized
  by someone else should simply be abandoned (no funds are at risk before
  the legitimate admin funds it). A `__constructor` would close the window.
- **Migration note.** Operators who scripted `execute_admin_transfer` from a
  third-party account must now have the pending admin sign it.
