//! Issue #1178 — a failed funded payout must roll back atomically.
//!
//! Every funded payout this contract makes is an ordinary SEP-41/SAC
//! `transfer` call to a token contract it does not control. This suite
//! deliberately makes that transfer **fail** (the token contract traps) on
//! each payout path and then asserts the invoice state, funder list, per-token
//! volume counter and every token balance are byte-for-byte unchanged.
//!
//! The mechanism is documented in `docs/failed-transfer-rollback.md`. In
//! short: Soroban invocation atomicity rolls back the whole top-level call,
//! and the payout entry points write their status changes *before* the
//! external transfer (Checks-Effects-Interactions), so those pre-transfer
//! writes come back with the failed transfer rather than surviving it.
//!
//! The failure is injected the same way the host-trap suite
//! (`tests_resource_limit_failure.rs`) detects a trap: by catching the panic
//! with `std::panic::catch_unwind`. The difference is the *source* of the
//! failure — here it is a token contract that panics on a chosen `transfer`
//! call, not a resource-limit enforcement.
#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger},
    token::Client as TokenClient,
    Address, BytesN, Env,
};

const AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
/// Pre-funded into the contract so the refund paths that draw on the
/// contract's own balance (default refund, upheld-dispute refund) pass
/// `ensure_spendable` and actually reach the token `transfer` under test.
/// Mirrors the funding other suites provision before defaulting an invoice.
const CONTRACT_BUFFER: i128 = AMOUNT * 100;

fn discount(amount: i128) -> i128 {
    amount * DISCOUNT_RATE as i128 / 10_000
}

// ----------------------------------------------------------------
// A token that can be armed to panic on a chosen transfer call
// ----------------------------------------------------------------

#[contracttype]
enum FailKey {
    Balance(Address),
    /// SAC-style trustline flag; everyone is authorized unless frozen.
    Authorized(Address),
    /// Number of `transfer` calls attempted since deployment.
    Transfers,
    /// Absolute ordinal (1-based) of the `transfer` call that must panic.
    FailAt,
}

fn ft_balance(env: &Env, id: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&FailKey::Balance(id.clone()))
        .unwrap_or(0)
}

/// Minimal SEP-41/SAC-shaped token with two test affordances: a one-shot
/// "fail this transfer" arm, and a settable `authorized` flag (so a recipient
/// can be frozen to force the escrow path). Panicking here stands in for any
/// token-side transfer failure.
#[contract]
pub struct FailingTransferToken;

#[contractimpl]
impl FailingTransferToken {
    pub fn decimals(_env: Env) -> u32 {
        7
    }

    pub fn authorized(env: Env, id: Address) -> bool {
        env.storage()
            .persistent()
            .get(&FailKey::Authorized(id))
            .unwrap_or(true)
    }

    pub fn set_authorized(env: Env, id: Address, flag: bool) {
        env.storage()
            .persistent()
            .set(&FailKey::Authorized(id), &flag);
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        ft_balance(&env, &id)
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let b = ft_balance(&env, &to);
        env.storage()
            .persistent()
            .set(&FailKey::Balance(to), &(b + amount));
    }

    /// Arm the `skip`-th transfer *after* this call (0 = the very next) to
    /// panic.
    pub fn arm_fail(env: Env, skip: u32) {
        let done: u64 = env
            .storage()
            .persistent()
            .get(&FailKey::Transfers)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&FailKey::FailAt, &(done + skip as u64 + 1));
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let n: u64 = env
            .storage()
            .persistent()
            .get(&FailKey::Transfers)
            .unwrap_or(0)
            + 1;
        env.storage().persistent().set(&FailKey::Transfers, &n);
        if let Some(fail_at) = env
            .storage()
            .persistent()
            .get::<_, u64>(&FailKey::FailAt)
        {
            if n == fail_at {
                panic!("failing-transfer-token: forced transfer failure");
            }
        }

        from.require_auth();
        let fb = ft_balance(&env, &from);
        assert!(fb >= amount, "failing-transfer-token: insufficient balance");
        env.storage()
            .persistent()
            .set(&FailKey::Balance(from), &(fb - amount));
        let tb = ft_balance(&env, &to);
        env.storage()
            .persistent()
            .set(&FailKey::Balance(to), &(tb + amount));
    }
}

// ----------------------------------------------------------------
// Fixture
// ----------------------------------------------------------------

struct T {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: Address,
    token_client: TokenClient<'static>,
    mock: FailingTransferTokenClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
}

fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();

    let token = env.register_contract(None, FailingTransferToken);
    let token_client = TokenClient::new(&env, &token);
    let mock = FailingTransferTokenClient::new(&env, &token);

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

    // The mock occupies the USDC bootstrap slot so every transfer for the
    // invoice under test routes through FailingTransferToken.
    let eurc = Address::generate(&env);
    let xlm = Address::generate(&env);
    contract.initialize(&admin, &token, &eurc, &xlm);

    mock.mint(&payer, &(AMOUNT * 10));
    mock.mint(&lp, &(AMOUNT * 10));
    mock.mint(&contract.address, &CONTRACT_BUFFER);

    let mut info = env.ledger().get();
    info.timestamp = 1_700_000_000;
    info.sequence_number += crate::constants::DEFAULT_RATE_LIMIT_LEDGERS as u32;
    env.ledger().set(info);

    T {
        env,
        contract,
        token,
        token_client,
        mock,
        admin,
        freelancer,
        payer,
        lp,
    }
}

fn submit(t: &T) -> u64 {
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token,
        &ReferralCode::None,
    )
}

fn advance_past_due(t: &T) {
    let mut info = t.env.ledger().get();
    info.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(info);
}

fn reason_hash(env: &Env) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xab;
    bytes[1] = 0xcd;
    bytes[31] = 0x01;
    BytesN::from_array(env, &bytes)
}

fn funders(t: &T, id: u64) -> Vec<(Address, i128)> {
    t.env
        .as_contract(&t.contract.address, || get_invoice_funders(&t.env, id))
}

fn volume(t: &T) -> i128 {
    t.env
        .as_contract(&t.contract.address, || get_token_volume(&t.env, &t.token))
}

/// Run `f` expecting it to trap (the armed token transfer panics), and assert
/// it did. Mirrors `tests_resource_limit_failure.rs`'s trap detection.
fn expect_trap<R>(f: impl FnOnce() -> R) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    assert!(
        result.is_err(),
        "expected the forced token-transfer failure to trap the invocation"
    );
}

// ================================================================
// 1. Freelancer advance at funding (transfer_out_strict)
// ================================================================

#[test]
fn funding_freelancer_payout_failure_rolls_back_entire_funding() {
    let t = setup();
    let id = submit(&t);

    let lp_before = t.token_client.balance(&t.lp);
    let contract_before = t.token_client.balance(&t.contract.address);
    let volume_before = volume(&t);

    // Transfer #1 = LP -> contract (inbound); #2 = contract -> freelancer
    // (the payout). Fail #2.
    t.mock.arm_fail(&1);

    expect_trap(|| t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false));

    assert_eq!(
        t.token_client.balance(&t.lp),
        lp_before,
        "LP's inbound funding transfer must roll back with the failed payout"
    );
    assert_eq!(
        t.token_client.balance(&t.contract.address),
        contract_before,
        "contract balance must be unchanged"
    );
    let inv = t.contract.get_invoice(&id);
    assert_eq!(inv.status, InvoiceStatus::Pending);
    assert_eq!(inv.amount_funded, 0);
    assert_eq!(funders(&t, id).len(), 0, "no funder entry must survive");
    assert_eq!(volume(&t), volume_before, "volume must not have advanced");
}

// ================================================================
// 2. Funder distribution at settlement (pay_or_escrow, default fee)
// ================================================================

#[test]
fn settlement_funder_distribution_failure_rolls_back_whole_settlement() {
    let t = setup();
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Funded);

    let payer_before = t.token_client.balance(&t.payer);
    let lp_before = t.token_client.balance(&t.lp);
    let contract_before = t.token_client.balance(&t.contract.address);

    // Within mark_paid: #1 = payer -> contract; #2 = contract -> LP. Fail #2.
    t.mock.arm_fail(&1);

    expect_trap(|| t.contract.mark_paid(&id, &AMOUNT));

    let inv = t.contract.get_invoice(&id);
    assert_eq!(
        inv.status,
        InvoiceStatus::Funded,
        "settlement must not have committed Paid"
    );
    assert_eq!(inv.amount_paid, 0);
    assert_eq!(t.token_client.balance(&t.payer), payer_before);
    assert_eq!(t.token_client.balance(&t.lp), lp_before);
    assert_eq!(t.token_client.balance(&t.contract.address), contract_before);
}

// ================================================================
// 3. Multi-transfer settlement: fee succeeds, funder share fails
// ================================================================

#[test]
fn settlement_failure_after_fee_transfer_rolls_back_the_fee_too() {
    let t = setup();
    t.contract.update_fee_rate(&100); // 1% protocol fee
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);

    let payer_before = t.token_client.balance(&t.payer);
    let admin_before = t.token_client.balance(&t.admin);
    let lp_before = t.token_client.balance(&t.lp);
    let contract_before = t.token_client.balance(&t.contract.address);

    // #1 payer->contract, #2 contract->admin (fee), #3 contract->LP. Fail #3
    // so the fee transfer at #2 has already succeeded when #3 traps.
    t.mock.arm_fail(&2);

    expect_trap(|| t.contract.mark_paid(&id, &AMOUNT));

    assert_eq!(
        t.token_client.balance(&t.admin),
        admin_before,
        "the already-completed fee transfer must roll back with the later failure"
    );
    assert_eq!(t.token_client.balance(&t.payer), payer_before);
    assert_eq!(t.token_client.balance(&t.lp), lp_before);
    assert_eq!(t.token_client.balance(&t.contract.address), contract_before);
    let inv = t.contract.get_invoice(&id);
    assert_eq!(inv.status, InvoiceStatus::Funded);
    assert_eq!(inv.amount_paid, 0);
}

// ================================================================
// 4. Partial-funding refund on cancel (pay_or_escrow)
// ================================================================

#[test]
fn cancel_partial_refund_failure_leaves_state_unchanged() {
    let t = setup();
    let id = submit(&t);
    let half = AMOUNT / 2;
    t.contract.fund_invoice(&t.lp, &id, &half, &false);
    assert_eq!(
        t.contract.get_invoice(&id).status,
        InvoiceStatus::PartiallyFunded
    );

    let lp_before = t.token_client.balance(&t.lp);
    let contract_before = t.token_client.balance(&t.contract.address);

    // Cancel's only transfer is the refund back to the funder.
    t.mock.arm_fail(&0);

    expect_trap(|| t.contract.cancel_invoice(&id));

    assert_eq!(
        t.contract.get_invoice(&id).status,
        InvoiceStatus::PartiallyFunded,
        "the pre-transfer Cancelled write must roll back"
    );
    assert_eq!(t.token_client.balance(&t.lp), lp_before);
    assert_eq!(t.token_client.balance(&t.contract.address), contract_before);
    assert_eq!(funders(&t, id).len(), 1, "funder entry must survive");
}

// ================================================================
// 5. Principal refund on default (pay_or_escrow)
// ================================================================

#[test]
fn default_refund_failure_leaves_invoice_funded() {
    let t = setup();
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);
    advance_past_due(&t);

    let lp_before = t.token_client.balance(&t.lp);
    let contract_before = t.token_client.balance(&t.contract.address);

    // Funding used transfers #1–#2; the LP refund is #3.
    t.mock.arm_fail(&0);

    expect_trap(|| t.contract.claim_default(&t.lp, &id));

    assert_eq!(
        t.contract.get_invoice(&id).status,
        InvoiceStatus::Funded,
        "the pre-transfer Defaulted write must roll back"
    );
    assert_eq!(t.token_client.balance(&t.lp), lp_before);
    assert_eq!(t.token_client.balance(&t.contract.address), contract_before);
}

// ================================================================
// 6. LP refund on upheld dispute (pay_or_escrow)
// ================================================================

#[test]
fn dispute_upheld_refund_failure_leaves_invoice_disputed() {
    let t = setup();
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);
    t.contract.dispute_invoice(&id, &reason_hash(&t.env));

    let lp_before = t.token_client.balance(&t.lp);
    let contract_before = t.token_client.balance(&t.contract.address);

    // Funding used #1–#2; disputing makes no transfer; the LP refund is #3.
    t.mock.arm_fail(&0);

    expect_trap(|| {
        t.contract
            .resolve_dispute(&t.admin, &id, &reason_hash(&t.env), &1u32)
    });

    assert_eq!(
        t.contract.get_invoice(&id).status,
        InvoiceStatus::Disputed,
        "the pre-transfer Cancelled write must roll back"
    );
    assert_eq!(t.token_client.balance(&t.lp), lp_before);
    assert_eq!(t.token_client.balance(&t.contract.address), contract_before);
}

// ================================================================
// 7. Escrowed payout release (claim_pending — clears escrow before transfer)
// ================================================================

#[test]
fn claim_pending_payout_failure_keeps_escrow_intact() {
    let t = setup();
    let id = submit(&t);
    let half = AMOUNT / 2;
    t.contract.fund_invoice(&t.lp, &id, &half, &false);

    // Freeze the LP so the cancel refund is escrowed instead of transferred.
    t.mock.set_authorized(&t.lp, &false);
    t.contract.cancel_invoice(&id);
    let refund = half - discount(half);
    assert_eq!(t.contract.get_pending_payout(&t.lp, &t.token), refund);

    // Re-authorize so the claim reaches its transfer, then arm it to fail.
    t.mock.set_authorized(&t.lp, &true);
    let lp_before = t.token_client.balance(&t.lp);
    let contract_before = t.token_client.balance(&t.contract.address);
    // Cancel escrowed without transferring, so the claim transfer is #2.
    t.mock.arm_fail(&0);

    expect_trap(|| t.contract.claim_pending_payout(&t.lp, &t.token));

    assert_eq!(
        t.contract.get_pending_payout(&t.lp, &t.token),
        refund,
        "claim_pending clears the escrow before transferring; it must roll back"
    );
    assert_eq!(t.token_client.balance(&t.lp), lp_before);
    assert_eq!(t.token_client.balance(&t.contract.address), contract_before);
}
