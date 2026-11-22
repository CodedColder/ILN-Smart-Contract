//! Issue #1179 — fee tiers and settlement rounding.
//!
//! `mark_paid` splits the settled amount into a protocol fee (paid to the
//! admin) and a distribution (paid to the funding LPs). This suite pins the
//! **effective fee rate** selection (`effective_fee_rate`) and the
//! **integer-truncation rounding** of every division in that split.
//!
//! All arithmetic here is integer base units; every `/` truncates toward
//! zero, so `protocol_fee` and each funder share round *down* and any
//! remainder ("dust") stays in the contract. `docs/fee-tier-settlement.md`
//! records the verified behavior and the residual risk.
#![cfg(test)]

use super::*;
use crate::constants::MAX_FEE_TIERS;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    vec, Address, Env,
};

const AMOUNT: i128 = 10_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct F {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    token_id: Address,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
}

fn setup() -> F {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc_addr = usdc_id.address();
    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp_a = Address::generate(&env);
    let lp_b = Address::generate(&env);

    let contract_id = env.register(InvoiceLiquidityContract, ());
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

    // Plenty of headroom on every actor, plus a contract buffer so the
    // paths that pay out of the contract's own balance (e.g. a fee above
    // the settled amount) still reach their token transfer.
    token_admin.mint(&payer, &(AMOUNT * 100));
    token_admin.mint(&lp_a, &(AMOUNT * 100));
    token_admin.mint(&lp_b, &(AMOUNT * 100));
    token_admin.mint(&contract.address, &(AMOUNT * 100));

    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let eurc_admin = Address::generate(&env);
    let eurc_id = env.register_stellar_asset_contract_v2(eurc_admin);

    contract.initialize(&admin, &xlm_id.address(), &usdc_addr, &eurc_id.address());

    let mut info = env.ledger().get();
    info.timestamp = 1_700_000_000;
    env.ledger().set(info);

    F {
        env,
        contract,
        token,
        token_id: usdc_addr,
        admin,
        freelancer,
        payer,
        lp_a,
        lp_b,
    }
}

fn advance_ledgers(env: &Env) {
    let mut info = env.ledger().get();
    info.sequence_number += 5_000;
    env.ledger().set(info);
}

fn set_flat_fee(t: &F, rate: u32) {
    advance_ledgers(&t.env);
    t.contract.update_fee_rate(&rate);
}

fn set_tiers(t: &F, tiers: Vec<(i128, u32)>) {
    advance_ledgers(&t.env);
    t.contract.update_fee_tiers(&tiers);
}

fn submit(t: &F, amount: i128) -> u64 {
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &amount,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token_id,
        &ReferralCode::None,
    )
}

// ================================================================
// Rounding
// ================================================================

#[test]
fn flat_fee_truncates_and_settles_exactly() {
    let t = setup();
    set_flat_fee(&t, 100); // 1%
    let amount = 3_333_333;
    let id = submit(&t, amount);
    t.contract.fund_invoice(&t.lp_a, &id, &amount, &false);

    let admin_before = t.token.balance(&t.admin);
    let lp_before = t.token.balance(&t.lp_a);
    let contract_before = t.token.balance(&t.contract.address);

    t.contract.mark_paid(&id, &amount);

    // 3_333_333 * 100 / 10_000 = 33_333.33 -> 33_333 (truncated down).
    let expected_fee = amount * 100 / 10_000;
    assert_eq!(expected_fee, 33_333);
    assert_eq!(amount * 100 % 10_000, 3_300, "a sub-unit remainder is discarded");
    assert_eq!(t.token.balance(&t.admin), admin_before + 33_333);
    assert_eq!(t.token.balance(&t.lp_a), lp_before + (amount - 33_333));
    assert_eq!(
        t.token.balance(&t.contract.address),
        contract_before,
        "fee + distribution conserve the settled amount for a single funder"
    );
}

// ================================================================
// Fee tiers
// ================================================================

#[test]
fn tier_selection_uses_highest_applicable_bracket() {
    let t = setup();
    let tiers = vec![
        &t.env,
        (1_000_000i128, 300u32),
        (5_000_000, 200),
        (20_000_000, 100),
    ];
    set_tiers(&t, tiers);
    let id = submit(&t, AMOUNT); // 10_000_000 -> the (5_000_000, 200) bracket
    t.contract.fund_invoice(&t.lp_a, &id, &AMOUNT, &false);

    let admin_before = t.token.balance(&t.admin);
    let lp_before = t.token.balance(&t.lp_a);
    t.contract.mark_paid(&id, &AMOUNT);

    let expected_fee = AMOUNT * 200 / 10_000;
    assert_eq!(expected_fee, 200_000);
    assert_eq!(t.token.balance(&t.admin), admin_before + 200_000);
    assert_eq!(t.token.balance(&t.lp_a), lp_before + (AMOUNT - 200_000));
}

#[test]
fn amount_below_lowest_tier_pays_zero_fee() {
    let t = setup();
    let tiers = vec![&t.env, (5_000_000i128, 300u32)];
    set_tiers(&t, tiers);
    // At the 6-decimal USDC floor, but below the only bracket's threshold.
    let amount = 1_000_000;
    let id = submit(&t, amount);
    t.contract.fund_invoice(&t.lp_a, &id, &amount, &false);

    let admin_before = t.token.balance(&t.admin);
    let lp_before = t.token.balance(&t.lp_a);
    t.contract.mark_paid(&id, &amount);

    assert_eq!(
        t.token.balance(&t.admin),
        admin_before,
        "no bracket matches -> effective rate 0, no fee"
    );
    assert_eq!(
        t.token.balance(&t.lp_a),
        lp_before + amount,
        "the whole amount is distributed"
    );
}

#[test]
fn tier_walk_follows_storage_order_not_sorted_order() {
    let t = setup();
    // Deliberately NOT sorted ascending by min_amount. The last matching
    // entry in *storage order* wins, so the later (1_000_000, 300) overrides
    // the earlier (5_000_000, 100) even though 5_000_000 is the higher
    // bracket. `update_fee_tiers` does not sort, so selection is
    // storage-order-dependent (see the doc's RR-2).
    let tiers = vec![&t.env, (5_000_000i128, 100u32), (1_000_000, 300)];
    set_tiers(&t, tiers);
    let id = submit(&t, AMOUNT); // 10_000_000 matches both entries
    t.contract.fund_invoice(&t.lp_a, &id, &AMOUNT, &false);

    let admin_before = t.token.balance(&t.admin);
    t.contract.mark_paid(&id, &AMOUNT);

    // Last storage entry (1_000_000, 300) wins -> 300 bps, not 100 bps.
    assert_eq!(
        t.token.balance(&t.admin),
        admin_before + AMOUNT * 300 / 10_000
    );
}

#[test]
fn empty_tiers_fall_back_to_flat_fee_rate() {
    let t = setup();
    set_flat_fee(&t, 500);
    assert_eq!(t.contract.get_fee_tiers().len(), 0);

    let id = submit(&t, AMOUNT);
    t.contract.fund_invoice(&t.lp_a, &id, &AMOUNT, &false);

    let admin_before = t.token.balance(&t.admin);
    t.contract.mark_paid(&id, &AMOUNT);

    assert_eq!(
        t.token.balance(&t.admin),
        admin_before + AMOUNT * 500 / 10_000
    );
}

#[test]
fn update_fee_tiers_rejects_more_than_cap() {
    let t = setup();
    let mut tiers = Vec::new(&t.env);
    for i in 0..(MAX_FEE_TIERS + 1) {
        tiers.push_back((i as i128 + 1, 100u32));
    }
    advance_ledgers(&t.env);
    let res = t.contract.try_update_fee_tiers(&tiers);
    assert_eq!(res, Err(Ok(ContractError::TooManyFeeTiers)));
}

// ================================================================
// Settlement totals
// ================================================================

#[test]
fn funder_share_truncation_retains_dust_in_contract() {
    let t = setup();
    set_flat_fee(&t, 100); // 1%
    let id = submit(&t, AMOUNT);
    let a: i128 = 3_333_333;
    let b: i128 = AMOUNT - a; // 6_666_667
    t.contract.fund_invoice(&t.lp_a, &id, &a, &false);
    t.contract.fund_invoice(&t.lp_b, &id, &b, &false);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Funded);

    let admin_before = t.token.balance(&t.admin);
    let a_before = t.token.balance(&t.lp_a);
    let b_before = t.token.balance(&t.lp_b);
    let contract_before = t.token.balance(&t.contract.address);

    t.contract.mark_paid(&id, &AMOUNT);

    let fee = AMOUNT * 100 / 10_000; // 100_000
    let distribute = AMOUNT - fee; // 9_900_000
    let share_a = distribute * a / AMOUNT; // 3_299_999
    let share_b = distribute * b / AMOUNT; // 6_600_000
    let dust = distribute - share_a - share_b;
    assert_eq!((share_a, share_b, dust), (3_299_999, 6_600_000, 1));

    assert_eq!(t.token.balance(&t.admin), admin_before + fee);
    assert_eq!(t.token.balance(&t.lp_a), a_before + share_a);
    assert_eq!(t.token.balance(&t.lp_b), b_before + share_b);
    assert_eq!(
        t.token.balance(&t.contract.address),
        contract_before + dust,
        "truncation dust stays in the contract, it is not redistributed"
    );
}

#[test]
fn settlement_conserves_value_between_fee_and_lp() {
    let t = setup();
    set_flat_fee(&t, 250); // 2.5%
    let id = submit(&t, AMOUNT);
    t.contract.fund_invoice(&t.lp_a, &id, &AMOUNT, &false);

    let admin_before = t.token.balance(&t.admin);
    let lp_before = t.token.balance(&t.lp_a);
    let contract_before = t.token.balance(&t.contract.address);

    t.contract.mark_paid(&id, &AMOUNT);

    let fee = AMOUNT * 250 / 10_000; // 250_000
    assert_eq!(fee + (AMOUNT - fee), AMOUNT);
    assert_eq!(t.token.balance(&t.admin), admin_before + fee);
    assert_eq!(t.token.balance(&t.lp_a), lp_before + (AMOUNT - fee));
    assert_eq!(
        t.token.balance(&t.contract.address),
        contract_before,
        "settlement moved exactly the settled amount out of the contract"
    );
}

#[test]
fn fee_rate_above_ten_thousand_saturates_lp_distribution_to_zero() {
    let t = setup();
    // There is no on-chain ceiling on the flat fee rate (see the doc's
    // RR-1). A rate above 10_000 bps (100%) makes the fee exceed the settled
    // amount, so `distribute` saturates to 0 and the LP receives nothing.
    set_flat_fee(&t, 20_000);
    let id = submit(&t, AMOUNT);
    t.contract.fund_invoice(&t.lp_a, &id, &AMOUNT, &false);

    let admin_before = t.token.balance(&t.admin);
    let lp_before = t.token.balance(&t.lp_a);
    t.contract.mark_paid(&id, &AMOUNT);

    let fee = AMOUNT * 20_000 / 10_000; // 2 * AMOUNT, paid out of the buffer
    assert_eq!(t.token.balance(&t.admin), admin_before + fee);
    assert_eq!(
        t.token.balance(&t.lp_a),
        lp_before,
        "LP distribution saturates to 0"
    );
}
