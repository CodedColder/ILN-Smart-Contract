//! Issue #855 — cross-contract system invariants (ILN × distribution × insurance).
//!
//! Spec: docs/formal-verification-cross-contract.md
//!
//! These checks deliberately read state from more than one contract after a
//! realistic multi-contract sequence. They complement per-contract proptest
//! suites, which cannot observe foreign storage.

#![cfg(test)]

extern crate std;

#[path = "mocks/mock_token.rs"]
mod mock_token;

use mock_token::{MockToken, MockTokenClient};

use insurance_pool::{InsurancePool, InsurancePoolClient};
use invoice_liquidity::{InvoiceLiquidityContract, InvoiceLiquidityContractClient, ReferralCode};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
const COVERAGE_CAP: i128 = 1_000;
const GOV_MINT: i128 = 10_000;

fn advance_ledger(env: &Env, sequence_delta: u32, timestamp_delta: u64) {
    let mut info = env.ledger().get();
    info.sequence_number += sequence_delta;
    info.timestamp += timestamp_delta;
    env.ledger().set(info);
}

struct Harness {
    env: Env,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    payment_token: MockTokenClient<'static>,
    payment_token_addr: Address,
    iln: InvoiceLiquidityContractClient<'static>,
    iln_id: Address,
    dist: iln_distribution::IlnDistributionClient<'static>,
    pool: InsurancePoolClient<'static>,
    pool_id: Address,
    /// Sum of all payment-token mints performed by this harness.
    inflows: i128,
}

impl Harness {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);
        let lp = Address::generate(&env);

        let payment_token_addr = env.register_contract(None, MockToken);
        let payment_token = MockTokenClient::new(&env, &payment_token_addr);

        let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let xlm_addr = xlm_id.address();
        let eurc_addr = Address::generate(&env);

        let iln_id = env.register_contract(None, InvoiceLiquidityContract);
        let iln = InvoiceLiquidityContractClient::new(&env, &iln_id);
        iln.initialize(&admin, &payment_token_addr, &eurc_addr, &xlm_addr);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 1_700_000_000;
        env.ledger().set(ledger);

        // Governance token for distribution initialize (mint authority).
        let gov_token_admin_addr = Address::generate(&env);
        let gov_token_id = env.register_stellar_asset_contract_v2(gov_token_admin_addr);
        let gov_token_addr = gov_token_id.address();
        StellarAssetClient::new(&env, &gov_token_addr).mint(&admin, &GOV_MINT);

        let dist_id = env.register_contract(None, iln_distribution::IlnDistribution);
        let dist = iln_distribution::IlnDistributionClient::new(&env, &dist_id);
        dist.initialize(&iln_id, &gov_token_addr);

        advance_ledger(&env, 200, 1_000);
        iln.set_distribution_contract(&dist_id);

        let pool_id = env.register_contract(None, InsurancePool);
        let pool = InsurancePoolClient::new(&env, &pool_id);
        pool.initialize(&iln_id, &COVERAGE_CAP, &payment_token_addr);
        advance_ledger(&env, 200, 1_000);
        iln.set_insurance_pool(&pool_id);

        Self {
            env,
            admin,
            freelancer,
            payer,
            lp,
            payment_token,
            payment_token_addr,
            iln,
            iln_id,
            dist,
            pool,
            pool_id,
            inflows: 0,
        }
    }

    fn mint(&mut self, to: &Address, amount: i128) {
        self.payment_token.mint(to, &amount);
        self.inflows += amount;
    }

    fn assert_conservation(&self) {
        let iln_bal = self.payment_token.balance(&self.iln_id);
        let pool_bal = self.payment_token.balance(&self.pool_id);
        let users = self.payment_token.balance(&self.lp)
            + self.payment_token.balance(&self.freelancer)
            + self.payment_token.balance(&self.payer)
            + self.payment_token.balance(&self.admin);
        assert_eq!(
            iln_bal + pool_bal + users,
            self.inflows,
            "X4: ILN escrow + insurance token balance + user balances must equal minted inflows"
        );
    }
}

#[test]
fn x1_funding_increases_lp_distribution_accrual() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2);

    let before = h.dist.get_accrual(&h.lp);
    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);
    let after = h.dist.get_accrual(&h.lp);
    assert!(
        after > before,
        "X1: fund_invoice must increase LP distribution accrual when wired (before={before}, after={after})"
    );
    h.assert_conservation();
}

#[test]
fn x2_settlement_increases_freelancer_distribution_accrual() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);

    let before = h.dist.get_accrual(&h.freelancer);
    h.iln.mark_paid(&id, &INVOICE_AMOUNT);
    let after = h.dist.get_accrual(&h.freelancer);
    assert!(
        after > before,
        "X2: mark_paid must increase freelancer distribution accrual when wired (before={before}, after={after})"
    );
    h.assert_conservation();
}

#[test]
fn x3_default_claim_reduces_insurance_pool_by_payout() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2 + 600);
    h.pool.deposit_premium(&h.lp, &600);
    assert!(h.pool.is_enrolled(&h.lp));
    let pool_before = h.pool.get_pool_balance();
    assert_eq!(pool_before, 600);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);

    let mut info = h.env.ledger().get();
    info.timestamp = due + 1;
    h.env.ledger().set(info);

    h.iln.claim_default(&h.lp, &id);
    let pool_after = h.pool.get_pool_balance();
    assert!(
        pool_after <= pool_before,
        "X3: pool balance must not increase on claim_default"
    );
    assert!(pool_after >= 0, "X3: pool balance must stay non-negative");
    // Enrolled + solvent → some compensation should have been paid for this fixture.
    assert!(
        pool_after < pool_before,
        "X3: enrolled solvent pool should pay out on claim_default (before={pool_before}, after={pool_after})"
    );
    h.assert_conservation();
}

#[test]
fn x4_payment_token_conserved_across_iln_and_insurance() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2 + 600);
    h.pool.deposit_premium(&h.lp, &600);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);
    h.assert_conservation();

    h.iln.mark_paid(&id, &INVOICE_AMOUNT);
    h.assert_conservation();
}

#[test]
fn x5_insurance_compensation_not_paid_twice_for_same_invoice() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2 + 600);
    h.pool.deposit_premium(&h.lp, &600);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);

    let mut info = h.env.ledger().get();
    info.timestamp = due + 1;
    h.env.ledger().set(info);

    h.iln.claim_default(&h.lp, &id);
    let pool_after_first = h.pool.get_pool_balance();

    // Second claim_default must not drain the pool again.
    let second = h.iln.try_claim_default(&h.lp, &id);
    assert!(
        second.is_err(),
        "X5: second claim_default for the same invoice must fail"
    );
    assert_eq!(
        h.pool.get_pool_balance(),
        pool_after_first,
        "X5: pool balance must be unchanged after a rejected re-claim"
    );
    h.assert_conservation();
}
