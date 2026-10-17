//! Issue #1069 — Initialize decimals for every approved asset
//!
//! `initialize`'s three bootstrap tokens (USDC, EURC at 6 decimals; XLM at
//! 7) are registered via a fixed constant, never checked against the
//! token itself — unlike `add_token`, which always calls
//! `validate_token_decimals`. `docs/token-safety.md`'s "Known
//! limitations" section already flagged why this can't simply be made to
//! call that same validation unconditionally: `initialize` is also called
//! in tests with placeholder (non-contract) addresses for `eurc_token`,
//! and on Stellar Mainnet classic-asset SACs — including the *real*
//! USDC/EURC — report **7** decimals, not the hardcoded 6, so strict
//! validation at `initialize` time would reject the exact deployment this
//! contract is meant to support, and changing the stored value outright
//! would silently change `validate_invoice_terms_with_token`'s minimum
//! invoice amount for USDC in every existing test and deployment that
//! predates this fix.
//!
//! # Before / after
//!
//! **Before:** the divergence between a bootstrap token's registered
//! decimals and its real on-chain `decimals()` was invisible — nothing
//! read, reported, or allowed correcting it after the fact.
//!
//! **After:** `audit_token_decimals` (read-only, Access: Anyone) reports
//! both sides so the divergence is observable without changing anything,
//! and `correct_token_decimals` (Admin only) lets an operator who has
//! confirmed the real value fix it for their specific deployment — fully
//! re-validated against the token's own `decimals()`, exactly like
//! `add_token`. `initialize`'s own behavior is completely unchanged: this
//! is additive, opt-in correction, not a forced migration.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token::StellarAssetClient,
    Address, Env, IntoVal,
};

const AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct T {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    usdc: Address,
    xlm: Address,
}

fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);

    // Real SACs, exactly like every other test in this crate -- and, per
    // Stellar's classic-asset model, always 7 decimals regardless of what
    // the asset is named. This is precisely the mainnet divergence
    // docs/token-safety.md describes: `initialize` will still register 6
    // for this "usdc_token" address.
    let usdc_admin = Address::generate(&env);
    let usdc = env.register_stellar_asset_contract_v2(usdc_admin).address();
    let xlm_admin = Address::generate(&env);
    let xlm = env.register_stellar_asset_contract_v2(xlm_admin).address();
    let eurc = Address::generate(&env);

    let usdc_sac = StellarAssetClient::new(&env, &usdc);
    usdc_sac.mint(&payer, &(AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    contract.initialize(&admin, &usdc, &eurc, &xlm);

    let mut info = env.ledger().get();
    info.timestamp = 1_700_000_000;
    info.sequence_number += crate::constants::DEFAULT_RATE_LIMIT_LEDGERS as u32;
    env.ledger().set(info);

    T {
        env,
        contract,
        admin,
        freelancer,
        payer,
        usdc,
        xlm,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// audit_token_decimals: making the divergence observable (success cases)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_audit_detects_usdc_bootstrap_mismatch() {
    let t = setup();

    let audit = t.contract.audit_token_decimals(&t.usdc);
    assert_eq!(
        audit.registered,
        Some(6),
        "initialize must still register the hardcoded 6 for usdc_token — unchanged behavior"
    );
    assert_eq!(
        audit.on_chain,
        Some(7),
        "a real Stellar classic-asset SAC always reports 7 decimals"
    );
    assert!(
        !audit.matches(),
        "the documented mainnet divergence (registered 6 vs real 7) must be detectable"
    );
}

#[test]
fn test_audit_matches_for_xlm_bootstrap() {
    let t = setup();

    // XLM's hardcoded bootstrap value (7) already matches a real SAC's
    // actual decimals() (also 7) -- contrast case: the mechanism must not
    // cry wolf when the registered and on-chain values genuinely agree.
    let audit = t.contract.audit_token_decimals(&t.xlm);
    assert_eq!(audit.registered, Some(7));
    assert_eq!(audit.on_chain, Some(7));
    assert!(audit.matches());
}

#[test]
fn test_audit_matches_for_token_added_with_correct_decimals() {
    let t = setup();

    let new_admin = Address::generate(&t.env);
    let new_token = t
        .env
        .register_stellar_asset_contract_v2(new_admin)
        .address();
    let new_sac = StellarAssetClient::new(&t.env, &new_token);
    new_sac.mint(&t.admin, &1_000_000_000_000);

    // add_token already validates decimals against the token itself, so
    // anything it approves must always audit as matching.
    t.contract.add_token(&new_token, &7);

    let audit = t.contract.audit_token_decimals(&new_token);
    assert_eq!(audit.registered, Some(7));
    assert_eq!(audit.on_chain, Some(7));
    assert!(audit.matches());
}

// ─────────────────────────────────────────────────────────────────────────
// correct_token_decimals: authorization
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_correct_token_decimals_rejects_non_admin() {
    let t = setup();
    let imposter = Address::generate(&t.env);

    // `require_admin` calls the *stored* admin address's `require_auth()`
    // — there's no caller parameter on `correct_token_decimals` to vary,
    // so under blanket `mock_all_auths()` this check can't be exercised at
    // all (every address's auth trivially succeeds). Mocking a specific,
    // non-admin signer's auth for this exact call is what actually
    // distinguishes "an imposter signed this" from "the real admin did".
    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "correct_token_decimals",
            args: (t.usdc.clone(), 7u32).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let result = t.contract.try_correct_token_decimals(&t.usdc, &7);
    assert!(
        result.is_err(),
        "correct_token_decimals must reject a call not authorized by the real admin"
    );
}

#[test]
fn test_correct_token_decimals_requires_approved_token() {
    let t = setup();
    let unapproved = Address::generate(&t.env);

    let result = t.contract.try_correct_token_decimals(&unapproved, &7);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

// ─────────────────────────────────────────────────────────────────────────
// correct_token_decimals: failure (re-validated, not a rubber stamp)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_correct_token_decimals_rejects_a_value_that_still_does_not_match() {
    let t = setup();

    // Re-affirming the already-wrong registered value (6) must fail --
    // this function cannot be used to "launder" an unvalidated value, it
    // can only set one that is actually re-validated against the token.
    let result = t.contract.try_correct_token_decimals(&t.usdc, &6);
    assert_eq!(result, Err(Ok(ContractError::InvalidTokenDecimals)));

    // Registered value must be unchanged after the rejected attempt.
    assert_eq!(t.contract.get_token_decimals(&t.usdc), Some(6));
}

#[test]
fn test_correct_token_decimals_boundary_above_max_token_decimals() {
    let t = setup();

    // 19 > MAX_TOKEN_DECIMALS (18) is rejected on the bound check alone,
    // before even comparing against the token's real decimals.
    let result = t.contract.try_correct_token_decimals(&t.usdc, &19);
    assert_eq!(result, Err(Ok(ContractError::InvalidTokenDecimals)));
}

// ─────────────────────────────────────────────────────────────────────────
// correct_token_decimals: success, and its forward-only effect
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_correct_token_decimals_succeeds_with_the_real_value() {
    let t = setup();

    t.contract.correct_token_decimals(&t.usdc, &7);

    assert_eq!(t.contract.get_token_decimals(&t.usdc), Some(7));
    let audit = t.contract.audit_token_decimals(&t.usdc);
    assert!(
        audit.matches(),
        "after correction, registered and on-chain decimals must agree"
    );
}

#[test]
fn test_correct_token_decimals_does_not_retroactively_change_existing_invoices() {
    let t = setup();

    // Submitted while USDC is still registered at 6 decimals.
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );
    let amount_before = t.contract.get_invoice(&id).amount;

    t.contract.correct_token_decimals(&t.usdc, &7);

    let amount_after = t.contract.get_invoice(&id).amount;
    assert_eq!(
        amount_before, amount_after,
        "correcting decimals must not retroactively alter an already-submitted invoice's amount"
    );
}

#[test]
fn test_correct_token_decimals_changes_minimum_amount_for_future_invoices_only() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    // Before correction: 1,000,000 (1 USDC at the registered 6 decimals)
    // is a valid minimum.
    let ok_before = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &1_000_000i128,
        &due_date,
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );
    assert!(ok_before.is_ok());

    t.contract.correct_token_decimals(&t.usdc, &7);

    // After correction: the real minimum is 10,000,000 (1 USDC at the now
    // correctly-registered 7 decimals) -- the same 1,000,000 amount that
    // passed a moment ago is now too small.
    let rejected_after = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &1_000_000i128,
        &due_date,
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );
    assert_eq!(rejected_after, Err(Ok(ContractError::InvalidAmount)));

    // The new minimum itself must succeed.
    let ok_after = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &10_000_000i128,
        &due_date,
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );
    assert!(ok_after.is_ok());
}
