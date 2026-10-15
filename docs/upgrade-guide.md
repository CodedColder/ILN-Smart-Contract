# ILN Contract Upgrade Guide

**Document Version:** 1.0  
**Date:** May 2024  
**Status:** Pre-Audit  

## Overview

The Invoice Liquidity Network (ILN) smart contract supports upgrades via WASM hash replacement. This guide documents the complete upgrade procedure, including access control, state migration, rollback procedures, and verification steps.

---

## Key Concepts

### What is a Contract Upgrade?

A Soroban contract upgrade replaces the executable code (WASM binary) while preserving all persistent state. The contract's storage and data remain intact, allowing a seamless transition to new functionality.

**Contract State Preserved During Upgrade:**
- All invoice records (created invoices, status, funding history)
- Reputation scores (payer and LP reputations)
- Configuration parameters (decay rates, fee rates)
- Admin address and control settings
- Fund queues, appeals, and dispute records

**What Can Be Changed:**
- Contract logic and behavior
- New functions and features
- Bug fixes and security patches
- Event signatures (within limitations)

**What Cannot Be Changed:**
- Storage layout of existing data structures (unless data migration is performed)
- Data type sizes in struct fields (e.g., `u32` → `u64`)
- Persistent storage keys (must remain compatible)

---

## Upgrade Process

### Phase 1: Pre-Upgrade Validation (Mainnet)

Before initiating an upgrade, the following checklist **must** be completed:

#### 1.1 Code Audit & Testing
- [ ] New WASM binary has passed security audit
- [ ] All test suites pass (unit, integration, property-based tests)
- [ ] Regression tests confirm backward compatibility
- [ ] No breaking changes to external contract interfaces
- [ ] Gas costs validated for critical operations

#### 1.2 State Migration Planning
- [ ] Current state dump captured from blockchain
- [ ] If schema changes required: migration logic designed and tested
- [ ] Rollback state snapshot prepared
- [ ] Data compatibility verified (e.g., no truncation of numbers)

#### 1.3 Documentation & Communication
- [ ] Upgrade notes published (what changed, why, benefits)
- [ ] LP and freelancer notifications sent (upgrade window, any user action required)
- [ ] Governance decision recorded (if governance-controlled upgrade)
- [ ] Change log entry added to repository

#### 1.4 Operational Readiness
- [ ] Testnet upgrade performed and validated
- [ ] Rollback procedure tested
- [ ] Admin team trained on upgrade process
- [ ] Communication channels ready for support (Discord, forums)
- [ ] Monitoring and alerting configured for post-upgrade

#### 1.5 Legal & Compliance (if applicable)
- [ ] Upgrade approved by governance (if applicable)
- [ ] Terms of service updated (if user-facing changes)
- [ ] Tax implications reviewed (if economic parameters change)

---

### Phase 2: Testnet Validation (Recommended)

**Goal:** Verify the upgrade works correctly before touching mainnet.

**Process:**

```bash
# 1. Build the new WASM binary
cargo build --release --target wasm32-unknown-unknown

# 2. Compute WASM hash
WASM_HASH=$(sha256sum target/wasm32-unknown-unknown/release/invoice_liquidity.wasm | cut -d' ' -f1)
echo "New WASM Hash: $WASM_HASH"

# 3. Deploy to testnet
soroban contract deploy --network testnet \
  --source-account ADMIN_KEY \
  --wasm target/wasm32-unknown-unknown/release/invoice_liquidity.wasm

# 4. Call upgrade function on testnet
soroban contract invoke \
  --id CONTRACT_ID \
  --network testnet \
  --source-account ADMIN_KEY \
  -- upgrade \
  --new_wasm_hash "$WASM_HASH"

# 5. Verify state integrity after upgrade
# - Check invoice counts match pre-upgrade
# - Verify sample invoice data is intact
# - Confirm reputation scores unchanged
# - Test new functionality (if applicable)

# 6. Perform smoke tests
# - Submit new invoice
# - Fund existing invoice
# - Mark invoice as paid
# - Query contract stats
```

**Success Criteria:**
- ✅ All invoices readable and unchanged
- ✅ Reputation scores intact
- ✅ New functions work as expected
- ✅ Events are emitted correctly
- ✅ No state inconsistencies detected

---

### Phase 3: Mainnet Upgrade

#### 3.1 Governance Approval (If Required)

If the contract is controlled by a governance token or multi-sig admin:

1. **Publish Upgrade Proposal**
   - Title: "ILN Contract Upgrade: [Brief Description]"
   - Description: Link to detailed upgrade notes
   - WASM Hash: Include for verification
   - Implementation: Call `upgrade(new_wasm_hash)` after approval

2. **Voting Period**
   - Vote duration: Typically 3-7 days
   - Quorum: As specified in governance contract
   - Approval threshold: As specified in governance contract

3. **Approval Confirmation**
   - Record governance decision on-chain
   - Document voting results
   - Publish rationale for transparency

#### 3.2 Execute Upgrade

```bash
# 1. Build final release binary
cargo build --release --target wasm32-unknown-unknown

# 2. Compute WASM hash (MUST match governance proposal)
WASM_HASH=$(sha256sum target/wasm32-unknown-unknown/release/invoice_liquidity.wasm | cut -d' ' -f1)
echo "Final WASM Hash: $WASM_HASH"

# Verify against governance proposal
# WASM_HASH should match the one voters approved

# 3. Call upgrade on mainnet
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  --source-account ADMIN_KEY \
  -- upgrade \
  --new_wasm_hash "$WASM_HASH"

# 4. Confirm event emission
soroban events \
  --network mainnet \
  --start-ledger CURRENT_LEDGER | grep "ContractUpgraded"
```

#### 3.3 Post-Upgrade Validation

Immediately after upgrade completes:

```bash
# 1. Verify contract state integrity
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  -- get_contract_stats

# Expected output: 
# - total_invoices should match pre-upgrade value
# - total_funded should match pre-upgrade value
# - total_paid should match pre-upgrade value

# 2. Spot-check sample invoices
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  -- get_invoice \
  --invoice_id 1

# 3. Monitor on-chain events
# Watch for errors in ContractUpgraded event
# Confirm no unexpected error events

# 4. Test new functionality (if applicable)
# Submit test invoice
# Fund test invoice
# Verify new features work

# 5. Check application/wallet status
# Ensure all downstream systems (frontends, indexers) handle upgrade
```

---

## Verification Procedures

### WASM Hash Verification

**Why:** Ensure the uploaded WASM binary matches the approved code.

**Process:**

```bash
# 1. Download compiled binary from source repository
git clone https://github.com/drips-network/ILN-Smart-Contract.git
cd ILN-Smart-Contract
git checkout <RELEASE_TAG>  # e.g., v1.2.0

# 2. Rebuild locally
cargo build --release --target wasm32-unknown-unknown

# 3. Compute local hash
LOCAL_HASH=$(sha256sum target/wasm32-unknown-unknown/release/invoice_liquidity.wasm | cut -d' ' -f1)

# 4. Fetch on-chain hash from ContractUpgraded event
CHAIN_HASH=$(soroban events \
  --network mainnet \
  --topic "upgraded" | jq -r '.new_wasm_hash')

# 5. Verify match
if [ "$LOCAL_HASH" == "$CHAIN_HASH" ]; then
    echo "✅ WASM hash verified!"
else
    echo "❌ WASM hash mismatch!"
    echo "Local:  $LOCAL_HASH"
    echo "Chain:  $CHAIN_HASH"
    exit 1
fi
```

### State Compatibility Checks

**Data Type Changes:** ⚠️ **BREAKING CHANGES**

If the upgrade includes changes to struct field types, manual state migration is required:

```rust
// ❌ BREAKING: This change requires migration
// pub discount_rate: u32,  // Old
pub discount_rate: u64,    // New (larger type)

// ✅ SAFE: This change is backward compatible
// pub amount_funded: i128,
pub total_amount_funded: i128,  // Different field name, old field still exists
```

**Safe Changes (No Migration Needed):**
- ✅ Adding new fields (with default values for existing records)
- ✅ Removing unused fields (old data is ignored)
- ✅ Changing function implementations (not called during storage reads)
- ✅ Adding new event types

**Unsafe Changes (Migration Required):**
- ❌ Changing struct field types (e.g., `u32` → `u64`)
- ❌ Reordering struct fields
- ❌ Changing field names (existing data uses old keys)
- ❌ Changing enum variants

**Migration Pattern (if needed):**

```rust
// During initialization or first-call-after-upgrade:
pub fn migrate_state(env: Env) -> Result<(), ContractError> {
    require_admin(&env)?;
    
    // Read old state
    let old_score = env.storage()
        .persistent()
        .get::<_, u32>(&StorageKey::OldPayerScore(payer.clone()))
        .unwrap_or(50);
    
    // Transform and write new state
    let new_rep = ReputationScore {
        score: old_score,
        last_activity_ledger: env.ledger().sequence(),
    };
    env.storage()
        .persistent()
        .set(&StorageKey::PayerScore(payer.clone()), &new_rep);
    
    Ok(())
}
```

---

## Rollback Procedure

**Use Case:** If post-upgrade issues are discovered and a rollback is necessary.

### 3-Step Rollback Process

#### Step 1: Assess the Issue

```
Decision Tree:
├─ Data Corruption?
│  ├─ YES → Rollback required (cannot fix without reverting)
│  └─ NO → Continue to Step 2
├─ Critical Functionality Broken?
│  ├─ YES → Rollback recommended
│  └─ NO → Continue to Step 2
└─ Can Issue Be Fixed with Hotfix?
   ├─ YES → Deploy hotfix (new upgrade)
   └─ NO → Proceed with rollback
```

#### Step 2: Prepare Rollback

```bash
# 1. Identify the last stable WASM hash
STABLE_HASH="abc123..."  # From pre-upgrade records

# 2. Rebuild the stable version from git tag
git checkout v1.1.0  # The previous stable version
cargo build --release --target wasm32-unknown-unknown
VERIFY_HASH=$(sha256sum target/wasm32-unknown-unknown/release/invoice_liquidity.wasm | cut -d' ' -f1)

# Verify hash matches records
if [ "$VERIFY_HASH" != "$STABLE_HASH" ]; then
    echo "❌ Rollback hash verification failed!"
    exit 1
fi
```

#### Step 3: Execute Rollback

```bash
# Call upgrade with the previous WASM hash
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  --source-account ADMIN_KEY \
  -- upgrade \
  --new_wasm_hash "$STABLE_HASH"

# Verify rollback
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  -- get_contract_stats
```

### Rollback Impact Analysis

| Data Element | Impact | Mitigation |
|---|---|---|
| **New Invoices** (created after failed upgrade) | Lost (no longer compatible) | Require users to re-submit |
| **Existing Invoices** | Recovered (state intact) | No action needed |
| **Reputation Scores** | Recovered | No action needed |
| **Fund Queues** | Recovered (may be stale) | May need to refresh |

**Communication After Rollback:**
1. Notify all users via Discord/Twitter
2. Publish post-mortem analysis (what failed, root cause)
3. Commit to fixed timeline for re-attempt
4. Offer support for affected transactions

---

## State Snapshot & Recovery

### Creating Pre-Upgrade Snapshot

```bash
# Run before upgrade execution
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  -- get_contract_stats > pre_upgrade_stats.json

# Dump first 100 invoices
for i in {1..100}; do
    soroban contract invoke \
      --id CONTRACT_ID \
      --network mainnet \
      -- get_invoice \
      --invoice_id $i >> pre_upgrade_invoices.json 2>/dev/null || true
done

# Save to version control
git add pre_upgrade_stats.json pre_upgrade_invoices.json
git commit -m "Pre-upgrade snapshot at ledger $LEDGER_HEIGHT"
```

### Validating Post-Upgrade State

```bash
# Rerun same queries
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  -- get_contract_stats > post_upgrade_stats.json

# Compare (should be identical)
diff pre_upgrade_stats.json post_upgrade_stats.json
# If diff is empty: ✅ State intact
# If diff found: ❌ Data loss or corruption
```

---

## Emergency Procedures

### If Upgrade Fails (During Execution)

1. **Do Not Panic** — State remains unchanged until confirmed on-chain
2. **Wait for Confirmation** — Soroban network may take time to process
3. **Check Event Logs**
   ```bash
   soroban events --network mainnet --topic "upgraded" | tail -20
   ```
4. **If No Event After 5 Minutes:**
   - Network may have rejected the upgrade (not authorized, invalid signature)
   - Retry with correct admin credentials
5. **If Event Shows Error:**
   - Check admin status and auth
   - Verify WASM hash format (must be 32 bytes)
   - Check contract balance for fees

### If Data Corruption is Detected

1. **Stop all operations** — Call `pause()` immediately
2. **Notify community** — Publish incident notice
3. **Assess scope:**
   ```bash
   soroban contract invoke \
     --id CONTRACT_ID \
     --network mainnet \
     -- get_contract_stats
   ```
4. **Decision:**
   - Minor issue → Deploy hotfix upgrade
   - Major issue → Rollback to previous version
   - Unknown issue → Fork to testnet for diagnosis

---

## Parameters & Configuration

### Safe Configuration Changes During Upgrade

The following parameters can be safely updated post-upgrade without breaking contract:

```rust
// Can be updated without upgrade
pub fn update_fee_rate(env: Env, rate: u32)
pub fn update_max_discount(env: Env, rate: u32)
pub fn update_config(env: Env, high_rep_threshold: u32, ...)

// Requires upgrade if logic needs to change
pub fn submit_invoice(...)  // Business logic
pub fn fund_invoice(...)    // Core mechanics
```

**Best Practice:** Use governance to change parameters, reserve upgrades for code logic changes.

---

## Monitoring Post-Upgrade

### Key Metrics to Monitor

| Metric | Target | Frequency | Alert Threshold |
|--------|--------|-----------|-----------------|
| **Invoice Submission Rate** | No sudden drop | 1 hour | <50% of baseline |
| **Fund Success Rate** | >99% | 1 hour | <98% |
| **Default Rate** | Baseline ±5% | 1 day | >10% deviation |
| **Transaction Latency** | <2s | 5 minutes | >5s |
| **Event Emission** | 100% | 1 hour | Missing events |

### Off-Chain Monitoring

```bash
# Monitor invoice submissions
while true; do
  soroban events \
    --network mainnet \
    --topic "submitted" \
    --start-ledger $(date +%s) | wc -l
  sleep 3600  # Every hour
done

# Monitor errors
soroban events \
  --network mainnet \
  --type "error" \
  --start-ledger UPGRADE_LEDGER | jq .

# Monitor fund queue resolutions
soroban events \
  --network mainnet \
  --topic "fund_queue_resolved" | tail -20
```

---

## Governance & Approval

### Multi-Sig Upgrade (Recommended)

For mainnet deployments, use a multi-sig admin:

```bash
# Create upgrade proposal (2-of-3 multi-sig)
soroban contract invoke \
  --id MULTISIG_CONTRACT \
  -- propose_upgrade \
  --target_contract CONTRACT_ID \
  --new_wasm_hash "$WASM_HASH" \
  --description "Upgrade ILN to v1.2: Bug fixes and new features"

# Signers review and vote
# After 2+ approvals:
soroban contract invoke \
  --id MULTISIG_CONTRACT \
  --source-account SIGNER1 \
  -- execute_upgrade \
  --proposal_id $PROPOSAL_ID
```

### DAO Upgrade (Future)

Once governance DAO is deployed:

1. Create governance proposal (Snapshot voting)
2. Warm-up period (discussion)
3. Voting period (3-7 days)
4. Time-lock (24-48 hours)
5. Execute on-chain

---

## Troubleshooting

### Common Issues

#### Issue: "Invalid WASM Hash Format"

```
Error: invalid_wasm_hash
```

**Cause:** WASM hash is not 32 bytes (256 bits)

**Solution:**
```bash
# Verify hash is correct format
echo "$WASM_HASH" | wc -c  # Should be 65 (64 hex chars + newline)
# If wrong, recompute:
sha256sum target/wasm32-unknown-unknown/release/invoice_liquidity.wasm
```

#### Issue: "Admin Not Authorized"

```
Error: Unauthorized
```

**Cause:** Caller is not the contract admin

**Solution:**
```bash
# Verify admin address
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  -- get_admin

# If using multi-sig, ensure all signers have signed
# If using single admin, verify private key is correct
```

#### Issue: "Contract State Not Found Post-Upgrade"

```
Error: ContractNotFound
```

**Cause:** Network hasn't finalized upgrade yet or wrong contract ID

**Solution:**
```bash
# Wait 30 seconds and retry
sleep 30

# Verify contract ID
echo "Contract ID: $CONTRACT_ID"

# Check contract exists
soroban contract info --id $CONTRACT_ID --network mainnet

# If still failing, contact Stellar support
```

---

## Best Practices & Recommendations

### ✅ Do's

- ✅ **Always test on testnet first** before mainnet upgrades
- ✅ **Capture pre-upgrade state snapshot** for rollback
- ✅ **Publish upgrade notes** with clear explanation of changes
- ✅ **Use multi-sig admin** for mainnet to prevent unauthorized upgrades
- ✅ **Implement time-locks** for governance-controlled upgrades
- ✅ **Monitor metrics post-upgrade** for anomalies
- ✅ **Maintain rollback readiness** for 48 hours after upgrade
- ✅ **Document all upgrades** with reason, date, WASM hash

### ❌ Don'ts

- ❌ **Don't upgrade without testing** on testnet first
- ❌ **Don't change struct field types** without data migration
- ❌ **Don't upgrade during peak usage** (e.g., end-of-month invoicing rush)
- ❌ **Don't skip governance approval** if DAO-controlled
- ❌ **Don't forget communication** to users about upgrades
- ❌ **Don't delete monitoring** until 1 week post-upgrade stability
- ❌ **Don't assume rollback won't be needed** — always prepare

---

## Support & Contact

For upgrade-related questions or issues:

1. **GitHub Issues:** https://github.com/drips-network/ILN-Smart-Contract/issues
2. **Discord:** [Link to community Discord]
3. **Email:** security@drips.network (for security issues)

---

## Appendix: Upgrade Checklist

```markdown
# ILN Contract Upgrade Checklist v1.0

## Pre-Upgrade (Mainnet)
- [ ] Security audit completed
- [ ] All tests passing (unit, integration, fuzzing)
- [ ] Testnet upgrade successful
- [ ] State snapshot captured
- [ ] Governance approval obtained (if required)
- [ ] Admin credentials secured
- [ ] Stakeholder communication sent
- [ ] Rollback procedure tested

## Upgrade Execution
- [ ] WASM binary built and verified
- [ ] WASM hash computed and confirmed
- [ ] Admin auth signatures collected
- [ ] Upgrade transaction signed
- [ ] Upgrade transaction submitted
- [ ] Upgrade event confirmed on-chain

## Post-Upgrade (First Hour)
- [ ] Contract state integrity verified
- [ ] Sample invoices spot-checked
- [ ] New functionality tested (if applicable)
- [ ] No error events observed
- [ ] Monitoring and alerts active
- [ ] Team standing by for support

## Post-Upgrade (First 24 Hours)
- [ ] All downstream systems operational (frontends, indexers, APIs)
- [ ] User transactions processed normally
- [ ] Reputation scores stable
- [ ] Fund queue resolutions working
- [ ] Dispute/appeal system functional
- [ ] No unusual contract behavior

## Post-Upgrade (Stabilization)
- [ ] 1 week of stable operation confirmed
- [ ] Monitoring reduced to normal levels
- [ ] Rollback standby ended
- [ ] Upgrade documentation complete
- [ ] Post-mortem (if any issues) published
- [ ] Archive pre-upgrade snapshot

```

---

## v1 → v2 State Migration (Script) — Issue #114

The v1 → v2 upgrade adds three fields to the persisted `Invoice` schema
(`submitter_reputation`, `allowed_lps`, `is_auction`). Because Soroban preserves
raw storage across a WASM-hash swap, existing `Invoice` records must be read and
re-written with the new fields populated (defaulted) or they will fail to decode
under v2. The migration is automated and verified by
[`scripts/migrate-v1-v2.ts`](../scripts/migrate-v1-v2.ts).

### End-to-end test against mainnet-shaped state (Issue #652)

`--simulate` no longer exercises a handful of hand-written fixtures — it builds a
mainnet-shaped pre-migration dataset: 500 deterministically generated invoices
spread across all six lifecycle states, plus explicit boundary-value invoices
(minimum unit amount, a near-whale amount, 0%/max discount rate). Funded/paid
amounts are kept internally consistent with each invoice's status, the way real
ledger state would be. The freelancer pool is large enough (40 addresses) that
roughly a fifth of invoices belong to a freelancer with no reputation entry,
so the `DEFAULT_REPUTATION` fallback path is exercised at scale, not just for
one fixture row. `verifyMigration()` then checks losslessness across the full
set, not a sample.

The generator (`sampleV1State()` in `scripts/migrate-v1-v2.ts`) uses a seeded
PRNG (`mulberry32`), so the dataset — and therefore CI's pass/fail outcome — is
reproducible across runs.

### The lossless transform

The single source of truth is the pure `migrateInvoice()` function:

| v2 field | Default applied |
|----------|-----------------|
| `submitter_reputation` | the freelancer's current reputation score, else `50` |
| `allowed_lps` | `null` (invoice stays public) |
| `is_auction` | `false` |

All v1 fields are carried over byte-for-byte; the migration never drops or
mutates existing data.

### Modes

```bash
# Default: in-memory simulation — deterministic, no network, no deps.
# Proves the transform is lossless across all invoice statuses. Exits 0.
npx tsx scripts/migrate-v1-v2.ts

# Real testnet run (requires @stellar/stellar-sdk + funded keys):
npx tsx scripts/migrate-v1-v2.ts --testnet
```

The `--simulate` path is suitable for CI so a regression in the migration logic
fails the build before any on-chain run.

### Testnet environment variables (`--testnet`)

| Variable | Purpose |
|----------|---------|
| `ADMIN_SECRET` | Secret key of the contract admin (signs `upgrade` + `migrate`). |
| `SOROBAN_RPC_URL` | RPC endpoint (default `https://soroban-testnet.stellar.org`). |
| `NETWORK_PASSPHRASE` | Network passphrase (default Testnet). |
| `V1_WASM` / `V2_WASM` | Paths to the built v1 / v2 WASM artifacts. |
| `V2_WASM_HASH` | Hash passed to `upgrade(new_wasm_hash)`. |

### On-chain procedure

1. Install + deploy v1 WASM; `initialize` the contract.
2. Seed sample state: `submit_invoice` across all statuses.
3. Snapshot v1 state (`get_invoice`, `get_invoice_count`).
4. Install v2 WASM; call `upgrade(V2_WASM_HASH)`.
5. Invoke the v2 `migrate` entrypoint to re-write `Invoice` records via
   `migrateInvoice()`.
6. Verify: counts match the snapshot, new fields carry their defaults, and v2-only
   calls (e.g. `get_invoice_count(&Some(status))`) succeed.

---

## Upgrade Migration Sequencing & State Versioning (Issue #1086)

### 1. Overview & Baseline Context

When upgrading the `invoice_liquidity` contract via WASM hash replacement, persistent storage state is preserved by the host environment. However, evolving data layouts (such as schema bumps, new fields, or the transition from monolithic `Invoice(u64)` storage to the hot/cold split `InvoiceCore(u64)` + `InvoiceMetadata(u64)`) require a deterministic, fail-closed operational procedure.

Without documented migration sequencing, operators risk:
1. Executing user transactions against incomplete or unmigrated schema layouts.
2. Race conditions between post-upgrade calls and schema migration.
3. Inadvertent double-migrations or partial state overwrites.
4. Silent fallback or authorization bypass during intermediate migration states.

### 2. The 6-Step Migration Sequencing Procedure

Every contract upgrade requiring state or schema migration must follow this exact sequential order:

```text
[1. Pre-Upgrade Snapshot & Pause]
            │
            ▼
[2. WASM Replacement (upgrade)]
            │
            ▼
[3. Schema Migration (migrate)]
            │
            ▼
[4. Dual-Format & Schema Validation]
            │
            ▼
[5. Operational Resumption (unpause)]
            │
            ▼
[6. Post-Upgrade Verification & Monitoring]
```

#### Step 1: Pre-Upgrade State Snapshot & Protocol Pause
- Admin calls `pause()` on the `invoice_liquidity` contract.
- Verified on-chain: `is_paused() == true` and `LastPauseTimestamp` is recorded.
- An off-chain state dump captures:
  - Current storage schema version: `get_storage_version()` (e.g. `1`).
  - Total invoice count: `get_total_invoices()`.
  - Token allowlist and decimal configurations.

#### Step 2: Contract WASM Upgrade
- The admin (or multi-sig / governance DAO proposal) invokes `upgrade(new_wasm_hash)`.
- The Soroban host replaces the bytecode while keeping instance and persistent storage untouched.
- At this stage, the protocol remains paused, preventing any non-admin interaction.

#### Step 3: Atomic Schema Migration (`migrate()`)
- The admin invokes the `migrate()` entrypoint:
  - Gated by `require_admin(&env)` — unauthorized callers fail with `ContractError::Unauthorized`.
  - Exemption from economic rate limiting (Issue #859): migration is an emergency/maintenance operation that must execute immediately post-upgrade.
  - Permitted while protocol is paused (verified by `test_upgrade_migration_sequencing_while_paused`).
- Idempotency Guarantee:
  - If `StorageVersion >= CURRENT_STORAGE_VERSION`, `migrate()` safely returns `Ok(current)` as a no-op without re-migrating or altering storage.
  - If `StorageVersion < CURRENT_STORAGE_VERSION`, incremental migration steps execute, `StorageVersion` is updated to `CURRENT_STORAGE_VERSION`, and a `migrated` event `(old_version, new_version, timestamp)` is emitted.

#### Step 4: Dual-Format Decoding & State Validation
- Verify backward compatibility for pre-upgrade data:
  - Legacy monolithic records stored under `DataKey::Invoice(id)` are decoded transparently by `load_invoice(env, id)` and `try_load_invoice_core(env, id)` using the built-in fallback path.
  - New records written post-upgrade via `save_invoice(env, invoice)` are partitioned into `DataKey::InvoiceCore(id)` (hot path) and `DataKey::InvoiceMetadata(id)` (cold path) with persistent TTL extensions (1,000,000 threshold / 2,000,000 extend-to ledgers).
- Verify `get_storage_version() == CURRENT_STORAGE_VERSION` (version `2`).

#### Step 5: Protocol Unpause & Operations Resume
- Admin invokes `unpause()`.
- Protocol state transitions to active: `is_paused() == false`.
- Normal operations (submitting, funding, settling invoices) resume.

#### Step 6: Rollback & Failure Contingency
- If `migrate()` encounters an unrecoverable failure prior to completion:
  - The contract remains paused.
  - Admin executes a rollback by invoking `upgrade(previous_wasm_hash)`.
  - Because `migrate()` operates atomically and updates `StorageVersion` only upon completion, partial corrupted states are avoided.
  - Legacy records remain readable under previous WASM bytecode due to strict backward compatibility.

### 3. Audited Security & Economic Guarantees

- **No Authorization Bypass:** Both `upgrade()` and `migrate()` require strict admin authentication (`require_admin(&env)`). Non-admin attempts fail closed with `ContractError::Unauthorized`.
- **No Unbounded State Growth:** Migration relies on lazy on-write migration combined with backward-compatible reads, avoiding block-gas-limit-exhausting batch loops over historical storage.
- **Fail-Closed Compatibility:** Invoices with missing or corrupt fields trigger typed contract errors (`ContractError::InvoiceNotFound`, `ContractError::StorageError`) and never silently return zeroed or malformed values.

---

**Document Prepared By:** DevOps & Security Team  
**Last Updated:** October 2026 (upgrade migration sequencing added — Issue #1086)  
**Next Review:** Upon next upgrade
