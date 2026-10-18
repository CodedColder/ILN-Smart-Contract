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

#### 1.0 TTL Field Liveness Check (Issue #1082)

Starting with this release `upgrade()` enforces a **fail-closed TTL guard**
before swapping the WASM binary.  The guard samples the most-recent 50
invoices (`TTL_UPGRADE_SAMPLE_SIZE`) and checks five categories of persistent
storage entry:

| Key category | TTL managed by |
|---|---|
| `InvoiceCore(id)` | `storage::save_invoice` |
| `InvoiceMetadata(id)` | `storage::save_invoice` |
| `FundQueue(id)` | `storage::save_fund_queue` |
| `FundQueueOpenedAt(id)` | `storage::try_set_fund_queue_opened_at` |
| `QueueResolution(id)` | `storage::save_queue_resolution` |

If any of the above entries has a remaining TTL below
`TTL_MIN_LEDGERS_BEFORE_UPGRADE` (500,000 ledgers ≈ 29 days) the call returns
`Err(ContractError::UpgradeRemovesTtlFields)` and **the WASM is not swapped**.

**Pre-upgrade TTL check command:**

```bash
# Query a sample of InvoiceCore TTLs.  All must show >= 500000 remaining.
# (Substitute real contract and invoice IDs.)
for id in $(seq $((NEXT_ID-50)) $NEXT_ID); do
  soroban contract invoke \
    --id $CONTRACT_ID \
    --network mainnet \
    -- get_invoice --invoice_id $id 2>/dev/null | jq '.id // empty' && echo "OK $id"
done
```

**Remediation if the guard fires:**

```bash
# Re-save affected invoices to refresh their TTL.
# save_invoice extends InvoiceCore and InvoiceMetadata to 2_000_000 ledgers.
soroban contract invoke \
  --id $CONTRACT_ID \
  --network mainnet \
  --source-account $ADMIN_KEY \
  -- refresh_invoice_ttl \
  --invoice_id $AFFECTED_ID

# For queue entries, call the appropriate helper.
soroban contract invoke \
  --id $CONTRACT_ID \
  --network mainnet \
  --source-account $ADMIN_KEY \
  -- refresh_queue_ttl \
  --invoice_id $AFFECTED_ID
```

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

### Step 0: Rehearse the Rollback (Issue #1083)

**Before initiating any rollback**, call `rehearse_rollback()` to verify that
the current storage state is rollback-safe.  This is a **read-only** check
that does not modify storage.

```bash
# Perform a rollback rehearsal — safe to call at any time.
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  --source-account ADMIN_KEY \
  -- rehearse_rollback
```

**Interpreting the result:**

```json
{
  "invoices_checked": 50,
  "split_format_count": 50,
  "legacy_format_count": 0,
  "orphaned_count": 0,
  "rollback_safe": true
}
```

* `rollback_safe: true` — proceed with the rollback (Steps 1-3 below).
* `rollback_safe: false` / `orphaned_count > 0` — **do NOT roll back.**  One
  or more invoices have only one half of the split storage format.  Rolling
  back to a v1 WASM would make those invoices silently inaccessible.

**Remediation when rollback is unsafe:**

```bash
# Re-save each affected invoice to restore both split halves.
# This is safe to call while the contract is live.
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  --source-account ADMIN_KEY \
  -- save_invoice \
  --invoice_id $AFFECTED_ID

# Re-run the rehearsal; it must report rollback_safe: true before proceeding.
soroban contract invoke \
  --id CONTRACT_ID \
  --network mainnet \
  --source-account ADMIN_KEY \
  -- rehearse_rollback
```

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
| **Existing Invoices — split format** | Silently inaccessible in v1 WASM if either half is orphaned | Run `rehearse_rollback()` first; repair orphans before rolling back |
| **Existing Invoices — legacy format** | Recovered (v1 WASM reads `Invoice(id)` directly) | No action needed |
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
- [ ] **TTL liveness verified** — all InvoiceCore/InvoiceMetadata/FundQueue entries in the most-recent 50 invoices have ≥ 500,000 ledgers of remaining TTL (Issue #1082)
- [ ] **Rollback rehearsal passed** — `rehearse_rollback()` returns `rollback_safe: true` with `orphaned_count: 0`; no split-format orphans detected in the most-recent 50 invoices (Issue #1083)
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

## v2 → v3: Token Decimals Backfill (Issue #1070)

Submit, batch submit and `convert_invoice_token` no longer fall back to 6
decimals when a token has no registered precision. Deployments initialized
before this change have no `TokenDecimals` entry for EURC.

1. `upgrade(V3_WASM_HASH)`.
2. Immediately call `migrate()`, which returns `3`. It writes only missing
   decimals for the bootstrap tokens (USDC 6, EURC 6, XLM 7, matching the
   previous effective behavior) and never overwrites an existing entry.
3. Verify with `get_token_decimals` for USDC, EURC and XLM.

Until step 2 runs, EURC invoices fail with `TokenDecimalsNotConfigured` (70).
This is fail-closed and needs no funds recovery. See
[token-safety.md](token-safety.md#upgrade-and-migration) for the pending-payout
escrow introduced in the same release, and its rollback caveat.

## Oracle Contracts: Feeder Authorization (Issue #1046)

This change is scoped to the **external payer-verification oracle contract**,
not `invoice_liquidity`. The ILN never calls the oracle's `update_verification`
write path, so:

- **invoice_liquidity**: no Wasm behavior change, no storage key/encoding change,
  no new error code — **no migration required**. A plain `upgrade()` (or no
  action at all) suffices.
- **Oracle contracts**: if you operate a payer-verification oracle, its
  `update_verification` must be authorization-gated on a persisted **feeder**
  (operator) address (see
  [oracle-integration.md](oracle-integration.md#authorizing-the-feeder-issue-1046)).

Operator rollout steps:

1. Deploy the updated oracle contract that persists a feeder and requires
   `feeder.require_auth()` on `update_verification` (rejecting when unset).
2. Configure the feeder address once at deployment (`set_feeder` in the
   reference implementation) using the operator key — a dedicated key, not the
   ILN admin key.
3. Confirm `update_verification` rejects an unsigned/unauthorized call and a
   call made before the feeder is configured.
4. If the oracle was already registered with ILN, no re-registration is needed
   — the interface version is unchanged; only the write path's authorization is
   tightened.

**Rollback caveat.** Reverting a conforming oracle back to an **unrestricted**
`update_verification` re-opens the "anyone can mint funding eligibility" hole
(Issue #1046). Do not roll back to an unauthorized oracle build while
`require_oracle_verification = true`.

## Oracle Feeder Disagreement: Multi-Oracle Quorum (Issue #1047)

This change **is** in `invoice_liquidity` (unlike #1046, which was confined to
the external oracle contract). It adds two `instance` storage keys
(`IdentityOracles(feed_type)` and `IdentityQuorum`), five entry points, two error
codes (79/80), and the `oracle_feeder_disagreement` event — but it is
**opt-in and fail-safe**:

- **Default behavior is unchanged.** An **empty** Identity-feeder list returns
  `NotConfigured`, so `fund_invoice` runs the exact prior single-oracle path
  (regression #1052 / duplicate #1053 / freshness #1050 / circuit-breaker checks
  intact). `oracle_interface.rs` / `OracleInterface` v1 is untouched, so **no
  re-registration of existing oracles** is needed. The default quorum is 1, so a
  list without an explicit `set_identity_quorum` still only needs one agreement.
- **No data-encoding migration.** The two new keys are absent on an upgraded
  contract and read as "empty list" / "default quorum (1)" — a plain `upgrade()`
  suffices; the storage version is unchanged. `IdentityOracles` is bounded by
  `MAX_IDENTITY_ORACLES_PER_FEED` (20) and declared in `storage.rs`'s
  `RESOURCE_ENVELOPE` (`identity_oracles_per_feed`).

Operator rollout steps (optional — only to enable disagreement safety):

1. Deploy an oracle contract that implements the **existing**
   `get_payer_data` interface for each additional feeder (each feeder is an
   independent deployment; no new method is required).
2. Register each feeder with `add_identity_oracle(feed_type, oracle)` (admin,
   rate-limited). Re-adding an existing feeder is a no-op; adding a genuinely
   new one past 20 fails with `TooManyIdentityOracles`.
3. Set the agreement threshold with `set_identity_quorum(quorum)` (admin,
   rate-limited, valid range `[1, 20]`). Pick a quorum that tolerates one feeder
   being offline (e.g. `2-of-3`), since a **quiet** feeder also withholds its
   agreement.
4. Verify reads with `get_identity_oracles` / `get_identity_quorum`, and monitor
   the `oracle_feeder_disagreement` event — a burst means a feeder is stale,
   misconfigured, or compromised and funding is failing closed.

**Rollback caveat.** Removing feeders (`remove_identity_oracle`) or lowering
`set_identity_quorum` back to 1 reverts the gate to the single-oracle trust
model — reintroducing the single-point-of-failure #1047 addressed. Emptying the
list entirely also re-enables the legacy single-oracle resolution chain.

## Oracle Circuit-Breaker Auto-Recovery (Issue #1048)

This change **is** in `invoice_liquidity`. It adds one persistent storage key
(`OracleCircuitRecovery(feed_type, token)` → `CircuitRecoveryState`), two public
read/entry points (`get_circuit_recovery_state`; `check_oracle_health` now also
drives recovery), and the `oracle_circuit_recovered` event — but it is
**fail-safe and needs no coordinated rollout**:

- **No data-encoding migration.** `OracleCircuitRecovery` is absent on an
  upgraded contract and reads as `None`; the recovery helper treats a missing
  record as **fail-safe** (no auto-recovery). A plain `upgrade()` suffices; the
  storage version is unchanged. An already-tripped breaker therefore still needs
  the admin `reset_oracle_circuit` until it re-trips under the new code.
- **Behavior change is confined to a tripped breaker.** While the breaker is
  closed, `record_oracle_health` does no extra work (the recovery helper returns
  immediately), so the funding happy path is unchanged. While it is tripped,
  `check_oracle_health` (keeper path) can now clear it; `fund_invoice` still
  rejects until then and `reset_oracle_circuit` still force-clears it.
- **Anti-flapping preserved.** A single fresh query never recovers; recovery
  needs `CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` (3) consecutive fresh,
  timestamp-advancing observations from the tripping oracle spanning at least
  `CIRCUIT_RECOVERY_MIN_LEDGERS` (120) ledgers.

Operator rollout steps:

1. Deploy the upgraded `invoice_liquidity` WASM (`upgrade()`); no state
   migration call is required.
2. Ensure a **keeper** is calling `check_oracle_health(feed_type, token, payer)`
   on a regular cadence — this is the permissionless path that drives
   auto-recovery, so an outage nobody observes cannot auto-heal.
3. Add `oracle_circuit_recovered` to your alerting (breaker cleared
   automatically) and poll `get_circuit_recovery_state` to detect a **stuck**
   recovery (e.g. keepers not running, or the tripping oracle replaced) that
   still needs the admin `reset_oracle_circuit`.

**Rollback caveat.** Rolling back the WASM restores the fully sticky breaker
(admin reset only). No data migration is needed for the rollback either — the
`OracleCircuitRecovery` key is simply ignored by the older code and removed the
next time a breaker trips and recovers under the new code.

## Oracle Gate-Failure Events (Issue #1049)

This change **is** in `invoice_liquidity`. It adds one public entry point
(`check_oracle_verification`) and one new event type
(`OracleDataStaleRejected` / `oracle_stale_data_rejected`), and it extracts the
existing identity gate into a shared helper — but it is **additive and
behaviour-preserving**:

- **No data-encoding migration and no new storage key.** The shared helper uses
  the same `OracleHealth` / circuit-breaker state and the same checks in the same
  order; no key, value encoding, error code, or TTL constant changed. A plain
  `upgrade()` suffices; the storage version is unchanged.
- **`fund_invoice` behavior is unchanged.** The gate still returns exactly the
  errors it did before (`OracleDataStale`, `PayerUnverified`,
  `OracleTimestampRegression`, `OracleDuplicateUpdate`, `OracleCircuitOpen`) with
  the same precedence, and still rolls its own events back. The new entry point
  is the only path whose failure events commit.
- **New entry point is authorization-free.** `check_oracle_verification(feed_type,
  token, payer)` is callable by anyone (mirroring `check_oracle_health`), and
  grants no privilege: it runs the read-only gate and records the oracle health
  snapshot.

Operator rollout steps:

1. Deploy the upgraded `invoice_liquidity` WASM (`upgrade()`); no state
   migration call is required.
2. Add `check_oracle_verification(feed_type, token, payer)` to your monitoring
   cadence to observe *why* oracle-gated funding is failing — it returns an
   `IdentityVerificationOutcome` (`Verified` / `Stale` / `QuorumDisagreement` /
   `TimestampRegression` / `DuplicateUpdate` / `CircuitOpen` / `Unverified`).
3. Add `oracle_stale_data_rejected` and `oracle_feeder_disagreement` to your
   alerting; a burst means a feeder is stale, misconfigured, or compromised and
   funding is failing closed.

**Rollback caveat.** Rolling back the WASM removes the diagnostic entry point and
the `oracle_stale_data_rejected` event; no data migration is needed (nothing new
is persisted). Funding behavior is identical on both builds.

## Compromised-Feeder Adversarial Coverage (Issue #1054)

This change is **test- and documentation-only** — it does **not** modify
`invoice_liquidity` (or any contract) at all, so there is nothing to upgrade and
nothing to migrate:

- **No contract, ABI, storage, or error-code change.** The issue adds a focused
  adversarial suite (`test_1054_*` in `tests_oracle_registry.rs`) and a cost
  benchmark (`benchmark_compromised_feeder_rejection_paths` in
  `tests_benchmarks.rs`), both exercising existing entry points. A plain
  `upgrade()` of an already-deployed contract is unaffected; no storage version
  bump, no new key, no redeploy of oracles.
- **Why it still matters operationally.** The suite pins, as executable
  assertions, two properties operators must rely on: (a) a compromised feeder is
  only *contained* (breaker tripped) when a keeper is calling the permissionless
  `check_oracle_verification` — a rejected `fund_invoice` rolls its writes back
  and cannot advance the breaker; and (b) a compromised **majority** of Identity
  feeders genuinely satisfies the quorum and cannot be stopped on chain, so it
  remains a governance `remove_identity_oracle` action. Neither is a migration
  item, but both belong in the operator runbook.

**No rollout steps and no rollback caveat** — nothing is deployed or persisted.
See [`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#17-test-compromised-feeder-data-issue-1054)
and [`oracle-design.md`](oracle-design.md) for the before/after detail.

## Oracle Health & Quorum Views (Issue #1055)

This change adds two **additive, permissionless, read-only** oracle views — it
does **not** change any existing behaviour, so there is nothing to migrate:

- **No behaviour, storage, or error-code change.** `get_oracle_status(feed_type,
  token)` (aggregate of recorded configuration + health) and
  `check_identity_quorum_health(feed_type, token, payer)` (live per-feeder health
  + quorum verdict) are new entry points returning new `#[contracttype]` views
  (`OracleStatusView`, `IdentityQuorumHealth`). No existing key, value encoding,
  error code, or TTL constant changes, so **no storage migration is required** and
  a plain `upgrade()` of an already-deployed contract is unaffected. The only
  touch to a live path is extracting the quorum loop's per-feeder check into the
  shared `oracle_registry::classify_identity_feeder`, which yields the identical
  count.
- **Why it matters operationally.** Operators gain a one-call `/health` probe
  (`get_oracle_status`) that previously required fanning out across five entry
  points, and — the part with no prior reader at all — a live view of the
  multi-oracle Identity quorum (`check_identity_quorum_health`). Add both to the
  post-upgrade monitoring pass: alert on `circuit_tripped`, a stale `health`, or a
  non-empty feeder list with `quorum_met = false`. Both views are read-only and
  need no authorization, and `get_oracle_status` makes no cross-contract calls, so
  neither is affected by pausing.

**No rollout steps and no rollback caveat** — nothing is deployed or persisted.
See [`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#18-expose-oracle-health-and-quorum-issue-1055),
[`oracle-integration.md`](oracle-integration.md#exposing-health-and-quorum-issue-1055)
and [`benchmarks.md`](benchmarks.md#oracle-health--quorum-exposure-cost-issue-1055)
for the before/after detail.

---

## Documented Fail-Closed Oracle Behavior (Issue #1056)

This change **documents** the oracle gate's disposition and adds regression
tests — it does **not** change any code path, so there is nothing to migrate:

- **No behaviour, storage, or error-code change.** The gate's fail-closed
  semantics (a payer funds only on a positive, fresh verification once any
  oracle is configured; the sole fail-open path is the unconfigured feed) and
  the exact outcome-to-error mapping (`Stale → OracleDataStale`,
  `QuorumDisagreement`/`Unverified → PayerUnverified`,
  `TimestampRegression`, `DuplicateUpdate`, `CircuitOpen`, in the fixed
  precedence documented in [`oracle-design.md`](oracle-design.md#fail-closed-semantics-issue-1056))
  were already enforced by the #1046–#1055 work. No storage key, value encoding,
  error code, event, entry-point signature, or TTL constant is touched, so
  **no storage migration is required** and a plain `upgrade()` of an
  already-deployed contract is unaffected.
- **Corrected evidence pointer for reviewers.** The issue's audit evidence cites
  `lib.rs:3832-3847`; those lines are the dispute-resolution ("Upheld") refund
  block, not oracle logic. The real fail-closed path is `fund_invoice`'s
  `require_oracle_verification` block (`lib.rs:2656-2711`), delegating to
  `oracle_registry::evaluate_identity_verification`. No action follows from the
  citation beyond the corrected reference.
- **New regression suite.** `tests_oracle_registry.rs`'s `test_1056_*` (9 cases)
  pins the documented contract: success, the fail-open exception, unverified,
  quorum disagreement, the exact staleness ledger boundary, frozen/duplicate
  rejection, rejection atomicity, gate/diagnostic agreement, and the fact that
  oracle verification never bypasses the funder's authorization.

**No rollout steps and no rollback caveat** — nothing is deployed or persisted.
See [`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#19-document-fail-closed-oracle-behavior-issue-1056)
and [`threat-model.md`](threat-model.md) for the before/after detail.

## Worst-Case Queue Insertion Benchmark (Issue #1057)

This change **benchmarks and regression-tests** `join_fund_queue` at the queue
bound (`MAX_FUND_QUEUE_LEN` = 50) — it does **not** change any code path, so
there is nothing to migrate:

- **No behaviour, storage, or error-code change.** Admission still follows the
  #1095/#1091 rule: at the bound, a joiner that strictly outscores the tail is
  admitted (displacing the tail and emitting `fund_queue_displaced`), and a
  joiner at or below the tail score is rejected with `QueueFull`. The
  `FundQueueOpenedAt` MEV marker (#MEV-1) is still written on the first join
  only (`try_set_fund_queue_opened_at`), so a displacing re-join never resets
  the resolution maturity gate. No storage key, value encoding, error code,
  event, entry-point signature, or TTL constant is touched, so **no storage
  migration is required** and a plain `upgrade()` of an already-deployed
  contract is unaffected.
- **New benchmark.** `tests_benchmarks.rs`'s `benchmark_worst_case_queue_insertion`
  sweeps admission positions (head index 0, middle index 25, tail index 49) plus
  the `QueueFull` rejection branch at the bound, replacing #1062's single
  equal-score point (which always forced index 0 and could not show where the
  worst case is). Finding: rows are near-flat — cost is dominated by the
  serialized entry size at the bound, not the insertion index; the head insert
  is marginally the worst *admitted* case.
- **New regression suite.** `tests_storage.rs`'s `test_1057_*` (6 cases) pins
  head displacement, middle placement, the tail+1 admission boundary, exact-tail
  rejection (queue unchanged), stripped-authorization rejection, and
  `FundQueueOpenedAt` non-reset on a displacing re-join.

**No rollout steps and no rollback caveat** — nothing is deployed or persisted.
See [`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#20-benchmark-worst-case-queue-insertion-issue-1057)
and [`benchmarks.md`](benchmarks.md) (§ Worst-case queue insertion) for the
before/after detail and the run command.

---

**Document Prepared By:** DevOps & Security Team  
**Last Updated:** October 2026 (upgrade migration sequencing added — Issue #1086)  
**Next Review:** Upon next upgrade

## Appendix: fund-queue TTL (Issues #1009 / #1010) and multisig error codes

* **Fund-queue TTL.** `FundQueue`, `FundQueueOpenedAt` and `QueueResolution` are
  now extended together (threshold 1,000,000 / extend-to 2,000,000 ledgers) on
  every read and write. No migration is required and no keys are added.
  Queue entries written by a *previous* wasm keep their old (short) TTL until
  they are next read or written by the new wasm, so after upgrading, have an
  operator call `resolve_fund_queue(invoice_id)` (idempotent; returns the
  approved LP and bumps all three records) for any invoice with a resolved
  queue that is still awaiting funding. Rollback to the old wasm is safe: the
  extended TTLs simply stop being refreshed.
* **Multisig error codes.** The multisig admin `ContractError` variants lost in
  a merge were restored as codes **47–59** (`InvalidMultisigConfig` = 47 …
  `RotationAlreadyPending` = 59). Codes 44–46 are unchanged
  (`MaxInvoiceAmountExceeded`, `GlobalVolumeCapExceeded`, `InvalidTwapWindow`).
  SDK error maps must be regenerated from the contract spec.
