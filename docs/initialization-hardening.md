# Initialization Hardening (Issue #1162)

This document describes how `invoice_liquidity` initialization is authorized,
why it is safe against replay and competing first callers, and how operators
should recover from a failed or interrupted initialization.

---

## 1. Background

Soroban separates **WASM deployment** (`stellar contract deploy`) from
**initialization** (`initialize`). Deployment installs the instance; a separate
transaction must then establish the privileged `admin` address.

Before #1162, `initialize(env, admin, usdc_token, eurc_token, xlm_token)` in
[lib.rs](../contracts/invoice_liquidity/src/lib.rs) had two weaknesses:

1. It never called `admin.require_auth()`, so **anyone** could be the first
   caller.
2. It inferred "already initialized" from the presence of `DataKey::InvoiceCount`
   alone — an optional-looking signal that a partially set-up instance might not
   have.

Because the admin controls fees, approved tokens, oracle settings, pause state,
upgrades, migrations, and other governance-integrated operations, an
unauthorized first caller could seize the instance during the public window
between deployment and initialization.

---

## 2. Authenticated initialization flow

`initialize` now **requires the supplied `admin` to authorize the call**:

```rust
if crate::storage::is_initialized(&env) {
    return Err(ContractError::AlreadyInitialized);
}

// The intended initializer must authorize the call.
admin.require_auth();

// Checks-effects-interactions: write the marker first.
env.storage()
    .instance()
    .set(&crate::storage::DataKey::Initialized, &true);
env.storage()
    .instance()
    .set(&crate::storage::DataKey::Admin, &admin);
```

Consequences:

- The `admin` argument **must be the same identity that signs the transaction**.
- A deployer, CI key, or any other key **cannot** authorize initialization on
  behalf of a *different* administrator. If the intended admin is not the
  deployer, that admin must submit the `initialize` transaction themselves
  (e.g. `stellar contract invoke --source <admin-key> ... -- initialize ...`).

### Deployer vs. administrator

| Scenario | Whoever may call `initialize` |
|----------|-------------------------------|
| Admin == deployer | The deployer (single signed transaction) |
| Admin != deployer | The admin identity only — the deployer cannot authorize for it |

`scripts/deploy-testnet.sh` handles both: when `INVOICE_LIQUIDITY_ADMIN`
matches the deployer it initializes automatically; otherwise it prints the exact
`stellar contract invoke --source <admin-key>` command to run manually.

---

## 3. Replay safety and competing initializers

Initialization is **one-shot and fail-closed**:

- **Authoritative marker.** `DataKey::Initialized` is written *before* any other
  state (checks-effects-interactions), so once setup begins the instance is
  already marked and a competing call fails deterministically.
- **Legacy signal detection.** `storage::is_initialized` additionally treats the
  historical `Admin` / `Config` / `NextInvoiceId` / `InvoiceCount` keys as
  "already initialized". An instance created before #1162 therefore **cannot be
  re-initialized after an upgrade**, closing the takeover path on existing
  deployments.
- **Deterministic failure.** Any repeated or competing call returns
  `ContractError::AlreadyInitialized` (code `16`) rather than silently
  succeeding or partially overwriting admin.

The `Initialized` key is **appended last** in the `DataKey` enum on purpose:
`#[contracttype]` encodes variants by position, so inserting it earlier would
renumber every following key and corrupt existing storage. See
[storage-layout.md](./storage-layout.md).

### Invariants

| ID | Invariant |
|----|-----------|
| INV-INIT1 | `initialize` succeeds only when `admin.require_auth()` passes. |
| INV-INIT2 | After a successful `initialize`, a second call returns `AlreadyInitialized`. |
| INV-INIT3 | A competing caller (different signer/address) can never change the stored `Admin`. |
| INV-INIT4 | Any pre-existing initialization signal (marker **or** legacy key) blocks re-initialization. |
| INV-INIT5 | A reverted `initialize` commits **no** authority (marker/admin/config) and **no** token state (approvals/decimals/list). |
| INV-INIT6 | A successful `initialize` commits authority *and* fully registered bootstrap tokens together; `get_initialization_state().consistent` is then `true`. |

---

## 4. Write-sequence hardening and partial-setup detection

Issue #1166 hardens the *write sequence* itself and makes the
"all-or-nothing" property externally verifiable.

### 4.1 Post-condition guard in `initialize`

After every write, `initialize` re-checks the instance with
`storage::initialization_is_consistent` and returns
`ContractError::InitializationInconsistent` (code `53`) if authority and the
three bootstrap tokens are not *all* present. Because a Soroban call that
returns `Err` rolls back the entire invocation, this means an instance that
carries **partial authority or token state can never be committed** — even if a
future maintainer reorders the writes or inserts a fallible step between them.
The `Initialized` event is therefore only emitted on a fully consistent setup.

### 4.2 Verifying an instance in the field

`get_initialization_state()` (Access: **Anyone**) returns an
`InitializationState` snapshot so operators and indexers can confirm an instance
was not left half-configured:

| Field | Meaning |
|-------|---------|
| `initialized` | Any initialization signal is present (see `is_initialized`). |
| `marker_set` | The authoritative `Initialized` marker is present. |
| `admin_set` | An `Admin` is recorded. |
| `config_set` | A `Config` is recorded. |
| `token_list_len` | Number of tokens in the `TokenList` (`0` = none). |
| `consistent` | Authority **and** all three bootstrap tokens (approved, with decimals) are present. |

The snapshot is O(1): it inspects instance keys plus only the three bootstrap
tokens named by `Config`, so it never iterates the (unbounded) approved-token
list.

> **Residual risk.** `consistent` asserts the three *bootstrap* tokens are fully
> registered; it does not audit tokens added later via `add_token`, nor does it
> re-derive each token's on-chain `decimals()` (see
> [token-decimals-audit](./token-decimals-audit.md) for that
> per-token check). A `false` `consistent` on an `initialized == true` instance
> indicates a partial setup to investigate; it is not, by itself, proof of a
> security incident.

---

## 5. Failure recovery

| Situation | Symptom | Recovery |
|-----------|---------|----------|
| Init transaction rejected (auth) | `Unauthorized` | Re-run `initialize` **from the intended admin key**. |
| Init ran but the app never recorded success | Chain shows `AlreadyInitialized` on retry | Initialization **already succeeded**; no action needed — verify via `get_admin`. |
| Partial/reverted setup | Transaction reverted | Nothing was committed (Soroban transactions are atomic); re-run `initialize`. Confirm with `get_initialization_state()` — a reverted call leaves `initialized == false`. |
| Partial setup committed by an older build | `get_initialization_state()` shows `initialized == true` but `consistent == false` | Investigate which signal is missing via the snapshot fields; recover through the documented admin/governance flow (§4.2) — `initialize` cannot re-run. |
| Wrong admin was set (pre-#1162 instance) | `get_admin` returns an unexpected address | Rotate admin via the governance/admin flow (see [multisig-admin-runbook.md](./multisig-admin-runbook.md)); `initialize` cannot re-run. |
| Admin identity differs from deployer | Deployer-init fails with auth error | The admin must submit `initialize` themselves (see §2). |

Because the transaction is atomic, there is no partially-initialized on-chain
state to clean up: a failed `initialize` leaves the instance exactly as it was
(INV-INIT5).

---

## 6. Operational scripts

- `scripts/deploy-testnet.sh` — performs the **authenticated** `initialize`
  step immediately after deployment (tolerating `AlreadyInitialized` as a
  no-op), or prints the manual command when admin ≠ deployer.
- `scripts/seed.ts` — initializes only if `get_invoice_count` is unavailable;
  its admin argument is the same key that signs the transaction.
- `scripts/smoke-test.ts` — same pattern; the `admin` argument equals the
  signing key.

---

## 7. Tests

Coverage lives in
[tests_initialization_hardening.rs](../contracts/invoice_liquidity/src/tests_initialization_hardening.rs):

Issue #1162:

- `initialize_requires_admin_authorization`
- `initialize_succeeds_when_admin_authorizes`
- `repeated_initialize_fails_deterministically`
- `competing_initializer_cannot_seize_admin`
- `explicit_marker_alone_blocks_reinitialization`
- `legacy_initialized_deployment_cannot_be_reinitialized`

Issue #1166:

- `reverted_initialize_leaves_no_partial_authority_or_token_state`
- `successful_initialize_commits_complete_consistent_state`
- `initialization_state_view_detects_partial_setup`
- `competing_initialize_leaves_authority_and_token_state_intact`

---

## 8. References

- Issue #1162 — Secure deploy-time initialization and prevent unauthorized admin selection
- Issue #1166 — Harden `initialize` storage writes against partial setup
- [storage-layout.md](./storage-layout.md)
- [upgrade-guide.md](./upgrade-guide.md)
- [multisig-admin-runbook.md](./multisig-admin-runbook.md)
