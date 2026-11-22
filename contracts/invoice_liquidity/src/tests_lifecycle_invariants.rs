//! Issue #1159 — invoice lifecycle and accounting invariant tests.
//!
//! Every test here maps to an invariant listed in
//! `docs/invoice-lifecycle-invariants.md` (IDs in brackets, e.g. `[ACC-1]`).
//! The groups are:
//!
//! * **Accounting bounds** — `amount_funded` / `amount_paid` never exceed
//!   the invoice obligation (`[ACC-1]`, `[ACC-2]`).
//! * **Cumulative accounting** — `PartiallyFunded` / `Funded` agree with the
//!   per-LP contribution ledger (`[ACC-3]`..`[ACC-5]`).
//! * **Authorization** — no lifecycle transition can be driven without the
//!   payer / freelancer / LP / admin signature it requires (`[AUTH-*]`).
//!   These tests use *real* auth enforcement (`mock_auths` with a specific
//!   wrong signer, or no auths at all) — never `mock_all_auths` — for the
//!   negative case, and `env.auths()` to prove the exact signer required on
//!   the positive control.
//! * **Settlement / yield** — settlement and default are one-shot, and only
//!   a legitimately funded-and-settled obligation produces yield
//!   (`[SET-*]`).
//! * **Rollback** — a failed call leaves invoice, LP and token accounting
//!   exactly as it was (`[RB-*]`).
//! * **Property tests** — random interleavings of funding, payment,
//!   cancellation, expiry, dispute, default and appeal, checking every
//!   invariant after every step (`[PROP-1]`).
//!
//! Cross-contract (distribution / insurance) rollback lives in
//! `contracts/tests/cross_contract_invariants_test.rs`.

#![cfg(test)]

use super::*;
use crate::invoice::get_invoice_funders;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    Address, BytesN, Env, IntoVal, Val,
};
// Explicitly import std's `vec!` so it shadows the soroban `vec!` pulled in
// by `use super::*` (proptest's `prop_oneof!` expands to `vec![...]`).
use std::vec;
use std::vec::Vec as StdVec;

const AMOUNT: i128 = 1_000_000_000;
const RATE: u32 = 300;
const DAY: u64 = 24 * 60 * 60;
const DUE_OFFSET: u64 = 30 * DAY;
const T0: u64 = 1_700_000_000;
/// Start well past every admin rate-limit cooldown so admin entrypoints
/// (fee rate etc.) are callable immediately.
const SEQ0: u32 = 10_000;
/// Contract treasury reserve. Needed because an upheld dispute on a fully
/// funded invoice refunds LPs although the freelancer already received the
/// payout (see `[NE-2]` in the invariants doc).
const RESERVE: i128 = 10 * AMOUNT;

// ----------------------------------------------------------------
// Harness
// ----------------------------------------------------------------

struct Ctx {
    env: Env,
    client: InvoiceLiquidityContractClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lps: [Address; 3],
    outsider: Address,
    token: Address,
    tc: TokenClient<'static>,
}

#[derive(Clone, Debug, PartialEq)]
struct Snap {
    invoice: Invoice,
    funders: StdVec<(Address, i128)>,
    balances: StdVec<i128>,
    payer_score: u32,
}

fn quiet_env() -> Env {
    // Property tests create many Envs; do not write a snapshot per Env.
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn setup_with_reserve(reserve: i128) -> Ctx {
    let env = quiet_env();
    env.mock_all_auths();

    let mut li = env.ledger().get();
    li.timestamp = T0;
    li.sequence_number = SEQ0;
    env.ledger().set(li);

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lps = [
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    ];
    let outsider = Address::generate(&env);

    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let sac = StellarAssetClient::new(&env, &token);
    let tc = TokenClient::new(&env, &token);
    let xlm = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();

    let contract_id = env.register(InvoiceLiquidityContract, ());
    let client = InvoiceLiquidityContractClient::new(&env, &contract_id);
    client.initialize(&admin, &token, &Address::generate(&env), &xlm);

    for lp in lps.iter() {
        sac.mint(lp, &(10 * AMOUNT));
    }
    sac.mint(&payer, &(10 * AMOUNT));
    if reserve > 0 {
        sac.mint(&contract_id, &reserve);
    }

    Ctx {
        env,
        client,
        admin,
        freelancer,
        payer,
        lps,
        outsider,
        token,
        tc,
    }
}

fn setup() -> Ctx {
    setup_with_reserve(RESERVE)
}

fn hash(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

fn discount(amount: i128) -> i128 {
    amount * RATE as i128 / 10_000
}

impl Ctx {
    fn due(&self) -> u64 {
        T0 + DUE_OFFSET
    }

    fn submit(&self) -> u64 {
        self.client.submit_invoice(
            &self.freelancer,
            &self.payer,
            &AMOUNT,
            &self.due(),
            &RATE,
            &self.token,
            &ReferralCode::None,
        )
    }

    fn funded(&self) -> u64 {
        let id = self.submit();
        self.client.fund_invoice(&self.lps[0], &id, &AMOUNT, &false);
        id
    }

    fn defaulted(&self) -> u64 {
        let id = self.funded();
        self.set_time(self.due());
        self.client.claim_default(&self.lps[0], &id);
        id
    }

    fn inv(&self, id: u64) -> Invoice {
        self.client.get_invoice(&id)
    }

    fn funders(&self, id: u64) -> StdVec<(Address, i128)> {
        let env = &self.env;
        let v = env.as_contract(&self.client.address, || get_invoice_funders(env, id));
        v.iter().collect()
    }

    fn actors(&self) -> StdVec<Address> {
        let mut v = std::vec![
            self.admin.clone(),
            self.freelancer.clone(),
            self.payer.clone(),
            self.outsider.clone(),
            self.client.address.clone(),
        ];
        v.extend(self.lps.iter().cloned());
        v
    }

    fn balances(&self) -> StdVec<i128> {
        self.actors().iter().map(|a| self.tc.balance(a)).collect()
    }

    fn total(&self) -> i128 {
        self.balances().iter().sum()
    }

    fn snap(&self, id: u64) -> Snap {
        let invoice = self.inv(id);
        Snap {
            payer_score: self.client.payer_score(&invoice.payer),
            invoice,
            funders: self.funders(id),
            balances: self.balances(),
        }
    }

    fn set_time(&self, ts: u64) {
        let mut li = self.env.ledger().get();
        li.timestamp = ts;
        self.env.ledger().set(li);
    }

    fn advance(&self, secs: u64, ledgers: u32) {
        let mut li = self.env.ledger().get();
        li.timestamp += secs;
        li.sequence_number += ledgers;
        self.env.ledger().set(li);
    }

    /// Replace blanket auth mocking with a single authorization entry signed
    /// by `signer` for `fn_name(args)` on this contract. Any `require_auth`
    /// for a different address now fails, exactly as on-chain.
    fn only_signer(&self, signer: &Address, fn_name: &str, args: soroban_sdk::Vec<Val>) {
        self.env.mock_auths(&[MockAuth {
            address: signer,
            invoke: &MockAuthInvoke {
                contract: &self.client.address,
                fn_name,
                args,
                sub_invokes: &[],
            },
        }]);
    }

    /// Enforce auth with no authorization entries at all.
    fn no_auth(&self) {
        self.env.set_auths(&[]);
    }

    fn restore_auth(&self) {
        self.env.mock_all_auths();
    }

    /// Addresses whose authorization was consumed by the last invocation.
    fn last_signers(&self) -> StdVec<Address> {
        self.env.auths().into_iter().map(|(a, _)| a).collect()
    }
}

/// `[ACC-1]..[ACC-5]`, `[ST-*]`: accounting / status consistency that must
/// hold in every reachable state.
fn assert_accounting(inv: &Invoice, funders: &[(Address, i128)]) {
    assert!(inv.amount > 0);
    assert!(
        (0..=inv.amount).contains(&inv.amount_funded),
        "[ACC-1] amount_funded {} outside [0, {}]",
        inv.amount_funded,
        inv.amount
    );
    assert!(
        (0..=inv.amount).contains(&inv.amount_paid),
        "[ACC-2] amount_paid {} outside [0, {}]",
        inv.amount_paid,
        inv.amount
    );
    let sum: i128 = funders.iter().map(|(_, a)| *a).sum();
    assert_eq!(
        sum, inv.amount_funded,
        "[ACC-3] per-LP contributions must sum to amount_funded"
    );
    for (_, a) in funders {
        assert!(*a > 0, "[ACC-4] every recorded contribution is positive");
    }
    let full = inv.amount_funded == inv.amount;
    match inv.status {
        InvoiceStatus::Pending => {
            assert_eq!(inv.amount_funded, 0, "[ST-1] Pending has no funding");
            assert_eq!(inv.amount_paid, 0);
            assert!(inv.funder.is_none());
        }
        InvoiceStatus::PartiallyFunded => {
            assert!(
                inv.amount_funded > 0 && !full,
                "[ACC-5] PartiallyFunded requires 0 < amount_funded < amount"
            );
            assert_eq!(inv.amount_paid, 0);
            assert!(inv.funder.is_none());
        }
        InvoiceStatus::Funded => {
            assert!(full, "[ACC-5] Funded requires amount_funded == amount");
            assert!(inv.amount_paid < inv.amount);
            assert!(inv.funder.is_some());
        }
        InvoiceStatus::Paid => {
            assert!(full);
            assert_eq!(inv.amount_paid, inv.amount, "[ST-2] Paid iff fully paid");
            assert!(inv.funder.is_some());
        }
        InvoiceStatus::Defaulted | InvoiceStatus::Appealed => {
            assert!(full, "[ST-3] only a fully funded invoice can default");
            assert!(inv.amount_paid < inv.amount);
            assert!(inv.funder.is_some());
        }
        InvoiceStatus::Disputed => assert!(inv.amount_paid < inv.amount),
        InvoiceStatus::Expired => {
            assert_eq!(inv.amount_funded, 0, "[ST-4] only Pending can expire");
            assert_eq!(inv.amount_paid, 0);
        }
        InvoiceStatus::Cancelled => assert!(inv.amount_paid < inv.amount),
    }
}

fn check(ctx: &Ctx, id: u64) {
    assert_accounting(&ctx.inv(id), &ctx.funders(id));
}

// ================================================================
// 1. Accounting bounds — [ACC-1], [ACC-2]
// ================================================================

#[test]
fn acc_overfunding_rejected_at_every_tranche() {
    let ctx = setup();
    let id = ctx.submit();

    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &(AMOUNT * 6 / 10), &false);
    check(&ctx, id);

    let before = ctx.snap(id);
    let res = ctx
        .client
        .try_fund_invoice(&ctx.lps[1], &id, &(AMOUNT / 2), &false);
    assert_eq!(res, Err(Ok(ContractError::OverfundingRejected)));
    assert_eq!(
        ctx.snap(id),
        before,
        "[RB-1] rejected funding changes nothing"
    );

    // Remaining exactly closes the obligation.
    ctx.client
        .fund_invoice(&ctx.lps[1], &id, &(AMOUNT * 4 / 10), &false);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Funded);
    assert_eq!(ctx.inv(id).amount_funded, AMOUNT);
    check(&ctx, id);

    // Nothing more can be funded, not even 1 stroop.
    let before = ctx.snap(id);
    let res = ctx.client.try_fund_invoice(&ctx.lps[2], &id, &1, &false);
    assert_eq!(res, Err(Ok(ContractError::AlreadyFunded)));
    assert_eq!(ctx.snap(id), before);
}

#[test]
fn acc_overflowing_fund_amount_rejected() {
    let ctx = setup();
    let id = ctx.submit();
    ctx.client.fund_invoice(&ctx.lps[0], &id, &1_000, &false);
    let before = ctx.snap(id);
    let res = ctx
        .client
        .try_fund_invoice(&ctx.lps[1], &id, &i128::MAX, &false);
    assert_eq!(res, Err(Ok(ContractError::ArithmeticOverflow)));
    assert_eq!(ctx.snap(id), before);
}

/// Regression for a bug found while writing these tests: `fund_invoice`
/// accepted `fund_amount == 0`, moving a Pending invoice to
/// PartiallyFunded with `amount_funded == 0` (breaking [ACC-5]) and adding a
/// zero-amount funder — which also blocked `expire_invoice`,
/// `update_invoice` and `transfer_invoice` for free.
#[test]
fn acc_zero_and_negative_fund_amount_rejected() {
    let ctx = setup();
    let id = ctx.submit();
    let before = ctx.snap(id);
    for bad in [0_i128, -1, -AMOUNT] {
        let res = ctx.client.try_fund_invoice(&ctx.lps[0], &id, &bad, &false);
        assert_eq!(res, Err(Ok(ContractError::InvalidAmount)), "amount {bad}");
        assert_eq!(ctx.snap(id), before);
    }
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Pending);
}

#[test]
fn acc_overpayment_rejected_and_paid_never_exceeds_amount() {
    let ctx = setup();
    let id = ctx.funded();

    ctx.client.mark_paid(&id, &(AMOUNT / 4));
    ctx.client.mark_paid(&id, &(AMOUNT / 4));
    check(&ctx, id);
    assert_eq!(ctx.inv(id).amount_paid, AMOUNT / 2);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Funded);

    let before = ctx.snap(id);
    let res = ctx.client.try_mark_paid(&id, &(AMOUNT / 2 + 1));
    assert_eq!(res, Err(Ok(ContractError::OverpaymentRejected)));
    assert_eq!(ctx.snap(id), before);

    for bad in [0_i128, -5] {
        assert_eq!(
            ctx.client.try_mark_paid(&id, &bad),
            Err(Ok(ContractError::InvalidAmount))
        );
    }
    assert_eq!(ctx.snap(id), before);

    ctx.client.mark_paid(&id, &(AMOUNT / 2));
    let inv = ctx.inv(id);
    assert_eq!(inv.status, InvoiceStatus::Paid);
    assert_eq!(inv.amount_paid, AMOUNT);
    check(&ctx, id);
}

#[test]
fn acc_payment_requires_full_funding() {
    let ctx = setup();
    let id = ctx.submit();
    assert_eq!(
        ctx.client.try_mark_paid(&id, &1),
        Err(Ok(ContractError::NotFunded))
    );
    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &(AMOUNT - 1), &false);
    let before = ctx.snap(id);
    assert_eq!(
        ctx.client.try_mark_paid(&id, &1),
        Err(Ok(ContractError::NotFunded))
    );
    assert_eq!(ctx.snap(id), before);
}

// ================================================================
// 2. Cumulative accounting — [ACC-3]..[ACC-5]
// ================================================================

#[test]
fn acc_multi_lp_partial_funding_is_cumulative() {
    let ctx = setup();
    let id = ctx.submit();
    let tranches: [(usize, i128); 4] = [
        (0, 123_456_789),
        (1, 300_000_001),
        (0, 76_543_211), // same LP funds twice -> one aggregated entry
        (2, 399_999_999),
    ];
    let mut running = 0;
    for (lp, amt) in tranches {
        let lp_before = ctx.tc.balance(&ctx.lps[lp]);
        ctx.client.fund_invoice(&ctx.lps[lp], &id, &amt, &false);
        running += amt;
        let inv = ctx.inv(id);
        assert_eq!(inv.amount_funded, running);
        assert_eq!(inv.status, InvoiceStatus::PartiallyFunded);
        assert_eq!(
            lp_before - ctx.tc.balance(&ctx.lps[lp]),
            amt - discount(amt),
            "LP pays the discounted cost of its own tranche only"
        );
        check(&ctx, id);
    }
    let funders = ctx.funders(id);
    assert_eq!(funders.len(), 3, "repeat funding aggregates per LP");
    assert_eq!(funders[0], (ctx.lps[0].clone(), 200_000_000));

    let freelancer_before = ctx.tc.balance(&ctx.freelancer);
    let last = AMOUNT - running;
    ctx.client.fund_invoice(&ctx.lps[1], &id, &last, &false);
    let inv = ctx.inv(id);
    assert_eq!(inv.status, InvoiceStatus::Funded);
    assert_eq!(inv.funder, Some(ctx.lps[1].clone()));
    assert_eq!(
        ctx.tc.balance(&ctx.freelancer) - freelancer_before,
        AMOUNT - discount(AMOUNT),
        "freelancer is paid exactly once, on the transition to Funded"
    );
    check(&ctx, id);
}

/// Dispute rejection (admin or timeout) restores the status implied by the
/// cumulative funding level — never a status inconsistent with it.
#[test]
fn acc_dispute_rejection_restores_status_from_cumulative_funding() {
    for (funding, expected) in [
        (0_i128, InvoiceStatus::Pending),
        (AMOUNT / 3, InvoiceStatus::PartiallyFunded),
        (AMOUNT, InvoiceStatus::Funded),
    ] {
        for auto in [false, true] {
            let ctx = setup();
            let id = ctx.submit();
            if funding > 0 {
                ctx.client.fund_invoice(&ctx.lps[0], &id, &funding, &false);
            }
            let before = ctx.snap(id);
            ctx.client.dispute_invoice(&id, &hash(&ctx.env, 1));
            check(&ctx, id);
            if auto {
                ctx.advance(0, 10_000);
                ctx.client.auto_resolve_dispute(&id);
            } else {
                ctx.client.resolve_dispute(&id, &hash(&ctx.env, 2), &2);
            }
            let after = ctx.snap(id);
            assert_eq!(after.invoice.status, expected);
            assert_eq!(after.invoice.amount_funded, funding);
            assert_eq!(after.funders, before.funders);
            assert_eq!(after.balances, before.balances, "rejection moves no funds");
            check(&ctx, id);
        }
    }
}

#[test]
fn acc_auto_resolve_requires_timeout() {
    let ctx = setup();
    let id = ctx.funded();
    ctx.client.dispute_invoice(&id, &hash(&ctx.env, 1));
    ctx.advance(0, 9_999);
    let before = ctx.snap(id);
    assert!(ctx.client.try_auto_resolve_dispute(&id).is_err());
    assert_eq!(ctx.snap(id), before);
}

#[test]
fn acc_cancel_refunds_each_partial_funder_exactly() {
    let ctx = setup();
    let id = ctx.submit();
    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &(AMOUNT / 5), &false);
    ctx.client
        .fund_invoice(&ctx.lps[1], &id, &(AMOUNT / 7), &false);
    let total_before = ctx.total();
    let lp0 = ctx.tc.balance(&ctx.lps[0]);
    let lp1 = ctx.tc.balance(&ctx.lps[1]);
    ctx.client.cancel_invoice(&id);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Cancelled);
    assert_eq!(
        ctx.tc.balance(&ctx.lps[0]) - lp0,
        AMOUNT / 5 - discount(AMOUNT / 5)
    );
    assert_eq!(
        ctx.tc.balance(&ctx.lps[1]) - lp1,
        AMOUNT / 7 - discount(AMOUNT / 7)
    );
    assert_eq!(ctx.total(), total_before, "refunds conserve tokens");
    check(&ctx, id);
}

// ================================================================
// 3. Authorization — [AUTH-1]..[AUTH-8]
//
// Pattern: (a) a *specific wrong* signer authorizes the exact call and it
// still fails; (b) no authorization at all fails; state is unchanged in
// both cases; (c) positive control under mock_all_auths, asserting via
// env.auths() that exactly the expected role signed.
// ================================================================

fn assert_requires<F>(
    ctx: &Ctx,
    id: u64,
    expected: &Address,
    wrong: &[&Address],
    fn_name: &str,
    args: soroban_sdk::Vec<Val>,
    call: F,
) where
    F: Fn(&Ctx) -> bool,
{
    let before = ctx.snap(id);
    for w in wrong {
        ctx.only_signer(w, fn_name, args.clone());
        assert!(
            !call(ctx),
            "{fn_name}: must fail when signed only by a non-authorized party"
        );
        assert_eq!(
            ctx.snap(id),
            before,
            "{fn_name}: failed auth must not change state"
        );
    }
    ctx.no_auth();
    assert!(!call(ctx), "{fn_name}: must fail with no authorization");
    assert_eq!(ctx.snap(id), before);

    // Positive control: the expected role's signature alone is sufficient
    // for the contract-level check (sub-invocations such as token transfers
    // are covered by mock_all_auths here).
    ctx.restore_auth();
    assert!(call(ctx), "{fn_name}: expected role must be able to call");
    let signers = ctx.last_signers();
    assert!(
        signers.iter().all(|s| s == expected) && !signers.is_empty(),
        "{fn_name}: exactly the expected role must authorize, got {signers:?}"
    );
}

#[test]
fn auth_cancel_requires_freelancer() {
    let ctx = setup();
    let id = ctx.submit();
    let args = (id,).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.freelancer.clone(),
        &[&ctx.payer, &ctx.admin, &ctx.lps[0]],
        "cancel_invoice",
        args,
        |c| c.client.try_cancel_invoice(&id).is_ok(),
    );
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Cancelled);
}

#[test]
fn auth_update_and_transfer_invoice_require_freelancer() {
    let ctx = setup();
    let id = ctx.submit();
    let new_due = ctx.due() + DAY;
    let args = (ctx.freelancer.clone(), id, AMOUNT / 2, new_due, RATE).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.freelancer.clone(),
        &[&ctx.payer, &ctx.admin],
        "update_invoice",
        args,
        |c| {
            c.client
                .try_update_invoice(&c.freelancer, &id, &(AMOUNT / 2), &new_due, &RATE)
                .is_ok()
        },
    );

    // A different address signing for *itself* is still not the freelancer.
    let before = ctx.snap(id);
    let res = ctx
        .client
        .try_update_invoice(&ctx.outsider, &id, &AMOUNT, &new_due, &RATE);
    assert_eq!(res, Err(Ok(ContractError::Unauthorized)));
    assert_eq!(ctx.snap(id), before);

    let new_owner = Address::generate(&ctx.env);
    let args = (id, new_owner.clone()).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.freelancer.clone(),
        &[&ctx.payer, &new_owner],
        "transfer_invoice",
        args,
        |c| c.client.try_transfer_invoice(&id, &new_owner).is_ok(),
    );
    assert_eq!(ctx.inv(id).freelancer, new_owner);
}

#[test]
fn auth_fund_requires_the_funding_lp() {
    let ctx = setup();
    let id = ctx.submit();
    let lp = ctx.lps[0].clone();
    // Another LP (or the freelancer) cannot spend lps[0]'s funds.
    let before = ctx.snap(id);
    for w in [&ctx.lps[1], &ctx.freelancer] {
        ctx.only_signer(
            w,
            "fund_invoice",
            (lp.clone(), id, AMOUNT, false).into_val(&ctx.env),
        );
        assert!(ctx
            .client
            .try_fund_invoice(&lp, &id, &AMOUNT, &false)
            .is_err());
        assert_eq!(ctx.snap(id), before);
    }
    ctx.no_auth();
    assert!(ctx
        .client
        .try_fund_invoice(&lp, &id, &AMOUNT, &false)
        .is_err());
    assert_eq!(ctx.snap(id), before);

    ctx.restore_auth();
    ctx.client.fund_invoice(&lp, &id, &AMOUNT, &false);
    let signers = ctx.last_signers();
    assert_eq!(signers, std::vec![lp]);
}

#[test]
fn auth_mark_paid_requires_payer() {
    let ctx = setup();
    let id = ctx.funded();
    let args = (id, AMOUNT).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.payer.clone(),
        &[&ctx.freelancer, &ctx.lps[0], &ctx.admin],
        "mark_paid",
        args,
        |c| c.client.try_mark_paid(&id, &AMOUNT).is_ok(),
    );
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Paid);
}

#[test]
fn auth_dispute_and_appeal_require_payer() {
    let ctx = setup();
    let id = ctx.funded();
    let h = hash(&ctx.env, 9);
    let args = (id, h.clone()).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.payer.clone(),
        &[&ctx.freelancer, &ctx.lps[0], &ctx.admin],
        "dispute_invoice",
        args,
        |c| c.client.try_dispute_invoice(&id, &h).is_ok(),
    );
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Disputed);

    let ctx = setup();
    let id = ctx.defaulted();
    let h = hash(&ctx.env, 9); // host objects are per-Env
    let args = (id, h.clone()).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.payer.clone(),
        &[&ctx.freelancer, &ctx.lps[0], &ctx.admin],
        "appeal_default",
        args,
        |c| c.client.try_appeal_default(&id, &h).is_ok(),
    );
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Appealed);
}

#[test]
fn auth_dispute_and_appeal_resolution_require_admin() {
    let ctx = setup();
    let id = ctx.funded();
    ctx.client.dispute_invoice(&id, &hash(&ctx.env, 1));
    let h = hash(&ctx.env, 2);
    let args = (id, h.clone(), 1_u32).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.admin.clone(),
        &[&ctx.payer, &ctx.freelancer, &ctx.lps[0]],
        "resolve_dispute",
        args,
        |c| c.client.try_resolve_dispute(&id, &h, &1).is_ok(),
    );
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Cancelled);

    let ctx = setup();
    let id = ctx.defaulted();
    ctx.client.appeal_default(&id, &hash(&ctx.env, 3));
    let args = (id, true).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.admin.clone(),
        &[&ctx.payer, &ctx.freelancer, &ctx.lps[0]],
        "resolve_appeal",
        args,
        |c| c.client.try_resolve_appeal(&id, &true).is_ok(),
    );
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Defaulted);
}

#[test]
fn auth_claim_default_requires_a_recorded_funder() {
    let ctx = setup();
    let id = ctx.funded();
    ctx.set_time(ctx.due());
    let lp = ctx.lps[0].clone();
    let args = (lp.clone(), id).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &lp,
        &[&ctx.lps[1], &ctx.freelancer, &ctx.payer, &ctx.admin],
        "claim_default",
        args,
        |c| c.client.try_claim_default(&lp, &id).is_ok(),
    );
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Defaulted);

    // An LP that never funded, signing for itself, is rejected.
    let ctx = setup();
    let id = ctx.funded();
    ctx.set_time(ctx.due());
    let before = ctx.snap(id);
    assert_eq!(
        ctx.client.try_claim_default(&ctx.lps[2], &id),
        Err(Ok(ContractError::Unauthorized))
    );
    assert_eq!(ctx.snap(id), before);
}

#[test]
fn auth_lp_position_and_yield_require_current_funder() {
    let ctx = setup();
    let id = ctx.funded();
    let new_lp = ctx.lps[1].clone();
    let args = (id, new_lp.clone()).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &ctx.lps[0].clone(),
        &[&ctx.lps[1], &ctx.freelancer, &ctx.admin],
        "transfer_lp_position",
        args,
        |c| c.client.try_transfer_lp_position(&id, &new_lp).is_ok(),
    );
    assert_eq!(ctx.inv(id).funder, Some(new_lp.clone()));
    check(&ctx, id);

    ctx.client.mark_paid(&id, &AMOUNT);
    let args = (id,).into_val(&ctx.env);
    assert_requires(
        &ctx,
        id,
        &new_lp,
        &[&ctx.lps[0], &ctx.payer, &ctx.admin],
        "claim_yield",
        args,
        |c| c.client.try_claim_yield(&id).is_ok(),
    );
}

/// Permissionless entrypoints (`expire_invoice`, `auto_resolve_dispute`)
/// need no signature, but are gated purely on state/time, so they cannot be
/// used to bypass a role: they can only perform the one transition the
/// table allows.
#[test]
fn auth_permissionless_transitions_are_state_gated() {
    let ctx = setup();
    let id = ctx.submit();
    ctx.no_auth();
    assert_eq!(
        ctx.client.try_expire_invoice(&id),
        Err(Ok(ContractError::NotYetDefaulted))
    );
    ctx.set_time(ctx.due() + 1);
    ctx.client.expire_invoice(&id);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Expired);
    assert!(ctx.last_signers().is_empty());
    ctx.restore_auth();

    // A partially funded invoice can never be expired by a third party.
    let ctx = setup();
    let id = ctx.submit();
    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &1_000_000, &false);
    ctx.set_time(ctx.due() + 1);
    ctx.no_auth();
    assert_eq!(
        ctx.client.try_expire_invoice(&id),
        Err(Ok(ContractError::AlreadyFunded))
    );
}

/// `initialize` performs no authorization, so the re-initialization guard is
/// the only thing protecting the admin role. Regression for a bug found
/// while writing these tests: the guard checked a storage key
/// (`InvoiceCount`) that is never written, so anyone could re-run
/// `initialize` and install themselves as admin (and then e.g. resolve
/// disputes in their favour).
#[test]
fn auth_reinitialize_cannot_replace_admin() {
    let ctx = setup();
    let id = ctx.funded();
    ctx.client.dispute_invoice(&id, &hash(&ctx.env, 1));
    let attacker = Address::generate(&ctx.env);
    let res = ctx.client.try_initialize(
        &attacker,
        &ctx.token,
        &Address::generate(&ctx.env),
        &Address::generate(&ctx.env),
    );
    assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));

    // The original admin still controls dispute resolution.
    ctx.client.resolve_dispute(&id, &hash(&ctx.env, 2), &2);
    assert_eq!(ctx.last_signers(), std::vec![ctx.admin.clone()]);
}

// ================================================================
// 4. Settlement once / legitimate yield — [SET-1]..[SET-4]
// ================================================================

#[test]
fn set_settlement_cannot_be_applied_twice() {
    let ctx = setup();
    let id = ctx.funded();
    let lp_before = ctx.tc.balance(&ctx.lps[0]);
    ctx.client.mark_paid(&id, &AMOUNT);
    let lp_gain = ctx.tc.balance(&ctx.lps[0]) - lp_before;
    assert_eq!(
        lp_gain, AMOUNT,
        "fee 0: single LP receives the full settlement"
    );

    let before = ctx.snap(id);
    assert_eq!(
        ctx.client.try_mark_paid(&id, &1),
        Err(Ok(ContractError::AlreadyPaid))
    );
    ctx.set_time(ctx.due() + 1);
    assert_eq!(
        ctx.client.try_claim_default(&ctx.lps[0], &id),
        Err(Ok(ContractError::AlreadyPaid))
    );
    assert_eq!(
        ctx.client.try_dispute_invoice(&id, &hash(&ctx.env, 1)),
        Err(Ok(ContractError::AlreadyPaid))
    );
    assert_eq!(
        ctx.client.try_cancel_invoice(&id),
        Err(Ok(ContractError::AlreadyPaid))
    );
    assert_eq!(
        ctx.client.try_fund_invoice(&ctx.lps[1], &id, &1, &false),
        Err(Ok(ContractError::AlreadyPaid))
    );
    assert_eq!(ctx.snap(id), before, "terminal Paid is immutable");
}

#[test]
fn set_settlement_with_fee_never_distributes_more_than_received() {
    let ctx = setup();
    ctx.client.update_fee_rate(&250);
    let id = ctx.submit();
    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &333_333_333, &false);
    ctx.client
        .fund_invoice(&ctx.lps[1], &id, &333_333_333, &false);
    ctx.client
        .fund_invoice(&ctx.lps[2], &id, &333_333_334, &false);

    let contract_before = ctx.tc.balance(&ctx.client.address);
    let admin_before = ctx.tc.balance(&ctx.admin);
    let lps_before: StdVec<i128> = ctx.lps.iter().map(|l| ctx.tc.balance(l)).collect();
    ctx.client.mark_paid(&id, &AMOUNT);

    let fee = AMOUNT * 250 / 10_000;
    assert_eq!(ctx.tc.balance(&ctx.admin) - admin_before, fee);
    let distributed: i128 = ctx
        .lps
        .iter()
        .zip(lps_before.iter())
        .map(|(l, b)| ctx.tc.balance(l) - b)
        .sum();
    assert!(
        distributed <= AMOUNT - fee,
        "[SET-3] payouts bounded by settlement"
    );
    assert!(
        distributed >= AMOUNT - fee - 3,
        "only rounding dust is retained"
    );
    assert!(
        ctx.tc.balance(&ctx.client.address) >= contract_before,
        "settlement never draws on the contract's pre-existing balance"
    );
    check(&ctx, id);
}

#[test]
fn set_default_cannot_be_claimed_twice_or_after_appeal() {
    let ctx = setup();
    let id = ctx.funded();
    ctx.set_time(ctx.due());
    let lp_before = ctx.tc.balance(&ctx.lps[0]);
    ctx.client.claim_default(&ctx.lps[0], &id);
    assert_eq!(
        ctx.tc.balance(&ctx.lps[0]) - lp_before,
        AMOUNT - discount(AMOUNT),
        "default refunds the LP's principal cost exactly once"
    );
    let before = ctx.snap(id);
    assert_eq!(
        ctx.client.try_claim_default(&ctx.lps[0], &id),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
    assert_eq!(ctx.snap(id), before);

    ctx.client.appeal_default(&id, &hash(&ctx.env, 1));
    assert_eq!(
        ctx.client.try_appeal_default(&id, &hash(&ctx.env, 1)),
        Err(Ok(ContractError::AlreadyAppealed))
    );
    assert_eq!(
        ctx.client.try_claim_default(&ctx.lps[0], &id),
        Err(Ok(ContractError::InvoiceAppealed))
    );
    ctx.client.resolve_appeal(&id, &true);
    let after_resolve = ctx.snap(id);
    assert_eq!(
        after_resolve.balances, before.balances,
        "appeal moves no funds"
    );
    assert_eq!(
        ctx.client.try_claim_default(&ctx.lps[0], &id),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
    assert_eq!(
        ctx.client.try_appeal_default(&id, &hash(&ctx.env, 2)),
        Err(Ok(ContractError::AlreadyAppealed)),
        "an invoice can be appealed at most once"
    );
    assert_eq!(ctx.snap(id), after_resolve);
}

#[test]
fn set_default_requires_due_date_and_full_funding() {
    let ctx = setup();
    let id = ctx.funded();
    ctx.set_time(ctx.due() - 1);
    assert_eq!(
        ctx.client.try_claim_default(&ctx.lps[0], &id),
        Err(Ok(ContractError::NotYetDefaulted))
    );

    let ctx = setup();
    let id = ctx.submit();
    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &(AMOUNT / 2), &false);
    ctx.set_time(ctx.due() + 1);
    assert_eq!(
        ctx.client.try_claim_default(&ctx.lps[0], &id),
        Err(Ok(ContractError::NotFunded))
    );
}

/// [SET-4]: only an invoice that was fully funded *and* settled yields
/// anything; every other path yields 0 or an error.
#[test]
fn set_yield_only_from_legitimately_funded_and_settled_obligation() {
    let expected_yield = AMOUNT * RATE as i128 / 10_000;

    // Pending / PartiallyFunded: no recorded funder -> NothingToClaim.
    let ctx = setup();
    let id = ctx.submit();
    assert_eq!(
        ctx.client.try_claim_yield(&id),
        Err(Ok(ContractError::NothingToClaim))
    );
    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &(AMOUNT / 2), &false);
    assert_eq!(
        ctx.client.try_claim_yield(&id),
        Err(Ok(ContractError::NothingToClaim))
    );
    // Cancelled after partial funding: still nothing.
    ctx.client.cancel_invoice(&id);
    assert_eq!(
        ctx.client.try_claim_yield(&id),
        Err(Ok(ContractError::NothingToClaim))
    );

    // Funded but unsettled: zero.
    let ctx = setup();
    let id = ctx.funded();
    assert_eq!(ctx.client.claim_yield(&id), 0);
    ctx.client.mark_paid(&id, &(AMOUNT / 2));
    assert_eq!(
        ctx.client.claim_yield(&id),
        0,
        "partial settlement yields nothing"
    );
    ctx.client.mark_paid(&id, &(AMOUNT / 2));
    assert_eq!(ctx.client.claim_yield(&id), expected_yield);
    // claim_yield is a view: repeated calls move no funds.
    let before = ctx.snap(id);
    assert_eq!(ctx.client.claim_yield(&id), expected_yield);
    assert_eq!(ctx.snap(id), before);

    // Defaulted / Appealed / dispute-cancelled: error.
    let ctx = setup();
    let id = ctx.defaulted();
    assert_eq!(
        ctx.client.try_claim_yield(&id),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
    ctx.client.appeal_default(&id, &hash(&ctx.env, 1));
    assert_eq!(
        ctx.client.try_claim_yield(&id),
        Err(Ok(ContractError::InvoiceAppealed))
    );

    let ctx = setup();
    let id = ctx.funded();
    ctx.client.dispute_invoice(&id, &hash(&ctx.env, 1));
    assert_eq!(
        ctx.client.try_claim_yield(&id),
        Err(Ok(ContractError::InvoiceDisputed))
    );
    ctx.client.resolve_dispute(&id, &hash(&ctx.env, 2), &1);
    assert_eq!(
        ctx.client.try_claim_yield(&id),
        Err(Ok(ContractError::AlreadyCancelled))
    );
}

#[test]
fn set_upheld_dispute_refunds_partial_payments_exactly_once() {
    let ctx = setup();
    let id = ctx.funded();
    ctx.client.mark_paid(&id, &(AMOUNT / 4));
    let payer_before = ctx.tc.balance(&ctx.payer);
    let lp_before = ctx.tc.balance(&ctx.lps[0]);
    ctx.client.dispute_invoice(&id, &hash(&ctx.env, 1));
    ctx.client.resolve_dispute(&id, &hash(&ctx.env, 2), &1);
    assert_eq!(ctx.tc.balance(&ctx.payer) - payer_before, AMOUNT / 4);
    assert_eq!(
        ctx.tc.balance(&ctx.lps[0]) - lp_before,
        AMOUNT - discount(AMOUNT)
    );
    let before = ctx.snap(id);
    assert_eq!(
        ctx.client.try_resolve_dispute(&id, &hash(&ctx.env, 2), &1),
        Err(Ok(ContractError::NotDisputed))
    );
    assert_eq!(
        ctx.client.try_dispute_invoice(&id, &hash(&ctx.env, 3)),
        Err(Ok(ContractError::AlreadyDisputed))
    );
    assert_eq!(ctx.snap(id), before);
    check(&ctx, id);
}

// ================================================================
// 5. Failed-transaction rollback — [RB-1]..[RB-4]
// ================================================================

/// `fund_invoice` / `update_invoice` / `convert_invoice_token` write
/// `Expired` and then return `Err(InvoiceExpired)`. Soroban rolls back all
/// storage writes of a failed invocation, so that write never persists; the
/// only durable path to Expired is `expire_invoice` ([NE-4]).
#[test]
fn rb_lazy_expiry_on_error_path_is_rolled_back() {
    let ctx = setup();
    let id = ctx.submit();
    ctx.set_time(ctx.due() + 1);
    let before = ctx.snap(id);
    assert_eq!(
        ctx.client
            .try_fund_invoice(&ctx.lps[0], &id, &AMOUNT, &false),
        Err(Ok(ContractError::InvoiceExpired))
    );
    assert_eq!(
        ctx.client.try_update_invoice(
            &ctx.freelancer,
            &id,
            &AMOUNT,
            &(ctx.due() + 10 * DAY),
            &RATE
        ),
        Err(Ok(ContractError::InvoiceExpired))
    );
    assert_eq!(ctx.snap(id), before);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Pending);
    ctx.client.expire_invoice(&id);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Expired);
    check(&ctx, id);
}

/// Error paths that return after taking the reentrancy lock must not leave
/// the lock set (they rely on rollback), otherwise every later lifecycle
/// call would fail with `Reentrancy`.
#[test]
fn rb_failed_calls_do_not_leave_reentrancy_lock_set() {
    let ctx = setup();
    let id = ctx.submit();
    assert!(ctx
        .client
        .try_fund_invoice(&ctx.lps[0], &id, &(AMOUNT + 1), &false)
        .is_err());
    assert!(ctx.client.try_mark_paid(&id, &1).is_err());
    assert!(ctx.client.try_claim_default(&ctx.lps[0], &id).is_err());
    assert!(ctx
        .client
        .try_resolve_dispute(&id, &hash(&ctx.env, 1), &1)
        .is_err());
    ctx.client.fund_invoice(&ctx.lps[0], &id, &AMOUNT, &false);
    ctx.client.mark_paid(&id, &AMOUNT);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Paid);
}

/// An upheld dispute on a Funded invoice refunds the LP from the contract's
/// own balance. If that balance is insufficient, the whole resolution —
/// status change included — must revert ([RB-3], [NE-2]).
#[test]
fn rb_insolvent_dispute_refund_reverts_status_and_balances() {
    let ctx = setup_with_reserve(0);
    let id = ctx.funded();
    ctx.client.dispute_invoice(&id, &hash(&ctx.env, 1));
    let before = ctx.snap(id);
    assert!(ctx
        .client
        .try_resolve_dispute(&id, &hash(&ctx.env, 2), &1)
        .is_err());
    assert_eq!(ctx.snap(id), before);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Disputed);
    // The rejection path moves no funds and still works.
    ctx.client.resolve_dispute(&id, &hash(&ctx.env, 2), &2);
    assert_eq!(ctx.inv(id).status, InvoiceStatus::Funded);
}

#[test]
fn rb_paused_contract_blocks_lifecycle_without_side_effects() {
    let ctx = setup();
    let id = ctx.submit();
    ctx.client
        .fund_invoice(&ctx.lps[0], &id, &(AMOUNT / 2), &false);
    ctx.client.pause();
    let before = ctx.snap(id);
    assert_eq!(
        ctx.client
            .try_fund_invoice(&ctx.lps[1], &id, &(AMOUNT / 2), &false),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        ctx.client.try_cancel_invoice(&id),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        ctx.client.try_dispute_invoice(&id, &hash(&ctx.env, 1)),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(ctx.snap(id), before);
}

// ================================================================
// 6. Property tests — [PROP-1]
// ================================================================

#[derive(Clone, Copy, Debug)]
enum Op {
    Fund(usize, i128),
    FundRest(usize),
    Pay(i128),
    PayRest,
    Cancel,
    Expire,
    AdvanceDays(u64),
    ClaimDefault(usize),
    Appeal,
    ResolveAppeal(bool),
    Dispute,
    ResolveDispute(u32),
    AutoResolve,
    ClaimYield,
    TransferLp(usize),
}

/// Amounts biased towards the boundaries: non-positive, tiny, and
/// anything up to 120% of the obligation.
fn amount_strategy() -> impl Strategy<Value = i128> {
    prop_oneof![
        1 => -2i128..=0,
        1 => 1i128..=10,
        6 => 1i128..=AMOUNT * 6 / 5,
    ]
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0usize..3, amount_strategy()).prop_map(|(l, a)| Op::Fund(l, a)),
        2 => (0usize..3).prop_map(Op::FundRest),
        2 => amount_strategy().prop_map(Op::Pay),
        1 => Just(Op::PayRest),
        1 => Just(Op::Cancel),
        1 => Just(Op::Expire),
        2 => (0u64..=40).prop_map(Op::AdvanceDays),
        1 => (0usize..3).prop_map(Op::ClaimDefault),
        1 => Just(Op::Appeal),
        1 => any::<bool>().prop_map(Op::ResolveAppeal),
        1 => Just(Op::Dispute),
        1 => (0u32..=3).prop_map(Op::ResolveDispute),
        1 => Just(Op::AutoResolve),
        1 => Just(Op::ClaimYield),
        1 => (0usize..3).prop_map(Op::TransferLp),
    ]
}

fn is_terminal(s: &InvoiceStatus) -> bool {
    matches!(
        s,
        InvoiceStatus::Paid | InvoiceStatus::Expired | InvoiceStatus::Cancelled
    )
}

fn funding_status(inv: &Invoice) -> InvoiceStatus {
    if inv.amount_funded == inv.amount {
        InvoiceStatus::Funded
    } else if inv.amount_funded > 0 {
        InvoiceStatus::PartiallyFunded
    } else {
        InvoiceStatus::Pending
    }
}

/// The documented transition table, keyed by entrypoint. Returns whether a
/// *successful* call of `op` may move `from` to `to`.
fn allowed(op: &Op, from: &Invoice, to: &Invoice) -> bool {
    use InvoiceStatus::*;
    let (f, t) = (&from.status, &to.status);
    match op {
        Op::Fund(..) | Op::FundRest(_) => {
            matches!(f, Pending | PartiallyFunded) && matches!(t, PartiallyFunded | Funded)
        }
        Op::Pay(_) | Op::PayRest => *f == Funded && matches!(t, Funded | Paid),
        Op::Cancel => matches!(f, Pending | PartiallyFunded) && *t == Cancelled,
        Op::Expire => *f == Pending && *t == Expired,
        Op::ClaimDefault(_) => *f == Funded && *t == Defaulted,
        Op::Appeal => *f == Defaulted && *t == Appealed,
        Op::ResolveAppeal(_) => *f == Appealed && *t == Defaulted,
        Op::Dispute => matches!(f, Pending | PartiallyFunded | Funded) && *t == Disputed,
        Op::ResolveDispute(1) => *f == Disputed && *t == Cancelled,
        Op::ResolveDispute(2) | Op::AutoResolve => *f == Disputed && *t == funding_status(from),
        Op::ResolveDispute(_) => false,
        Op::ClaimYield | Op::AdvanceDays(_) => f == t,
        Op::TransferLp(_) => *f == Funded && *t == Funded,
    }
}

fn run_sequence(ops: StdVec<Op>) {
    let ctx = setup();
    let id = ctx.submit();
    let supply = ctx.total();
    let mut paid_transitions = 0;
    let mut default_transitions = 0;
    let mut payer_outflow: i128 = 0;
    let payer_start = ctx.tc.balance(&ctx.payer);

    for op in ops {
        let pre = ctx.snap(id);
        let inv = &pre.invoice;
        let ok = match op {
            Op::Fund(l, a) => ctx
                .client
                .try_fund_invoice(&ctx.lps[l], &id, &a, &false)
                .is_ok(),
            Op::FundRest(l) => {
                let rest = inv.amount - inv.amount_funded;
                ctx.client
                    .try_fund_invoice(&ctx.lps[l], &id, &rest, &false)
                    .is_ok()
            }
            Op::Pay(a) => ctx.client.try_mark_paid(&id, &a).is_ok(),
            Op::PayRest => {
                let rest = inv.amount - inv.amount_paid;
                ctx.client.try_mark_paid(&id, &rest).is_ok()
            }
            Op::Cancel => ctx.client.try_cancel_invoice(&id).is_ok(),
            Op::Expire => ctx.client.try_expire_invoice(&id).is_ok(),
            Op::AdvanceDays(d) => {
                ctx.advance(d * DAY, (d as u32) * 2_000);
                true
            }
            Op::ClaimDefault(l) => ctx.client.try_claim_default(&ctx.lps[l], &id).is_ok(),
            Op::Appeal => ctx
                .client
                .try_appeal_default(&id, &hash(&ctx.env, 4))
                .is_ok(),
            Op::ResolveAppeal(u) => ctx.client.try_resolve_appeal(&id, &u).is_ok(),
            Op::Dispute => ctx
                .client
                .try_dispute_invoice(&id, &hash(&ctx.env, 5))
                .is_ok(),
            Op::ResolveDispute(r) => ctx
                .client
                .try_resolve_dispute(&id, &hash(&ctx.env, 6), &r)
                .is_ok(),
            Op::AutoResolve => ctx.client.try_auto_resolve_dispute(&id).is_ok(),
            Op::ClaimYield => match ctx.client.try_claim_yield(&id) {
                Ok(Ok(y)) => {
                    // [SET-4] yield only from a settled obligation.
                    if y > 0 {
                        assert_eq!(inv.status, InvoiceStatus::Paid);
                        assert_eq!(y, inv.amount * RATE as i128 / 10_000);
                    }
                    true
                }
                _ => false,
            },
            Op::TransferLp(l) => ctx
                .client
                .try_transfer_lp_position(&id, &ctx.lps[l])
                .is_ok(),
        };
        let post = ctx.snap(id);

        if !ok {
            // [RB-1] a failed call is a no-op on invoice, LP ledger, tokens.
            assert_eq!(post, pre, "failed {op:?} must roll back completely");
            continue;
        }

        assert!(
            allowed(&op, &pre.invoice, &post.invoice),
            "{op:?}: illegal transition {:?} -> {:?}",
            pre.invoice.status,
            post.invoice.status
        );
        if is_terminal(&pre.invoice.status) {
            assert_eq!(
                post.invoice, pre.invoice,
                "[ST-5] terminal states are final"
            );
            assert_eq!(post.balances, pre.balances);
        }
        assert_accounting(&post.invoice, &post.funders);
        assert_eq!(ctx.total(), supply, "[ACC-6] tokens are conserved");

        // Per-entrypoint accounting effects.
        let df = post.invoice.amount_funded - pre.invoice.amount_funded;
        let dp = post.invoice.amount_paid - pre.invoice.amount_paid;
        match op {
            Op::Fund(_, a) => {
                assert!(a > 0);
                assert_eq!(df, a);
            }
            Op::FundRest(_) => assert_eq!(post.invoice.amount_funded, post.invoice.amount),
            Op::Pay(a) => {
                assert!(a > 0);
                assert_eq!(dp, a);
                payer_outflow += a;
            }
            Op::PayRest => {
                payer_outflow += dp;
                assert_eq!(post.invoice.amount_paid, post.invoice.amount);
            }
            _ => {
                assert_eq!(df, 0, "{op:?} must not change amount_funded");
                assert_eq!(dp, 0, "{op:?} must not change amount_paid");
            }
        }
        if post.invoice.status == InvoiceStatus::Paid && pre.invoice.status != InvoiceStatus::Paid {
            paid_transitions += 1;
        }
        if post.invoice.status == InvoiceStatus::Defaulted
            && pre.invoice.status == InvoiceStatus::Funded
        {
            default_transitions += 1;
        }
        // Upheld dispute refunds the payer's partial payments.
        if matches!(op, Op::ResolveDispute(1)) {
            payer_outflow -= pre.invoice.amount_paid;
        }
        assert_eq!(payer_start - ctx.tc.balance(&ctx.payer), payer_outflow);
    }
    assert!(
        paid_transitions <= 1,
        "[SET-1] settlement applied at most once"
    );
    assert!(
        default_transitions <= 1,
        "[SET-2] default applied at most once"
    );
    assert!(
        payer_outflow <= AMOUNT,
        "[ACC-2] payer never pays above the obligation"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn prop_lifecycle_and_accounting_invariants(
        ops in prop::collection::vec(op_strategy(), 1..40),
    ) {
        run_sequence(ops);
    }

    /// Funding-heavy sequences: many small tranches from several LPs, then
    /// settlement or default. Targets [ACC-1]/[ACC-3]/[ACC-5] at boundaries.
    #[test]
    fn prop_partial_funding_then_settle_or_default(
        tranches in prop::collection::vec((0usize..3, 1i128..=AMOUNT / 3), 1..12),
        settle in any::<bool>(),
        pay_chunks in 1u32..5,
    ) {
        let mut ops: StdVec<Op> = tranches.into_iter().map(|(l, a)| Op::Fund(l, a)).collect();
        ops.push(Op::FundRest(0));
        if settle {
            for _ in 1..pay_chunks {
                ops.push(Op::Pay(AMOUNT / pay_chunks as i128));
            }
            ops.push(Op::PayRest);
            ops.push(Op::PayRest);
        } else {
            ops.push(Op::AdvanceDays(31));
            ops.push(Op::ClaimDefault(0));
            ops.push(Op::ClaimDefault(1));
            ops.push(Op::ClaimDefault(2));
            ops.push(Op::Appeal);
            ops.push(Op::ResolveAppeal(true));
            ops.push(Op::ClaimDefault(0));
        }
        ops.push(Op::ClaimYield);
        run_sequence(ops);
    }
}
