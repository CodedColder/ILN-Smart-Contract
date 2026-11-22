//! Issue #1159 — cross-contract invoice lifecycle and accounting invariants.
//!
//! Proves that a failed lifecycle transaction leaves invoice, LP, insurance
//! and distribution accounting unchanged (rolled back atomically), and that
//! successful settlement / default are applied to every contract exactly
//! once. Uses the *real* `iln_distribution` and `insurance_pool` contracts
//! plus the shared `MockToken` (which can inject a transfer failure).
//!
//! Invariant IDs refer to `docs/invoice-lifecycle-invariants.md`.
//!
//! Issue #1160 extends this with token/obligation conservation: registered
//! obligations X1-X5 and X9-X12 (see `docs/proof-obligations.json` and
//! `docs/cross-contract-accounting-audit.md`). Every `#[test]` here must be
//! claimed by an obligation, or `scripts/check-proof-obligations.py` fails.
//!
//! NOTE: `mock_all_auths()` (root-only) is used deliberately instead of
//! `mock_all_auths_allowing_non_root_auth()`: a distribution contract that
//! is wired to a *different* ILN address must reject ILN's nested
//! `accrue_*` call, exactly as it would on-chain.

extern crate std;

#[path = "mocks/mock_token.rs"]
mod mock_token;

use iln_distribution::{IlnDistribution, IlnDistributionClient};
use insurance_pool::{InsurancePool, InsurancePoolClient};
use invoice_liquidity::{
    ContractError, InvoiceLiquidityContract, InvoiceLiquidityContractClient, InvoiceStatus,
    ReferralCode,
};
use mock_token::{MockToken, MockTokenClient};
use soroban_sdk::{
    contracttype,
    testutils::{Address as _, EnvTestConfig, Ledger},
    Address, Env,
};

const AMOUNT: i128 = 1_000_000_000;
const RATE: u32 = 300;
const DAY: u64 = 24 * 60 * 60;
const T0: u64 = 1_700_000_000;
const SEQ0: u32 = 10_000;
const COVERAGE_CAP: i128 = 1_000;
const PREMIUM: i128 = 600;
/// ILN treasury reserve. `claim_default` and upheld disputes refund LP
/// principal that was already forwarded to the freelancer at funding time,
/// so those refunds are paid out of ILN's pooled balance, not a per-invoice
/// escrow (see [NE-2] in the invariants doc).
const RESERVE: i128 = 10 * AMOUNT;

/// Same storage key layout as `MockToken`'s private key enum, so a test can
/// disarm `fail_next_transfer`. Disarming is necessary because the flag's
/// one-shot `remove` is itself rolled back with the failed transaction —
/// which is precisely the atomicity property under test.
#[contracttype]
enum MockTokenKey {
    #[allow(dead_code)]
    Balance(Address),
    FailNext,
}

struct World {
    env: Env,
    iln: InvoiceLiquidityContractClient<'static>,
    token: MockTokenClient<'static>,
    dist: IlnDistributionClient<'static>,
    pool: InsurancePoolClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    lp2: Address,
}

/// Everything a failed transaction must leave untouched.
#[derive(Debug, PartialEq)]
struct Ledgers {
    invoice: invoice_liquidity::Invoice,
    lp_index: u32,
    total_funded: u64,
    total_paid: u64,
    payer_score: u32,
    balances: std::vec::Vec<i128>,
    pool_balance: i128,
    pool_claimed: bool,
    accruals: std::vec::Vec<i128>,
}

fn advance(env: &Env, secs: u64, ledgers: u32) {
    let mut li = env.ledger().get();
    li.timestamp += secs;
    li.sequence_number += ledgers;
    env.ledger().set(li);
}

fn world() -> World {
    world_with_reserve(RESERVE)
}

fn world_with_reserve(reserve: i128) -> World {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    let mut li = env.ledger().get();
    li.timestamp = T0;
    li.sequence_number = SEQ0;
    env.ledger().set(li);

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);
    let lp2 = Address::generate(&env);

    let token_id = env.register(MockToken, ());
    let token = MockTokenClient::new(&env, &token_id);
    for a in [&lp, &lp2, &payer] {
        token.mint(a, &(10 * AMOUNT));
    }

    let xlm = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let iln_id = env.register(InvoiceLiquidityContract, ());
    let iln = InvoiceLiquidityContractClient::new(&env, &iln_id);
    iln.initialize(&admin, &token_id, &Address::generate(&env), &xlm);
    if reserve > 0 {
        token.mint(&iln_id, &reserve);
    }

    let gov = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let dist_id = env.register(IlnDistribution, ());
    let dist = IlnDistributionClient::new(&env, &dist_id);
    dist.initialize(&iln_id, &gov);
    iln.set_distribution_contract(&dist_id);

    let pool_id = env.register(InsurancePool, ());
    let pool = InsurancePoolClient::new(&env, &pool_id);
    pool.initialize(&iln_id, &COVERAGE_CAP, &token_id);
    iln.set_insurance_pool(&pool_id);

    World {
        env,
        iln,
        token,
        dist,
        pool,
        admin,
        freelancer,
        payer,
        lp,
        lp2,
    }
}

impl World {
    fn submit(&self) -> u64 {
        self.iln.submit_invoice(
            &self.freelancer,
            &self.payer,
            &AMOUNT,
            &(T0 + 30 * DAY),
            &RATE,
            &self.token.address,
            &ReferralCode::None,
        )
    }

    fn actors(&self) -> [Address; 7] {
        [
            self.admin.clone(),
            self.freelancer.clone(),
            self.payer.clone(),
            self.lp.clone(),
            self.lp2.clone(),
            self.iln.address.clone(),
            self.pool.address.clone(),
        ]
    }

    fn ledgers(&self, id: u64) -> Ledgers {
        let stats = self.iln.get_contract_stats();
        Ledgers {
            invoice: self.iln.get_invoice(&id),
            lp_index: self.iln.list_invoices_by_lp(&self.lp, &0, &50).len(),
            total_funded: stats.total_funded,
            total_paid: stats.total_paid,
            payer_score: self.iln.payer_score(&self.payer),
            balances: self
                .actors()
                .iter()
                .map(|a| self.token.balance(a))
                .collect(),
            pool_balance: self.pool.get_pool_balance(),
            pool_claimed: self.pool.is_claimed(&id),
            accruals: [&self.lp, &self.lp2, &self.freelancer, &self.payer]
                .iter()
                .map(|a| self.dist.get_accrual(a))
                .collect(),
        }
    }

    fn total_tokens(&self) -> i128 {
        self.actors().iter().map(|a| self.token.balance(a)).sum()
    }

    /// Re-wire ILN to a distribution contract (admin call, rate-limited).
    fn rewire_distribution(&self, dist: &Address) {
        advance(&self.env, 0, 200);
        self.iln.set_distribution_contract(dist);
    }

    /// A real distribution contract that trusts a *different* ILN address,
    /// so every `accrue_*` call from this ILN fails authorization.
    fn miswired_distribution(&self) -> Address {
        let gov = self
            .env
            .register_stellar_asset_contract_v2(Address::generate(&self.env))
            .address();
        let id = self.env.register(IlnDistribution, ());
        IlnDistributionClient::new(&self.env, &id).initialize(&Address::generate(&self.env), &gov);
        id
    }

    fn disarm_token_failure(&self) {
        let env = &self.env;
        env.as_contract(&self.token.address, || {
            env.storage().temporary().remove(&MockTokenKey::FailNext)
        });
    }
}

/// [RB-2] Funding: the LP->ILN and ILN->freelancer token transfers have
/// already executed when the distribution hook fails; all of it must revert.
#[test]
fn xc_funding_rolls_back_when_distribution_accrual_fails() {
    let w = world();
    let bad = w.miswired_distribution();
    w.rewire_distribution(&bad);
    let id = w.submit();

    let before = w.ledgers(id);
    for amt in [AMOUNT / 2, AMOUNT] {
        assert!(w.iln.try_fund_invoice(&w.lp, &id, &amt, &false).is_err());
        assert_eq!(w.ledgers(id), before, "fund {amt}: nothing may persist");
    }
    assert_eq!(before.invoice.status, InvoiceStatus::Pending);
    assert_eq!(before.invoice.amount_funded, 0);
}

/// [RB-2] Settlement: payer->ILN, ILN->LP and fee transfers, status=Paid
/// and reputation updates all revert if the distribution settlement hook
/// fails; once the hook works, settlement is applied (and accrued) once.
#[test]
fn xc_settlement_rolls_back_when_distribution_settlement_fails() {
    let w = world();
    let good = w.dist.address.clone();
    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    assert!(w.dist.get_accrual(&w.lp) > 0);

    let bad = w.miswired_distribution();
    w.rewire_distribution(&bad);
    let before = w.ledgers(id);
    assert!(w.iln.try_mark_paid(&id, &AMOUNT).is_err());
    assert_eq!(w.ledgers(id), before);
    assert_eq!(before.invoice.status, InvoiceStatus::Funded);
    assert_eq!(before.invoice.amount_paid, 0);

    // Partial payments do not call the settlement hook, so they succeed.
    w.iln.mark_paid(&id, &(AMOUNT / 2));
    let partial = w.ledgers(id);
    assert_eq!(partial.invoice.amount_paid, AMOUNT / 2);
    assert!(w.iln.try_mark_paid(&id, &(AMOUNT / 2)).is_err());
    assert_eq!(w.ledgers(id), partial);

    w.rewire_distribution(&good);
    let freelancer_accrual = w.dist.get_accrual(&w.freelancer);
    w.iln.mark_paid(&id, &(AMOUNT / 2));
    let settled = w.ledgers(id);
    assert_eq!(settled.invoice.status, InvoiceStatus::Paid);
    assert_eq!(settled.total_paid, before.total_paid + 1);
    assert!(w.dist.get_accrual(&w.freelancer) > freelancer_accrual);

    // [SET-1] settlement cannot be applied (or accrued) a second time.
    assert_eq!(
        w.iln.try_mark_paid(&id, &1),
        Err(Ok(ContractError::AlreadyPaid))
    );
    assert_eq!(w.ledgers(id), settled);
}

/// [RB-2] A token transfer failure aborts funding with no trace in ILN,
/// LP balances or distribution accruals.
#[test]
fn xc_token_failure_rolls_back_funding_and_accrual() {
    let w = world();
    let id = w.submit();
    let before = w.ledgers(id);

    // Partial and full tranches alike leave no trace when a transfer fails.
    for amt in [AMOUNT / 4, AMOUNT] {
        w.token.fail_next_transfer();
        assert!(w.iln.try_fund_invoice(&w.lp, &id, &amt, &false).is_err());
        assert_eq!(w.ledgers(id), before, "fund {amt}");
        w.disarm_token_failure();
    }

    // (Distribution accrues per whole 100-USDC of volume, so fund the full
    // 100-USDC invoice in one tranche to make the accrual observable.)
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    let after = w.ledgers(id);
    assert_eq!(after.invoice.status, InvoiceStatus::Funded);
    assert_eq!(after.invoice.amount_funded, AMOUNT);
    assert_eq!(after.lp_index, 1);
    assert!(
        after.accruals[0] > before.accruals[0],
        "accrual only on success"
    );
}

/// [RB-3] `claim_default` refunds the LP and then claims insurance. If the
/// refund transfer fails, the insurance pool must not have paid out or
/// marked the invoice claimed, and the invoice must still be Funded.
/// Once it succeeds, the insurance payout happens exactly once.
#[test]
fn xc_failed_default_leaves_insurance_untouched_and_success_is_once() {
    let w = world();
    w.token.mint(&w.lp, &PREMIUM);
    w.pool.deposit_premium(&w.lp, &PREMIUM);
    assert!(w.pool.is_enrolled(&w.lp));

    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);

    let before = w.ledgers(id);
    w.token.fail_next_transfer();
    assert!(w.iln.try_claim_default(&w.lp, &id).is_err());
    assert_eq!(
        w.ledgers(id),
        before,
        "no partial default / insurance state"
    );
    w.disarm_token_failure();

    let supply = w.total_tokens();
    let lp_before = w.token.balance(&w.lp);
    w.iln.claim_default(&w.lp, &id);
    let after = w.ledgers(id);
    assert_eq!(after.invoice.status, InvoiceStatus::Defaulted);
    assert!(after.pool_claimed);
    let payout = before.pool_balance - after.pool_balance;
    assert!(payout > 0 && payout <= COVERAGE_CAP);
    assert_eq!(
        w.token.balance(&w.lp) - lp_before,
        AMOUNT - AMOUNT * RATE as i128 / 10_000 + payout,
        "LP gets principal refund + insurance payout, once"
    );
    assert_eq!(
        w.total_tokens(),
        supply,
        "[ACC-6] tokens conserved across ILN + pool"
    );

    // [SET-2] a second default claim changes nothing in either contract.
    assert_eq!(
        w.iln.try_claim_default(&w.lp, &id),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
    assert_eq!(w.ledgers(id), after);
}

/// An insurance payout failure (empty pool) is tolerated by design: the
/// default still completes, and the pool's accounting is untouched (no
/// partial claim marker, no balance change).
#[test]
fn xc_insurance_failure_does_not_corrupt_pool_or_block_default() {
    let w = world();
    w.pool.enroll(&w.lp); // enrolled but the pool holds no reserve
    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);

    let before = w.ledgers(id);
    w.iln.claim_default(&w.lp, &id);
    let after = w.ledgers(id);
    assert_eq!(after.invoice.status, InvoiceStatus::Defaulted);
    assert_eq!(after.pool_balance, before.pool_balance);
    assert!(
        !after.pool_claimed,
        "failed insurance claim leaves no marker"
    );
    assert_eq!(
        after.accruals, before.accruals,
        "default accrues no rewards"
    );
}

/// Upheld dispute on a Funded invoice draws LP refunds from ILN's own
/// balance ([NE-2]). With an insufficient balance, the resolution reverts
/// atomically, including the distribution/insurance side.
#[test]
fn xc_insolvent_dispute_resolution_reverts_atomically() {
    let w = world_with_reserve(0);
    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    w.iln
        .dispute_invoice(&id, &soroban_sdk::BytesN::from_array(&w.env, &[1; 32]));
    let before = w.ledgers(id);
    assert!(w
        .iln
        .try_resolve_dispute(&id, &soroban_sdk::BytesN::from_array(&w.env, &[2; 32]), &1)
        .is_err());
    assert_eq!(w.ledgers(id), before);
    assert_eq!(before.invoice.status, InvoiceStatus::Disputed);
}

/// End-to-end: one invoice settled, one defaulted with insurance, a failed
/// funding attempt in between. Token supply is conserved across every
/// contract and distribution accrues only for successful operations.
#[test]
fn xc_end_to_end_accounting_is_conserved() {
    let w = world();
    w.token.mint(&w.lp, &PREMIUM);
    w.pool.deposit_premium(&w.lp, &PREMIUM);
    let supply = w.total_tokens();

    let a = w.submit();
    w.iln.fund_invoice(&w.lp, &a, &AMOUNT, &false);
    let accrual_one = w.dist.get_accrual(&w.lp);
    assert!(accrual_one > 0);

    let b = w.submit();
    assert_eq!(
        w.iln.try_fund_invoice(&w.lp, &b, &(AMOUNT + 1), &false),
        Err(Ok(ContractError::OverfundingRejected))
    );
    assert_eq!(w.dist.get_accrual(&w.lp), accrual_one);
    w.iln.fund_invoice(&w.lp, &b, &AMOUNT, &false);
    assert_eq!(w.dist.get_accrual(&w.lp), 2 * accrual_one);

    w.iln.mark_paid(&a, &AMOUNT);
    advance(&w.env, 31 * DAY, 100);
    w.iln.claim_default(&w.lp, &b);

    assert_eq!(w.iln.get_invoice(&a).status, InvoiceStatus::Paid);
    assert_eq!(w.iln.get_invoice(&b).status, InvoiceStatus::Defaulted);
    assert!(w.pool.is_claimed(&b) && !w.pool.is_claimed(&a));
    assert_eq!(w.total_tokens(), supply);
    assert!(w.token.balance(&w.iln.address) >= 0);
    let stats = w.iln.get_contract_stats();
    assert_eq!((stats.total_funded, stats.total_paid), (2, 1));
}

// ===========================================================================
// Issue #1100 / #1160 — registered obligations X1–X5
//
// These are the tests named by `docs/proof-obligations.json`. Until they
// existed the registry check (scripts/check-proof-obligations.py and
// `proof_obligations_registry_is_consistent`) failed on a clean tree.
// ===========================================================================

/// Protocol fee used by the ledger tests. Chosen non-zero so the fee leg of
/// settlement is observable; `AMOUNT * FEE_BPS / 10_000` has no remainder.
const FEE_BPS: u32 = 50;

/// Discount withheld from a tranche (same integer formula as the contract).
fn discount(amount: i128) -> i128 {
    amount * RATE as i128 / 10_000
}

/// What an LP actually transfers for a tranche (and what a refund returns).
fn cost(amount: i128) -> i128 {
    amount - discount(amount)
}

fn fee_of(amount: i128) -> i128 {
    amount * FEE_BPS as i128 / 10_000
}

/// Settlement share owed to an LP that funded `amount` of an `AMOUNT` invoice.
fn share(amount: i128) -> i128 {
    (AMOUNT - fee_of(AMOUNT)) * amount / AMOUNT
}

impl World {
    fn bal(&self, a: &Address) -> i128 {
        self.token.balance(a)
    }

    fn iln_balance(&self) -> i128 {
        self.token.balance(&self.iln.address)
    }

    #[track_caller]
    fn assert_conserved(&self, supply: i128, step: &str) {
        assert_eq!(self.total_tokens(), supply, "tokens not conserved: {step}");
    }

    /// The pool's token balance must equal its liquid + backstop accounting.
    #[track_caller]
    fn assert_pool_backed(&self, step: &str) {
        assert_eq!(
            self.pool.get_pool_balance() + self.pool.get_backstop_balance(),
            self.token.balance(&self.pool.address),
            "pool accounting != pool token balance: {step}"
        );
    }
}

/// [X1] A successful funding accrues distribution volume for the funding LP
/// and for no one else.
#[test]
fn x1_funding_increases_lp_distribution_accrual() {
    let w = world();
    let id = w.submit();
    let (lp_before, other_before, fl_before) = (
        w.dist.get_accrual(&w.lp),
        w.dist.get_accrual(&w.lp2),
        w.dist.get_accrual(&w.freelancer),
    );

    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);

    assert!(w.dist.get_accrual(&w.lp) > lp_before, "funding LP accrues");
    assert_eq!(w.dist.get_accrual(&w.lp2), other_before, "bystander LP");
    assert_eq!(w.dist.get_accrual(&w.freelancer), fl_before, "freelancer");
}

/// [X2] A successful on-time settlement accrues to the freelancer (and the
/// payer, when its rate is non-zero) and never decreases anyone's accrual.
#[test]
fn x2_settlement_increases_freelancer_distribution_accrual() {
    let w = world();
    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    let (fl, payer, lp) = (
        w.dist.get_accrual(&w.freelancer),
        w.dist.get_accrual(&w.payer),
        w.dist.get_accrual(&w.lp),
    );

    w.iln.mark_paid(&id, &AMOUNT);

    assert!(w.dist.get_accrual(&w.freelancer) > fl, "freelancer accrues");
    assert!(w.dist.get_accrual(&w.payer) >= payer);
    assert_eq!(w.dist.get_accrual(&w.lp), lp, "settlement does not touch LP");
}

/// [X3] `claim_default` for an enrolled LP lowers the pool by exactly the
/// compensation the LP receives; an un-enrolled LP leaves the pool untouched.
#[test]
fn x3_default_claim_reduces_insurance_pool_by_payout() {
    let w = world();
    w.token.mint(&w.lp, &PREMIUM);
    w.pool.deposit_premium(&w.lp, &PREMIUM);
    assert!(w.pool.is_enrolled(&w.lp) && !w.pool.is_enrolled(&w.lp2));

    let insured = w.submit();
    let uninsured = w.submit();
    w.iln.fund_invoice(&w.lp, &insured, &AMOUNT, &false);
    w.iln.fund_invoice(&w.lp2, &uninsured, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);

    let pool_before = w.pool.get_pool_balance();
    let lp_before = w.bal(&w.lp);
    w.iln.claim_default(&w.lp, &insured);

    let payout = pool_before - w.pool.get_pool_balance();
    assert!(payout > 0 && payout <= pool_before, "pool never overdrawn");
    assert!(w.pool.get_pool_balance() >= 0);
    assert_eq!(w.bal(&w.lp) - lp_before, cost(AMOUNT) + payout);
    w.assert_pool_backed("after insured default");

    let pool_mid = w.pool.get_pool_balance();
    w.iln.claim_default(&w.lp2, &uninsured);
    assert_eq!(w.pool.get_pool_balance(), pool_mid, "un-enrolled: no draw");
    assert!(!w.pool.is_claimed(&uninsured));
    assert_eq!(
        w.iln.get_invoice(&uninsured).status,
        InvoiceStatus::Defaulted,
        "default itself still applies"
    );
}

/// [X4] Every step of a mixed lifecycle conserves the payment-token supply
/// across LPs, payer, freelancer, admin, ILN and the insurance pool.
#[test]
fn x4_payment_token_conserved_across_iln_and_insurance() {
    let w = world();
    w.token.mint(&w.lp, &PREMIUM);
    let supply = w.total_tokens();

    w.pool.deposit_premium(&w.lp, &PREMIUM);
    w.assert_conserved(supply, "premium deposit");

    let (a, b, c) = (w.submit(), w.submit(), w.submit());
    w.iln.fund_invoice(&w.lp, &a, &AMOUNT, &false);
    w.assert_conserved(supply, "full funding a");
    w.iln.fund_invoice(&w.lp2, &b, &(AMOUNT / 2), &false);
    w.assert_conserved(supply, "partial funding b");
    w.iln.fund_invoice(&w.lp, &c, &AMOUNT, &false);
    w.assert_conserved(supply, "full funding c");

    w.iln.mark_paid(&a, &(AMOUNT / 2));
    w.assert_conserved(supply, "partial payment a");
    w.iln.mark_paid(&a, &(AMOUNT / 2));
    w.assert_conserved(supply, "settlement a");
    w.iln.cancel_invoice(&b);
    w.assert_conserved(supply, "cancel b");

    advance(&w.env, 31 * DAY, 100);
    w.iln.claim_default(&w.lp, &c);
    w.assert_conserved(supply, "default c + insurance");
    w.assert_pool_backed("end of sequence");
}

/// [X5] Insurance compensation is paid once per invoice: a repeated default
/// claim fails, and so does a direct second pool claim; the pool is unchanged.
#[test]
fn x5_insurance_compensation_not_paid_twice_for_same_invoice() {
    let w = world();
    w.token.mint(&w.lp, &PREMIUM);
    w.pool.deposit_premium(&w.lp, &PREMIUM);
    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);

    w.iln.claim_default(&w.lp, &id);
    assert!(w.pool.is_claimed(&id));
    let after = w.ledgers(id);

    assert_eq!(
        w.iln.try_claim_default(&w.lp, &id),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
    assert!(
        w.pool.try_claim(&id, &w.lp).is_err(),
        "pool itself rejects a second claim for the same invoice"
    );
    assert_eq!(w.ledgers(id), after, "no second payout, no state change");
    w.assert_pool_backed("after rejected second claim");
}

// ===========================================================================
// Issue #1160 — conservation of token balances and obligations
//
// Notation: cost(f) = f - discount(f) is what an LP transfers for a tranche
// `f`; `share(f)` its settlement payout; `dust` the integer-division
// remainder that stays in ILN (always >= 0 and < number of funders).
// ===========================================================================

/// [X11] Exact per-actor token flows for fund -> settle with a protocol fee
/// and three odd-sized tranches, with no treasury reserve: settlement is
/// funded only by the payer, and every unit is accounted for.
#[test]
fn xc_funding_and_settlement_ledger_is_exact_with_bounded_dust() {
    let w = world_with_reserve(0);
    w.iln.update_fee_rate(&FEE_BPS);
    let lp3 = Address::generate(&w.env);
    w.token.mint(&lp3, &(10 * AMOUNT));
    let supply = w.total_tokens() + w.bal(&lp3);

    let lps = [
        (w.lp.clone(), 333_333_333_i128),
        (w.lp2.clone(), 333_333_333),
        (lp3.clone(), 333_333_334),
    ];
    assert_eq!(lps.iter().map(|(_, f)| f).sum::<i128>(), AMOUNT);
    let start: std::vec::Vec<i128> = lps.iter().map(|(a, _)| w.bal(a)).collect();

    let id = w.submit();
    for (lp, f) in &lps {
        w.iln.fund_invoice(lp, &id, f, &false);
    }

    // Funding ledger.
    for (i, (lp, f)) in lps.iter().enumerate() {
        assert_eq!(start[i] - w.bal(lp), cost(*f), "LP {i} pays its discounted cost");
    }
    let total_cost: i128 = lps.iter().map(|(_, f)| cost(*f)).sum();
    let payout = AMOUNT - discount(AMOUNT);
    assert_eq!(w.bal(&w.freelancer), payout);
    let dust_fund = total_cost - payout;
    assert!((0..lps.len() as i128).contains(&dust_fund), "dust {dust_fund}");
    assert_eq!(w.iln_balance(), dust_fund, "ILN keeps only rounding dust");
    w.assert_conserved(supply, "after funding");

    // Settlement ledger.
    let payer_before = w.bal(&w.payer);
    w.iln.mark_paid(&id, &AMOUNT);
    assert_eq!(payer_before - w.bal(&w.payer), AMOUNT, "payer pays exactly once");
    assert_eq!(w.bal(&w.admin), fee_of(AMOUNT), "fee goes to the admin");
    for (i, (lp, f)) in lps.iter().enumerate() {
        assert_eq!(
            w.bal(lp),
            start[i] - cost(*f) + share(*f),
            "LP {i} net = share - cost"
        );
    }
    let paid_out = fee_of(AMOUNT) + lps.iter().map(|(_, f)| share(*f)).sum::<i128>();
    let dust_settle = AMOUNT - paid_out;
    assert!(
        (0..lps.len() as i128).contains(&dust_settle),
        "settlement never pays out more than it received (dust {dust_settle})"
    );
    assert_eq!(w.iln_balance(), dust_fund + dust_settle);
    w.assert_conserved(supply, "after settlement");
}

/// [X11] Cancelling a partially funded invoice refunds every LP exactly what
/// it deposited, leaves ILN at zero, and is not repeatable.
#[test]
fn xc_partial_funding_cancel_refunds_exact_deposits() {
    let w = world_with_reserve(0);
    let supply = w.total_tokens();
    let (lp_start, lp2_start) = (w.bal(&w.lp), w.bal(&w.lp2));

    let id = w.submit();
    let t = AMOUNT / 3; // 333_333_333: not divisible by the discount grid
    w.iln.fund_invoice(&w.lp, &id, &t, &false);
    w.iln.fund_invoice(&w.lp2, &id, &t, &false);
    assert_eq!(w.iln_balance(), cost(t) * 2, "ILN holds exactly the deposits");

    w.iln.cancel_invoice(&id);
    assert_eq!(w.bal(&w.lp), lp_start);
    assert_eq!(w.bal(&w.lp2), lp2_start);
    assert_eq!(w.iln_balance(), 0);
    w.assert_conserved(supply, "after cancel");
    let inv = w.iln.get_invoice(&id);
    assert_eq!(inv.status, InvoiceStatus::Cancelled);
    assert_eq!(inv.amount_funded, 2 * t, "[NE-1] history, not net position");

    let after = w.ledgers(id);
    assert_eq!(
        w.iln.try_cancel_invoice(&id),
        Err(Ok(ContractError::AlreadyCancelled))
    );
    assert_eq!(w.ledgers(id), after, "repeated cancel changes nothing");
}

/// [X11] [NE-2] An upheld dispute on a Funded invoice with a partial payment
/// returns the LP's cost and the payer's payment in full, so the *only*
/// net transfer is the freelancer's payout, which ILN's reserve absorbs.
#[test]
fn xc_upheld_dispute_ledger_reserve_bears_freelancer_payout() {
    let w = world();
    let supply = w.total_tokens();
    let (lp0, payer0, iln0) = (w.bal(&w.lp), w.bal(&w.payer), w.iln_balance());

    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    w.iln.mark_paid(&id, &(AMOUNT / 2));
    w.iln
        .dispute_invoice(&id, &soroban_sdk::BytesN::from_array(&w.env, &[1; 32]));
    w.iln
        .resolve_dispute(&id, &soroban_sdk::BytesN::from_array(&w.env, &[2; 32]), &1);

    let payout = AMOUNT - discount(AMOUNT);
    assert_eq!(w.iln.get_invoice(&id).status, InvoiceStatus::Cancelled);
    assert_eq!(w.bal(&w.lp), lp0, "LP made whole");
    assert_eq!(w.bal(&w.payer), payer0, "payer made whole");
    assert_eq!(w.bal(&w.freelancer), payout, "freelancer keeps the payout");
    assert_eq!(iln0 - w.iln_balance(), payout, "reserve bears it");
    w.assert_conserved(supply, "after upheld dispute");
}

/// [X11] Characterization of a verified gap (see `docs/cross-contract-accounting-audit.md`
/// F-2): distribution volume is accrued at funding time and is never
/// reversed when the funding is refunded. A funder who is made whole by a
/// cancellation still keeps the reward volume. If this test starts failing
/// because accrual is now reversed, update the audit doc and registry.
#[test]
fn xc_refunded_funding_volume_still_accrues_lp_reward() {
    let w = world_with_reserve(0);
    let lp0 = w.bal(&w.lp);
    assert_eq!(w.dist.get_accrual(&w.lp), 0);

    // Two fund-then-cancel round trips of half an invoice each: 100 USDC of
    // cumulative volume (the reward granularity) with zero net token cost.
    for _ in 0..2 {
        let id = w.submit();
        w.iln.fund_invoice(&w.lp, &id, &(AMOUNT / 2), &false);
        w.iln.cancel_invoice(&id);
        assert_eq!(w.iln.get_invoice(&id).status, InvoiceStatus::Cancelled);
    }

    assert_eq!(w.bal(&w.lp), lp0, "LP fully refunded");
    assert!(
        w.dist.get_accrual(&w.lp) > 0,
        "yet reward volume from the refunded funding remains accrued"
    );
}

/// [X12] Obligation coverage: with no treasury reserve, ILN's token balance
/// is always exactly the refundable deposits of unfunded invoices plus the
/// partial payments of unsettled ones, and returns to zero once every
/// obligation is discharged.
#[test]
fn xc_iln_balance_equals_outstanding_obligations() {
    let w = world_with_reserve(0);
    w.iln.update_fee_rate(&FEE_BPS);
    let supply = w.total_tokens();
    let lp_start = w.bal(&w.lp);

    // A: half funded -> ILN owes the LP its deposit back.
    let a = w.submit();
    w.iln.fund_invoice(&w.lp, &a, &(AMOUNT / 2), &false);
    let owed_a = cost(AMOUNT / 2);
    assert_eq!(w.iln_balance(), owed_a);

    // B: fully funded, quarter paid -> ILN holds the payer's partial payment.
    let b = w.submit();
    w.iln.fund_invoice(&w.lp2, &b, &AMOUNT, &false);
    assert_eq!(w.iln_balance(), owed_a, "full funding is passed straight through");
    w.iln.mark_paid(&b, &(AMOUNT / 4));
    assert_eq!(w.iln_balance(), owed_a + AMOUNT / 4);

    // C: funded and fully settled -> leaves no obligation behind.
    let c = w.submit();
    w.iln.fund_invoice(&w.lp, &c, &AMOUNT, &false);
    w.iln.mark_paid(&c, &AMOUNT);
    assert_eq!(w.iln_balance(), owed_a + AMOUNT / 4, "settled invoice owes nothing");
    w.assert_conserved(supply, "mixed obligations");

    // Discharge A and B; ILN must end at exactly zero.
    w.iln.cancel_invoice(&a);
    assert_eq!(w.iln_balance(), AMOUNT / 4);
    w.iln.mark_paid(&b, &(AMOUNT - AMOUNT / 4));
    assert_eq!(w.iln_balance(), 0, "no obligation left, no stranded tokens");
    assert_eq!(
        w.bal(&w.lp),
        lp_start - cost(AMOUNT) + share(AMOUNT),
        "LP net: A refunded in full, C earned its share"
    );
    w.assert_conserved(supply, "all obligations discharged");
}

/// [X12] The insurance pool's accounting always matches the tokens it holds,
/// through deposit, payout, and a rejected repeat claim.
#[test]
fn xc_insurance_pool_token_balance_matches_accounting() {
    let w = world();
    w.assert_pool_backed("empty pool");
    w.token.mint(&w.lp, &PREMIUM);
    w.pool.deposit_premium(&w.lp, &PREMIUM);
    w.assert_pool_backed("after deposit");
    assert_eq!(w.token.balance(&w.pool.address), PREMIUM);

    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);
    w.iln.claim_default(&w.lp, &id);
    w.assert_pool_backed("after payout");
    assert!(w.token.balance(&w.pool.address) < PREMIUM, "payout left the pool");

    assert!(w.iln.try_claim_default(&w.lp, &id).is_err());
    w.assert_pool_backed("after rejected repeat claim");
}
