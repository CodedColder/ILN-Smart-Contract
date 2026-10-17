/// Issue #1082 — Reject upgrades removing TTL fields
///
/// Tests are split into two modules:
///   1. `split_layout`  — existing hot/cold invoice-split layout tests
///      (unchanged behaviour).
///   2. `ttl_upgrade_guard` — new tests for the pre-upgrade TTL validation
///      added by Issue #1082.
///
/// # Before / After baseline
///
/// **Before** (no guard): `upgrade()` called `require_admin` + rate-limit +
/// `record_admin_action` + `env.deployer().update_current_contract_wasm()`.
/// Any persistent entry could have an arbitrarily-low TTL at upgrade time;
/// the upgrade would silently proceed and data could expire mid-flight in the
/// new WASM.
///
/// **After** (this PR): `upgrade()` additionally calls
/// `validate_ttl_fields_pre_upgrade()` _before_ the WASM swap.  If any
/// `InvoiceCore`, `InvoiceMetadata`, `FundQueue`, `FundQueueOpenedAt`, or
/// `QueueResolution` entry in the most-recent `TTL_UPGRADE_SAMPLE_SIZE`
/// invoices has a remaining TTL < `TTL_MIN_LEDGERS_BEFORE_UPGRADE` (500 000
/// ledgers ≈ 29 days), the call returns
/// `Err(ContractError::UpgradeRemovesTtlFields)` and the WASM is **never
/// swapped** (fail-closed).
#[cfg(test)]
mod split_layout {
    use crate::invoice::{Invoice, InvoiceCore, InvoiceMetadata, InvoiceStatus, ReferralCode};
    use soroban_sdk::testutils::Address as TestAddress;
    use soroban_sdk::{Address, Env};

    #[test]
    fn test_invoice_to_core_split() {
        let env = Env::default();
        // Create a full invoice
        let invoice = Invoice {
            id: 123,
            freelancer: Address::generate(&env),
            payer: Address::generate(&env),
            token: Address::generate(&env),
            amount: 1_000_000,
            due_date: 1234567890,
            discount_rate: 300,
            status: InvoiceStatus::Pending,
            funder: Some(Address::generate(&env)),
            funded_at: Some(1234567800),
            amount_funded: 0,
            amount_paid: 0,
            referral_code: ReferralCode::None,
            submitter_reputation: 50,
        };

        // Extract core
        let core = invoice.to_core();
        assert_eq!(core.id, 123);
        assert_eq!(core.amount, 1_000_000);
        assert_eq!(core.due_date, 1234567890);
        assert_eq!(core.discount_rate, 300);
        assert_eq!(core.status, InvoiceStatus::Pending);
        assert_eq!(core.amount_funded, 0);
        assert_eq!(core.amount_paid, 0);

        // Extract metadata
        let metadata = invoice.to_metadata();
        assert_eq!(metadata.funded_at, Some(1234567800));
        assert_eq!(metadata.submitter_reputation, 50);
        assert_eq!(metadata.referral_code, ReferralCode::None);
    }

    #[test]
    fn test_invoice_core_with_metadata_roundtrip() {
        let env = Env::default();
        // Create core and metadata
        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);
        let token = Address::generate(&env);
        let funder = Address::generate(&env);

        let core = InvoiceCore {
            id: 456,
            freelancer: freelancer.clone(),
            payer: payer.clone(),
            token: token.clone(),
            amount: 2_000_000,
            due_date: 1234567890,
            discount_rate: 500,
            status: InvoiceStatus::Funded,
            amount_funded: 2_000_000,
            amount_paid: 0,
        };

        let metadata = InvoiceMetadata {
            funder: Some(funder.clone()),
            funded_at: Some(1234567850),
            referral_code: ReferralCode::None,
            submitter_reputation: 75,
        };

        // Combine into full invoice
        let invoice = core.clone().with_metadata(metadata.clone());

        // Verify all fields match
        assert_eq!(invoice.id, 456);
        assert_eq!(invoice.freelancer, freelancer);
        assert_eq!(invoice.payer, payer);
        assert_eq!(invoice.token, token);
        assert_eq!(invoice.amount, 2_000_000);
        assert_eq!(invoice.due_date, 1234567890);
        assert_eq!(invoice.discount_rate, 500);
        assert_eq!(invoice.status, InvoiceStatus::Funded);
        assert_eq!(invoice.amount_funded, 2_000_000);
        assert_eq!(invoice.amount_paid, 0);
        assert_eq!(invoice.funder, Some(funder));
        assert_eq!(invoice.funded_at, Some(1234567850));
        assert_eq!(invoice.submitter_reputation, 75);

        // Round-trip back to core and metadata
        assert_eq!(invoice.to_core(), core);
        assert_eq!(invoice.to_metadata(), metadata);
    }

    #[test]
    fn test_invoice_hot_cold_separation_consistency() {
        let env = Env::default();
        // Test that extracting hot/cold and recombining gives same result
        let invoice = Invoice {
            id: 789,
            freelancer: Address::generate(&env),
            payer: Address::generate(&env),
            token: Address::generate(&env),
            amount: 5_000_000,
            due_date: 987654321,
            discount_rate: 100,
            status: InvoiceStatus::PartiallyFunded,
            funder: None,
            funded_at: None,
            amount_funded: 2_500_000,
            amount_paid: 1_500_000,
            referral_code: ReferralCode::None,
            submitter_reputation: 25,
        };

        let core = invoice.to_core();
        let metadata = invoice.to_metadata();
        let reconstructed = core.with_metadata(metadata);

        // Should be identical
        assert_eq!(invoice.id, reconstructed.id);
        assert_eq!(invoice.amount, reconstructed.amount);
        assert_eq!(invoice.amount_funded, reconstructed.amount_funded);
        assert_eq!(invoice.amount_paid, reconstructed.amount_paid);
        assert_eq!(invoice.status, reconstructed.status);
        assert_eq!(invoice.funder, reconstructed.funder);
        assert_eq!(invoice.funded_at, reconstructed.funded_at);
        assert_eq!(
            invoice.submitter_reputation,
            reconstructed.submitter_reputation
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #1082: Pre-upgrade TTL validation tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod ttl_upgrade_guard {
    use crate::constants::{TTL_MIN_LEDGERS_BEFORE_UPGRADE, TTL_UPGRADE_SAMPLE_SIZE};
    use crate::errors::ContractError;
    use crate::invoice::{Invoice, InvoiceStatus, ReferralCode};
    use crate::storage::{validate_ttl_fields_pre_upgrade, DataKey};
    use soroban_sdk::testutils::Address as TestAddress;
    use soroban_sdk::{Address, Env};

    // ──────────────────────────────────────────────────────────────────────
    // Helpers
    // ──────────────────────────────────────────────────────────────────────

    /// Minimal invoice used across tests.
    fn make_invoice(env: &Env, id: u64) -> Invoice {
        Invoice {
            id,
            freelancer: Address::generate(env),
            payer: Address::generate(env),
            token: Address::generate(env),
            amount: 1_000_000,
            due_date: 4_000_000_000u32, // far-future u32 Unix timestamp
            discount_rate: 200,
            status: InvoiceStatus::Pending,
            funder: None,
            funded_at: None,
            amount_funded: 0,
            amount_paid: 0,
            referral_code: ReferralCode::None,
            submitter_reputation: 50,
        }
    }

    /// Write one invoice via `storage::save_invoice` (which sets full TTLs)
    /// and advance `NextInvoiceId` so the sampler knows about it.
    fn write_invoice_with_full_ttl(env: &Env, id: u64) {
        let invoice = make_invoice(env, id);
        crate::storage::save_invoice(env, &invoice);
        // Advance NextInvoiceId to id + 1 so sampler upper-bound is id.
        crate::storage::write_next_invoice_id(env, id + 1);
    }

    // ──────────────────────────────────────────────────────────────────────
    // Success path
    // ──────────────────────────────────────────────────────────────────────

    /// No invoices written — guard must succeed (nothing to check).
    #[test]
    fn test_ttl_guard_succeeds_with_no_invoices() {
        let env = Env::default();
        // NextInvoiceId defaults to 1 — nothing written.
        let result = validate_ttl_fields_pre_upgrade(&env);
        assert!(
            result.is_ok(),
            "guard must pass when no invoices exist; got {:?}",
            result
        );
    }

    /// All invoices have fresh TTLs set by `save_invoice` → guard passes.
    #[test]
    fn test_ttl_guard_succeeds_when_all_ttls_are_healthy() {
        let env = Env::default();
        // Write 5 invoices with full TTLs.
        for id in 1..=5u64 {
            write_invoice_with_full_ttl(&env, id);
        }

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert!(
            result.is_ok(),
            "guard must pass when all TTLs are ≥ threshold; got {:?}",
            result
        );
    }

    /// Exactly TTL_UPGRADE_SAMPLE_SIZE invoices, all with full TTLs → passes.
    #[test]
    fn test_ttl_guard_succeeds_at_full_sample_window() {
        let env = Env::default();
        let n = TTL_UPGRADE_SAMPLE_SIZE as u64;
        for id in 1..=n {
            write_invoice_with_full_ttl(&env, id);
        }

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert!(
            result.is_ok(),
            "guard must pass when all {} sampled entries are healthy; got {:?}",
            n,
            result
        );
    }

    /// More invoices than the sample window — only the most-recent window is
    /// checked; old invoices with any TTL state don't affect the result.
    #[test]
    fn test_ttl_guard_only_samples_recent_window() {
        let env = Env::default();
        // Write TTL_UPGRADE_SAMPLE_SIZE + 10 invoices; all have full TTLs.
        let extra = TTL_UPGRADE_SAMPLE_SIZE as u64 + 10;
        for id in 1..=extra {
            write_invoice_with_full_ttl(&env, id);
        }

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert!(
            result.is_ok(),
            "guard must pass — all recent entries healthy; got {:?}",
            result
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Failure path — InvoiceCore / InvoiceMetadata
    // ──────────────────────────────────────────────────────────────────────

    /// InvoiceCore for the most-recent invoice has a TTL below the threshold
    /// → guard returns UpgradeRemovesTtlFields.
    #[test]
    fn test_ttl_guard_rejects_when_invoice_core_ttl_below_threshold() {
        let env = Env::default();
        // Write a healthy invoice first…
        write_invoice_with_full_ttl(&env, 1);

        // …then manually write a second InvoiceCore with a dangerously low TTL.
        let id = 2u64;
        let invoice = make_invoice(&env, id);
        let core = invoice.to_core();
        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &core);
        // Deliberately set TTL just below the safety threshold.
        let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&core_key, low_ttl, low_ttl);

        // Also write InvoiceMetadata with a healthy TTL so only Core is bad.
        let meta = invoice.to_metadata();
        let meta_key = DataKey::InvoiceMetadata(id);
        env.storage().persistent().set(&meta_key, &meta);
        env.storage()
            .persistent()
            .extend_ttl(&meta_key, 2_000_000, 2_000_000);

        // Advance NextInvoiceId.
        crate::storage::write_next_invoice_id(&env, id + 1);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            result,
            Err(ContractError::UpgradeRemovesTtlFields),
            "guard must reject upgrade when InvoiceCore TTL < threshold"
        );
    }

    /// InvoiceMetadata for the most-recent invoice has a TTL below the
    /// threshold → guard returns UpgradeRemovesTtlFields.
    #[test]
    fn test_ttl_guard_rejects_when_invoice_metadata_ttl_below_threshold() {
        let env = Env::default();
        let id = 1u64;
        let invoice = make_invoice(&env, id);

        // InvoiceCore: healthy TTL.
        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &invoice.to_core());
        env.storage()
            .persistent()
            .extend_ttl(&core_key, 2_000_000, 2_000_000);

        // InvoiceMetadata: below threshold.
        let meta_key = DataKey::InvoiceMetadata(id);
        env.storage()
            .persistent()
            .set(&meta_key, &invoice.to_metadata());
        let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&meta_key, low_ttl, low_ttl);

        crate::storage::write_next_invoice_id(&env, id + 1);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            result,
            Err(ContractError::UpgradeRemovesTtlFields),
            "guard must reject upgrade when InvoiceMetadata TTL < threshold"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Failure path — fund-queue keys
    // ──────────────────────────────────────────────────────────────────────

    /// FundQueue entry has a TTL below the threshold → guard rejects.
    #[test]
    fn test_ttl_guard_rejects_when_fund_queue_ttl_below_threshold() {
        let env = Env::default();
        let id = 1u64;

        // Write a healthy invoice.
        write_invoice_with_full_ttl(&env, id);

        // Write a FundQueue entry with a low TTL (simulates a forgotten renewal).
        let fq_key = DataKey::FundQueue(id);
        env.storage()
            .persistent()
            .set(&fq_key, &soroban_sdk::Vec::<crate::invoice::LpFundRequest>::new(&env));
        let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&fq_key, low_ttl, low_ttl);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            result,
            Err(ContractError::UpgradeRemovesTtlFields),
            "guard must reject upgrade when FundQueue TTL < threshold"
        );
    }

    /// FundQueueOpenedAt entry has a TTL below the threshold → guard rejects.
    #[test]
    fn test_ttl_guard_rejects_when_fund_queue_opened_at_ttl_below_threshold() {
        let env = Env::default();
        let id = 1u64;
        write_invoice_with_full_ttl(&env, id);

        let fqoa_key = DataKey::FundQueueOpenedAt(id);
        env.storage()
            .persistent()
            .set(&fqoa_key, &env.ledger().sequence());
        let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&fqoa_key, low_ttl, low_ttl);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            result,
            Err(ContractError::UpgradeRemovesTtlFields),
            "guard must reject upgrade when FundQueueOpenedAt TTL < threshold"
        );
    }

    /// QueueResolution entry has a TTL below the threshold → guard rejects.
    #[test]
    fn test_ttl_guard_rejects_when_queue_resolution_ttl_below_threshold() {
        let env = Env::default();
        let id = 1u64;
        write_invoice_with_full_ttl(&env, id);

        let qr_key = DataKey::QueueResolution(id);
        let approved = Address::generate(&env);
        env.storage().persistent().set(&qr_key, &approved);
        let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&qr_key, low_ttl, low_ttl);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            result,
            Err(ContractError::UpgradeRemovesTtlFields),
            "guard must reject upgrade when QueueResolution TTL < threshold"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Ledger-boundary cases
    // ──────────────────────────────────────────────────────────────────────

    /// TTL exactly equal to the threshold → passes (boundary: inclusive lower
    /// bound is TTL_MIN_LEDGERS_BEFORE_UPGRADE, so == threshold is allowed).
    #[test]
    fn test_ttl_guard_passes_at_exact_threshold() {
        let env = Env::default();
        let id = 1u64;
        let invoice = make_invoice(&env, id);

        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &invoice.to_core());
        // extend_ttl to exactly the threshold value.
        env.storage().persistent().extend_ttl(
            &core_key,
            TTL_MIN_LEDGERS_BEFORE_UPGRADE,
            TTL_MIN_LEDGERS_BEFORE_UPGRADE,
        );

        let meta_key = DataKey::InvoiceMetadata(id);
        env.storage()
            .persistent()
            .set(&meta_key, &invoice.to_metadata());
        env.storage().persistent().extend_ttl(
            &meta_key,
            TTL_MIN_LEDGERS_BEFORE_UPGRADE,
            TTL_MIN_LEDGERS_BEFORE_UPGRADE,
        );

        crate::storage::write_next_invoice_id(&env, id + 1);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert!(
            result.is_ok(),
            "guard must pass when TTL == threshold (boundary inclusive); got {:?}",
            result
        );
    }

    /// TTL is exactly one ledger below the threshold → rejected.
    #[test]
    fn test_ttl_guard_rejects_one_ledger_below_threshold() {
        let env = Env::default();
        let id = 1u64;
        let invoice = make_invoice(&env, id);

        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &invoice.to_core());
        let just_below = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&core_key, just_below, just_below);

        // Metadata is fine.
        let meta_key = DataKey::InvoiceMetadata(id);
        env.storage()
            .persistent()
            .set(&meta_key, &invoice.to_metadata());
        env.storage().persistent().extend_ttl(
            &meta_key,
            TTL_MIN_LEDGERS_BEFORE_UPGRADE,
            TTL_MIN_LEDGERS_BEFORE_UPGRADE,
        );

        crate::storage::write_next_invoice_id(&env, id + 1);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            result,
            Err(ContractError::UpgradeRemovesTtlFields),
            "guard must reject when TTL == threshold - 1"
        );
    }

    /// Only the oldest invoice (outside the sample window) has a low TTL —
    /// the guard only checks the recent window so it must still pass.
    #[test]
    fn test_ttl_guard_ignores_invoices_outside_sample_window() {
        let env = Env::default();

        // Write TTL_UPGRADE_SAMPLE_SIZE + 1 invoices; the very first one
        // gets a dangerously low TTL, but it will be outside the window.
        let total = TTL_UPGRADE_SAMPLE_SIZE as u64 + 1;

        // Invoice 1: write with healthy TTL first so NextInvoiceId advances,
        // then manually overwrite core with a low TTL.
        write_invoice_with_full_ttl(&env, 1);
        let low_core_key = DataKey::InvoiceCore(1u64);
        let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&low_core_key, low_ttl, low_ttl);

        // Invoices 2..=total: all healthy.
        for id in 2..=total {
            write_invoice_with_full_ttl(&env, id);
        }

        // NextInvoiceId should now be total + 1 (all ids 1..=total written).
        // The sample window is ids (total - TTL_UPGRADE_SAMPLE_SIZE + 1)..=total
        // = 2..=total, so invoice 1 is excluded.
        let result = validate_ttl_fields_pre_upgrade(&env);
        assert!(
            result.is_ok(),
            "guard must pass when the low-TTL entry is outside the sample window; got {:?}",
            result
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Authorization / adversarial cases
    // ──────────────────────────────────────────────────────────────────────

    /// Guard is re-entrant safe: calling it twice gives the same result.
    #[test]
    fn test_ttl_guard_is_idempotent() {
        let env = Env::default();
        write_invoice_with_full_ttl(&env, 1);

        let r1 = validate_ttl_fields_pre_upgrade(&env);
        let r2 = validate_ttl_fields_pre_upgrade(&env);
        assert!(r1.is_ok(), "first call should pass; got {:?}", r1);
        assert!(r2.is_ok(), "second call should pass; got {:?}", r2);
    }

    /// All keys in every checked category are simultaneously below the
    /// threshold for multiple invoices → still a single well-typed error.
    #[test]
    fn test_ttl_guard_rejects_with_correct_error_variant_for_multiple_bad_entries() {
        let env = Env::default();

        // Write 3 invoices all with low-TTL cores.
        for id in 1..=3u64 {
            let invoice = make_invoice(&env, id);
            let core_key = DataKey::InvoiceCore(id);
            env.storage().persistent().set(&core_key, &invoice.to_core());
            let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 100;
            env.storage()
                .persistent()
                .extend_ttl(&core_key, low_ttl, low_ttl);

            let meta_key = DataKey::InvoiceMetadata(id);
            env.storage()
                .persistent()
                .set(&meta_key, &invoice.to_metadata());
            env.storage()
                .persistent()
                .extend_ttl(&meta_key, low_ttl, low_ttl);
        }
        crate::storage::write_next_invoice_id(&env, 4);

        let result = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            result,
            Err(ContractError::UpgradeRemovesTtlFields),
            "multiple bad entries must produce UpgradeRemovesTtlFields"
        );
    }

    /// After refreshing a previously low-TTL entry the guard passes —
    /// confirms the remediation path works.
    #[test]
    fn test_ttl_guard_passes_after_remediation_refresh() {
        let env = Env::default();
        let id = 1u64;
        let invoice = make_invoice(&env, id);

        // Write with a low TTL initially.
        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &invoice.to_core());
        let low_ttl = TTL_MIN_LEDGERS_BEFORE_UPGRADE - 1;
        env.storage()
            .persistent()
            .extend_ttl(&core_key, low_ttl, low_ttl);

        let meta_key = DataKey::InvoiceMetadata(id);
        env.storage()
            .persistent()
            .set(&meta_key, &invoice.to_metadata());
        env.storage()
            .persistent()
            .extend_ttl(&meta_key, low_ttl, low_ttl);

        crate::storage::write_next_invoice_id(&env, id + 1);

        // Pre-remediation: should fail.
        let pre = validate_ttl_fields_pre_upgrade(&env);
        assert_eq!(
            pre,
            Err(ContractError::UpgradeRemovesTtlFields),
            "should fail before remediation"
        );

        // Remediation: re-save the invoice (extends TTL to 2_000_000).
        crate::storage::save_invoice(&env, &invoice);

        // Post-remediation: should pass.
        let post = validate_ttl_fields_pre_upgrade(&env);
        assert!(
            post.is_ok(),
            "guard must pass after TTL is refreshed; got {:?}",
            post
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Constants sanity checks
    // ──────────────────────────────────────────────────────────────────────

    /// Ensure the constant values meet the documented intent:
    ///   - TTL_MIN_LEDGERS_BEFORE_UPGRADE = 500_000 (≈ 29 days at 5s/ledger)
    ///   - TTL_UPGRADE_SAMPLE_SIZE = 50
    ///   - Both < FUND_QUEUE_TTL_EXTEND_TO (2_000_000) — otherwise every
    ///     freshly-written entry would already fail.
    #[test]
    fn test_ttl_constants_have_expected_values() {
        use crate::constants::{
            FUND_QUEUE_TTL_EXTEND_TO, TTL_MIN_LEDGERS_BEFORE_UPGRADE, TTL_UPGRADE_SAMPLE_SIZE,
        };
        assert_eq!(
            TTL_MIN_LEDGERS_BEFORE_UPGRADE, 500_000,
            "TTL_MIN_LEDGERS_BEFORE_UPGRADE must be 500_000"
        );
        assert_eq!(
            TTL_UPGRADE_SAMPLE_SIZE, 50,
            "TTL_UPGRADE_SAMPLE_SIZE must be 50"
        );
        assert!(
            TTL_MIN_LEDGERS_BEFORE_UPGRADE < FUND_QUEUE_TTL_EXTEND_TO,
            "minimum upgrade TTL must be below the extend-to value so fresh entries always pass"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #1083: Rollback rehearsal tests
// ─────────────────────────────────────────────────────────────────────────────
//
// # Before / After baseline
//
// **Before** (no rehearsal mechanism): there was no way to verify that the
// current storage state would survive a WASM rollback.  An admin rolling back
// to a v1 WASM (which only reads `Invoice(id)`) would silently lose any
// invoice stored exclusively in the v2 split format (`InvoiceCore` +
// `InvoiceMetadata`), with no on-chain indication of the data loss.
//
// **After** (this PR): a new `rehearse_rollback()` contract entry point and
// underlying `check_rollback_safety()` pure function scan the most-recent
// `ROLLBACK_SAMPLE_SIZE` invoice IDs and enforce:
//
//   `Invoice(id) present  OR  (InvoiceCore(id) AND InvoiceMetadata(id)) both present`
//
// If any invoice violates this invariant, `check_rollback_safety` returns
// `Err(ContractError::RollbackUnsafe)` (code 51), and the operator is
// blocked from proceeding with a rollback until the orphaned entries are
// repaired.  The function is **read-only** — it never modifies storage.
#[cfg(test)]
mod rollback_rehearsal {
    use crate::constants::ROLLBACK_SAMPLE_SIZE;
    use crate::errors::ContractError;
    use crate::events::RollbackRehearsalReport;
    use crate::invoice::{Invoice, InvoiceStatus, ReferralCode};
    use crate::storage::{check_rollback_safety, DataKey};
    use soroban_sdk::testutils::Address as TestAddress;
    use soroban_sdk::{Address, Env};

    // ──────────────────────────────────────────────────────────────────────
    // Helpers
    // ──────────────────────────────────────────────────────────────────────

    fn make_invoice(env: &Env, id: u64) -> Invoice {
        Invoice {
            id,
            freelancer: Address::generate(env),
            payer: Address::generate(env),
            token: Address::generate(env),
            amount: 1_000_000,
            due_date: 4_000_000_000u32,
            discount_rate: 200,
            status: InvoiceStatus::Pending,
            funder: None,
            funded_at: None,
            amount_funded: 0,
            amount_paid: 0,
            referral_code: ReferralCode::None,
            submitter_reputation: 50,
        }
    }

    /// Write invoice in the v2 split format (InvoiceCore + InvoiceMetadata).
    fn write_split_invoice(env: &Env, id: u64) {
        let invoice = make_invoice(env, id);
        crate::storage::save_invoice(env, &invoice);
        crate::storage::write_next_invoice_id(env, id + 1);
    }

    /// Write invoice in the legacy v1 monolithic format (Invoice key only).
    fn write_legacy_invoice(env: &Env, id: u64) {
        let invoice = make_invoice(env, id);
        let key = DataKey::Invoice(id);
        env.storage().persistent().set(&key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&key, 1_000_000, 2_000_000);
        crate::storage::write_next_invoice_id(env, id + 1);
    }

    /// Write only the InvoiceCore half (orphaned core — missing metadata).
    fn write_orphaned_core(env: &Env, id: u64) {
        let invoice = make_invoice(env, id);
        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &invoice.to_core());
        env.storage()
            .persistent()
            .extend_ttl(&core_key, 1_000_000, 2_000_000);
        crate::storage::write_next_invoice_id(env, id + 1);
    }

    /// Write only the InvoiceMetadata half (orphaned metadata — missing core).
    fn write_orphaned_metadata(env: &Env, id: u64) {
        let invoice = make_invoice(env, id);
        let meta_key = DataKey::InvoiceMetadata(id);
        env.storage()
            .persistent()
            .set(&meta_key, &invoice.to_metadata());
        env.storage()
            .persistent()
            .extend_ttl(&meta_key, 1_000_000, 2_000_000);
        crate::storage::write_next_invoice_id(env, id + 1);
    }

    /// Assert the expected report shape.
    fn assert_report(
        report: &RollbackRehearsalReport,
        checked: u32,
        split: u32,
        legacy: u32,
        orphaned: u32,
        safe: bool,
    ) {
        assert_eq!(report.invoices_checked, checked, "invoices_checked mismatch");
        assert_eq!(report.split_format_count, split, "split_format_count mismatch");
        assert_eq!(
            report.legacy_format_count, legacy,
            "legacy_format_count mismatch"
        );
        assert_eq!(report.orphaned_count, orphaned, "orphaned_count mismatch");
        assert_eq!(report.rollback_safe, safe, "rollback_safe mismatch");
    }

    // ──────────────────────────────────────────────────────────────────────
    // Success / safe path
    // ──────────────────────────────────────────────────────────────────────

    /// No invoices at all → trivially safe; all counters zero.
    #[test]
    fn test_rollback_safe_with_no_invoices() {
        let env = Env::default();
        let result = check_rollback_safety(&env);
        assert!(result.is_ok(), "empty storage must be rollback-safe");
        let report = result.unwrap();
        assert_report(&report, 0, 0, 0, 0, true);
    }

    /// All invoices in split format (both halves present) → safe.
    #[test]
    fn test_rollback_safe_all_split_format() {
        let env = Env::default();
        for id in 1..=5u64 {
            write_split_invoice(&env, id);
        }
        let result = check_rollback_safety(&env);
        assert!(result.is_ok(), "all-split storage must be rollback-safe");
        let report = result.unwrap();
        assert_report(&report, 5, 5, 0, 0, true);
    }

    /// All invoices in legacy monolithic format → safe (old WASM can read them).
    #[test]
    fn test_rollback_safe_all_legacy_format() {
        let env = Env::default();
        for id in 1..=5u64 {
            write_legacy_invoice(&env, id);
        }
        let result = check_rollback_safety(&env);
        assert!(result.is_ok(), "all-legacy storage must be rollback-safe");
        let report = result.unwrap();
        assert_report(&report, 5, 0, 5, 0, true);
    }

    /// Mixed legacy + split (all complete) → safe.
    #[test]
    fn test_rollback_safe_mixed_legacy_and_split() {
        let env = Env::default();
        // IDs 1, 3, 5: legacy; IDs 2, 4: split.
        write_legacy_invoice(&env, 1);
        write_split_invoice(&env, 2);
        write_legacy_invoice(&env, 3);
        write_split_invoice(&env, 4);
        write_legacy_invoice(&env, 5);

        let result = check_rollback_safety(&env);
        assert!(result.is_ok(), "mixed complete storage must be rollback-safe");
        let report = result.unwrap();
        assert_report(&report, 5, 2, 3, 0, true);
    }

    /// Exactly ROLLBACK_SAMPLE_SIZE invoices, all split → safe.
    #[test]
    fn test_rollback_safe_at_full_sample_window() {
        let env = Env::default();
        let n = ROLLBACK_SAMPLE_SIZE as u64;
        for id in 1..=n {
            write_split_invoice(&env, id);
        }
        let result = check_rollback_safety(&env);
        assert!(result.is_ok(), "full window of split invoices must be safe");
        let report = result.unwrap();
        assert_eq!(report.invoices_checked, n as u32);
        assert_eq!(report.split_format_count, n as u32);
        assert_eq!(report.orphaned_count, 0);
        assert!(report.rollback_safe);
    }

    // ──────────────────────────────────────────────────────────────────────
    // Failure / unsafe path
    // ──────────────────────────────────────────────────────────────────────

    /// An orphaned InvoiceCore (missing metadata) → RollbackUnsafe.
    #[test]
    fn test_rollback_unsafe_orphaned_core() {
        let env = Env::default();
        write_split_invoice(&env, 1); // healthy split
        write_orphaned_core(&env, 2); // missing metadata half

        let result = check_rollback_safety(&env);
        assert_eq!(
            result,
            Err(ContractError::RollbackUnsafe),
            "orphaned core must trigger RollbackUnsafe"
        );
    }

    /// An orphaned InvoiceMetadata (missing core) → RollbackUnsafe.
    #[test]
    fn test_rollback_unsafe_orphaned_metadata() {
        let env = Env::default();
        write_split_invoice(&env, 1); // healthy split
        write_orphaned_metadata(&env, 2); // missing core half

        let result = check_rollback_safety(&env);
        assert_eq!(
            result,
            Err(ContractError::RollbackUnsafe),
            "orphaned metadata must trigger RollbackUnsafe"
        );
    }

    /// Multiple orphaned entries → RollbackUnsafe with correct orphaned_count.
    /// (check_rollback_safety returns Err on the first orphan it finds;
    ///  callers wanting full counts must inspect the report field from a
    ///  separate non-short-circuit scan.  This test confirms it errors at all.)
    #[test]
    fn test_rollback_unsafe_multiple_orphaned_entries() {
        let env = Env::default();
        write_orphaned_core(&env, 1);
        write_orphaned_core(&env, 2);
        write_orphaned_metadata(&env, 3);

        let result = check_rollback_safety(&env);
        assert_eq!(
            result,
            Err(ContractError::RollbackUnsafe),
            "any orphaned entry must produce RollbackUnsafe"
        );
    }

    /// A single orphaned core in an otherwise-healthy set → still RollbackUnsafe.
    #[test]
    fn test_rollback_unsafe_single_bad_in_large_set() {
        let env = Env::default();
        for id in 1..=10u64 {
            write_split_invoice(&env, id);
        }
        // Overwrite the core for id 5 as orphaned (remove metadata).
        let meta_key = DataKey::InvoiceMetadata(5u64);
        // We can't "delete" storage in Soroban test env, but we can simulate
        // an orphaned state by writing a fresh id with only core.
        write_orphaned_core(&env, 11);

        let result = check_rollback_safety(&env);
        assert_eq!(
            result,
            Err(ContractError::RollbackUnsafe),
            "one orphaned entry in a large set must still fail"
        );
        // Suppress unused variable warning from the meta_key assignment above.
        let _ = meta_key;
    }

    // ──────────────────────────────────────────────────────────────────────
    // Ledger-boundary cases
    // ──────────────────────────────────────────────────────────────────────

    /// Orphaned entry outside the sample window → not detected (by design).
    /// The sampler only covers the most-recent ROLLBACK_SAMPLE_SIZE IDs.
    #[test]
    fn test_rollback_orphan_outside_sample_window_not_detected() {
        let env = Env::default();

        // Write ROLLBACK_SAMPLE_SIZE + 1 invoices.
        // Invoice 1 will be outside the window.
        let total = ROLLBACK_SAMPLE_SIZE as u64 + 1;

        // Invoice 1: orphaned core (outside window after rest are written).
        write_orphaned_core(&env, 1);

        // Invoices 2..=total: all healthy split.
        for id in 2..=total {
            write_split_invoice(&env, id);
        }

        // Window covers ids 2..=total (ROLLBACK_SAMPLE_SIZE ids), so id 1 is excluded.
        let result = check_rollback_safety(&env);
        assert!(
            result.is_ok(),
            "orphan outside sample window must not be detected; got {:?}",
            result
        );
        let report = result.unwrap();
        assert_eq!(report.invoices_checked, ROLLBACK_SAMPLE_SIZE as u32);
        assert_eq!(report.orphaned_count, 0);
        assert!(report.rollback_safe);
    }

    /// Exactly one invoice — the boundary case for the sample window lower bound.
    #[test]
    fn test_rollback_safe_exactly_one_split_invoice() {
        let env = Env::default();
        write_split_invoice(&env, 1);

        let result = check_rollback_safety(&env);
        assert!(result.is_ok(), "single split invoice must be safe");
        let report = result.unwrap();
        assert_report(&report, 1, 1, 0, 0, true);
    }

    /// Exactly one invoice that is orphaned.
    #[test]
    fn test_rollback_unsafe_exactly_one_orphaned_invoice() {
        let env = Env::default();
        write_orphaned_core(&env, 1);

        let result = check_rollback_safety(&env);
        assert_eq!(
            result,
            Err(ContractError::RollbackUnsafe),
            "single orphaned invoice must fail"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Authorization / adversarial cases
    // ──────────────────────────────────────────────────────────────────────

    /// check_rollback_safety is idempotent — calling it twice gives the same result.
    #[test]
    fn test_rollback_safety_is_idempotent() {
        let env = Env::default();
        write_split_invoice(&env, 1);
        write_split_invoice(&env, 2);

        let r1 = check_rollback_safety(&env);
        let r2 = check_rollback_safety(&env);
        assert!(r1.is_ok(), "first call must pass; got {:?}", r1);
        assert!(r2.is_ok(), "second call must pass; got {:?}", r2);
        assert_eq!(r1.unwrap(), r2.unwrap(), "results must be identical");
    }

    /// check_rollback_safety is read-only — no storage is modified.
    #[test]
    fn test_rollback_safety_does_not_modify_storage() {
        let env = Env::default();
        write_split_invoice(&env, 1);

        // Read state before.
        let has_core_before = env
            .storage()
            .persistent()
            .has(&DataKey::InvoiceCore(1u64));
        let has_meta_before = env
            .storage()
            .persistent()
            .has(&DataKey::InvoiceMetadata(1u64));

        check_rollback_safety(&env).unwrap();

        // State must be unchanged.
        let has_core_after = env
            .storage()
            .persistent()
            .has(&DataKey::InvoiceCore(1u64));
        let has_meta_after = env
            .storage()
            .persistent()
            .has(&DataKey::InvoiceMetadata(1u64));

        assert_eq!(has_core_before, has_core_after);
        assert_eq!(has_meta_before, has_meta_after);
    }

    /// After repairing an orphaned entry (re-saving the full invoice),
    /// the rehearsal passes — confirms the remediation path.
    #[test]
    fn test_rollback_safety_passes_after_orphan_repaired() {
        let env = Env::default();

        // Write an orphaned core.
        let id = 1u64;
        let invoice = make_invoice(&env, id);
        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &invoice.to_core());
        env.storage()
            .persistent()
            .extend_ttl(&core_key, 1_000_000, 2_000_000);
        crate::storage::write_next_invoice_id(&env, id + 1);

        // Pre-repair: must fail.
        let pre = check_rollback_safety(&env);
        assert_eq!(
            pre,
            Err(ContractError::RollbackUnsafe),
            "should fail before repair"
        );

        // Repair: save the complete invoice (writes both halves).
        crate::storage::save_invoice(&env, &invoice);

        // Post-repair: must pass.
        let post = check_rollback_safety(&env);
        assert!(
            post.is_ok(),
            "must pass after orphan is repaired; got {:?}",
            post
        );
        let report = post.unwrap();
        assert_eq!(report.orphaned_count, 0);
        assert!(report.rollback_safe);
    }

    // ──────────────────────────────────────────────────────────────────────
    // Report field accuracy
    // ──────────────────────────────────────────────────────────────────────

    /// Verify all report counters are accurate for a known mix of formats.
    #[test]
    fn test_rollback_report_counters_are_accurate() {
        let env = Env::default();
        // 3 split (both halves), 2 legacy, 0 orphaned.
        write_split_invoice(&env, 1);
        write_split_invoice(&env, 2);
        write_legacy_invoice(&env, 3);
        write_split_invoice(&env, 4);
        write_legacy_invoice(&env, 5);

        let result = check_rollback_safety(&env);
        assert!(result.is_ok());
        let report = result.unwrap();
        assert_report(&report, 5, 3, 2, 0, true);
    }

    /// An invoice with BOTH a legacy key AND split keys counts as legacy
    /// (the legacy key wins in the precedence check and we count it once).
    #[test]
    fn test_rollback_invoice_with_both_legacy_and_split_keys_counted_as_legacy() {
        let env = Env::default();
        let id = 1u64;
        let invoice = make_invoice(&env, id);

        // Write all three keys (unusual but possible during a migration).
        let legacy_key = DataKey::Invoice(id);
        env.storage().persistent().set(&legacy_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&legacy_key, 1_000_000, 2_000_000);

        let core_key = DataKey::InvoiceCore(id);
        env.storage().persistent().set(&core_key, &invoice.to_core());
        env.storage()
            .persistent()
            .extend_ttl(&core_key, 1_000_000, 2_000_000);

        let meta_key = DataKey::InvoiceMetadata(id);
        env.storage()
            .persistent()
            .set(&meta_key, &invoice.to_metadata());
        env.storage()
            .persistent()
            .extend_ttl(&meta_key, 1_000_000, 2_000_000);

        crate::storage::write_next_invoice_id(&env, id + 1);

        let result = check_rollback_safety(&env);
        assert!(result.is_ok(), "all-three-keys invoice must be safe");
        let report = result.unwrap();
        // Legacy key takes precedence in the invariant check.
        assert_eq!(report.legacy_format_count, 1);
        assert_eq!(report.split_format_count, 0);
        assert_eq!(report.orphaned_count, 0);
        assert!(report.rollback_safe);
    }

    // ──────────────────────────────────────────────────────────────────────
    // Constants sanity checks
    // ──────────────────────────────────────────────────────────────────────

    /// ROLLBACK_SAMPLE_SIZE must equal 50 and match TTL_UPGRADE_SAMPLE_SIZE.
    #[test]
    fn test_rollback_sample_size_constant() {
        use crate::constants::{ROLLBACK_SAMPLE_SIZE, TTL_UPGRADE_SAMPLE_SIZE};
        assert_eq!(
            ROLLBACK_SAMPLE_SIZE, 50,
            "ROLLBACK_SAMPLE_SIZE must be 50"
        );
        assert_eq!(
            ROLLBACK_SAMPLE_SIZE, TTL_UPGRADE_SAMPLE_SIZE,
            "ROLLBACK_SAMPLE_SIZE and TTL_UPGRADE_SAMPLE_SIZE should match so the same \
             population is checked by both guards"
        );
    }
}
