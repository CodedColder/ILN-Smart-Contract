//! Issue #698 — cross-contract reentrancy audit.
//!
//! Exercises the *real* `InvoiceLiquidityContract` against a hostile callee
//! that is installed where ILN makes an external call:
//!
//! | ILN entry point   | External call made while the entry point runs        |
//! |-------------------|------------------------------------------------------|
//! | `fund_invoice`    | distribution `accrue_lp`                             |
//! | `mark_paid`       | distribution `accrue_settlement`                     |
//! | `claim_default`   | insurance pool `is_enrolled` / `claim`               |
//!
//! From inside each hook the hostile contract tries to call back into five
//! ILN entry points (`fund_invoice`, `mark_paid`, `claim_default`,
//! `cancel_invoice`, `claim_pending_payout`). Non-root auth is mocked
//! (`mock_all_auths_allowing_non_root_auth`) so an authorization failure can
//! never be what stops the re-entry — only reentrancy protection can.
//!
//! Two independent layers protect ILN (see `docs/reentrancy-audit.md`):
//!
//! 1. The Soroban host refuses to call a contract that is already on the
//!    call stack. This fires first, so a hostile re-entry is rejected by the
//!    host, not by ILN's own lock.
//! 2. ILN's `ReentrancyLock` (Issue #535) is defense-in-depth for the case
//!    the host rule is ever relaxed. It cannot be reached through a hostile
//!    callee today, so it is tested directly by pre-setting the lock.
//!
//! The hostile-callee tests therefore assert the *observable outcome*
//! (re-entry rejected, every ledger applied exactly once, tokens conserved)
//! and deliberately do not assert which layer produced the rejection.

#[path = "mocks/mock_token.rs"]
mod mock_token;

use invoice_liquidity::{
    ContractError, Invoice, InvoiceLiquidityContract, InvoiceLiquidityContractClient,
    InvoiceStatus, ReferralCode,
};
use mock_token::{MockToken, MockTokenClient};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error,
    testutils::{Address as _, EnvTestConfig, Ledger},
    vec, Address, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

const AMOUNT: i128 = 1_000_000_000;
const RATE: u32 = 300;
const DAY: u64 = 24 * 60 * 60;
const T0: u64 = 1_700_000_000;
const SEQ0: u32 = 10_000;
/// ILN pooled reserve; `claim_default` refunds are paid from it ([NE-2]).
const RESERVE: i128 = 10 * AMOUNT;
/// Number of ILN entry points the hostile callee tries to re-enter per hook.
const PROBES: u32 = 5;

// ---------------------------------------------------------------------------
// Hostile callee
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
enum HostileKey {
    Probe,
    Attempts,
    Succeeded,
}

#[contracttype]
#[derive(Clone)]
pub struct Probe {
    pub iln: Address,
    /// An LP that funded (or will fund) the invoice.
    pub victim: Address,
    /// A second LP, used for a competing re-entrant funding tranche.
    pub other: Address,
    pub token: Address,
    pub invoice_id: u64,
    pub fund_amount: i128,
    /// When true the hook aborts with a contract error *after* probing.
    pub trap: bool,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum HostileError {
    Boom = 1,
}

/// Plays the insurance pool *and* the distribution contract.
#[contract]
pub struct ReentrantCallee;

fn attempt(env: &Env, iln: &Address, name: &str, args: Vec<Val>) -> bool {
    matches!(
        env.try_invoke_contract::<Val, soroban_sdk::Error>(iln, &Symbol::new(env, name), args),
        Ok(Ok(_))
    )
}

/// Try to re-enter ILN through every state-changing entry point and record
/// how many of the attempts the callee (wrongly) got through.
fn probe_reentry(env: &Env) {
    let p: Probe = env
        .storage()
        .instance()
        .get(&HostileKey::Probe)
        .expect("hostile callee not configured");

    let mut succeeded = 0u32;
    // A competing funding tranche: the only probe that would visibly change
    // state if re-entry were possible while the funding hook runs.
    succeeded += attempt(
        env,
        &p.iln,
        "fund_invoice",
        vec![
            env,
            p.other.into_val(env),
            p.invoice_id.into_val(env),
            (p.fund_amount / 4).into_val(env),
            false.into_val(env),
        ],
    ) as u32;
    succeeded += attempt(
        env,
        &p.iln,
        "mark_paid",
        vec![env, p.invoice_id.into_val(env), 1_i128.into_val(env)],
    ) as u32;
    succeeded += attempt(
        env,
        &p.iln,
        "claim_default",
        vec![env, p.victim.into_val(env), p.invoice_id.into_val(env)],
    ) as u32;
    succeeded += attempt(
        env,
        &p.iln,
        "cancel_invoice",
        vec![env, p.invoice_id.into_val(env)],
    ) as u32;
    succeeded += attempt(
        env,
        &p.iln,
        "claim_pending_payout",
        vec![env, p.victim.into_val(env), p.token.into_val(env)],
    ) as u32;

    let attempts: u32 = env
        .storage()
        .instance()
        .get(&HostileKey::Attempts)
        .unwrap_or(0);
    let total: u32 = env
        .storage()
        .instance()
        .get(&HostileKey::Succeeded)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&HostileKey::Attempts, &(attempts + PROBES));
    env.storage()
        .instance()
        .set(&HostileKey::Succeeded, &(total + succeeded));

    if p.trap {
        panic_with_error!(env, HostileError::Boom);
    }
}

#[contractimpl]
impl ReentrantCallee {
    pub fn configure(env: Env, probe: Probe) {
        env.storage().instance().set(&HostileKey::Probe, &probe);
    }

    pub fn attempts(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&HostileKey::Attempts)
            .unwrap_or(0)
    }

    /// Number of re-entry attempts that ILN accepted. Must stay 0.
    pub fn succeeded(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&HostileKey::Succeeded)
            .unwrap_or(0)
    }

    // --- insurance pool surface (called from `claim_default`) ---------------

    pub fn is_enrolled(_env: Env, _lp: Address) -> bool {
        true
    }

    pub fn claim(env: Env, _invoice_id: u64, _lp: Address) -> i128 {
        probe_reentry(&env);
        0
    }

    pub fn get_pool_balance(_env: Env) -> i128 {
        0
    }

    // --- distribution surface (called from funding / settlement) ------------

    pub fn accrue_lp(env: Env, _lp: Address, _amount: i128) {
        probe_reentry(&env);
    }

    pub fn accrue_settlement(env: Env, _freelancer: Address, _payer: Address, _on_time: bool) {
        probe_reentry(&env);
    }
}

/// Same storage encoding as ILN's private `ReentrancyLock` instance key, so a
/// test can read/force the lock without exposing it from the contract crate.
#[contracttype]
enum IlnKey {
    ReentrancyLock,
}

// ---------------------------------------------------------------------------
// Fixture (fresh `Env` per test — no state is shared between tests)
// ---------------------------------------------------------------------------

struct World {
    env: Env,
    iln: InvoiceLiquidityContractClient<'static>,
    token: MockTokenClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    lp2: Address,
}

/// Everything a rejected re-entry (or any rejected call) must leave untouched.
#[derive(Debug, PartialEq)]
struct Snapshot {
    invoice: Invoice,
    balances: std::vec::Vec<i128>,
    total_funded: u64,
    total_paid: u64,
    payer_score: u32,
    lp_index: u32,
    lp2_index: u32,
}

fn advance(env: &Env, secs: u64, ledgers: u32) {
    let mut li = env.ledger().get();
    li.timestamp += secs;
    li.sequence_number += ledgers;
    env.ledger().set(li);
}

fn world() -> World {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths_allowing_non_root_auth();
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
    token.mint(&iln_id, &RESERVE);

    World {
        env,
        iln,
        token,
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

    fn actors(&self) -> [Address; 6] {
        [
            self.admin.clone(),
            self.freelancer.clone(),
            self.payer.clone(),
            self.lp.clone(),
            self.lp2.clone(),
            self.iln.address.clone(),
        ]
    }

    fn total_tokens(&self) -> i128 {
        self.actors().iter().map(|a| self.token.balance(a)).sum()
    }

    fn snapshot(&self, id: u64) -> Snapshot {
        let stats = self.iln.get_contract_stats();
        Snapshot {
            invoice: self.iln.get_invoice(&id),
            balances: self
                .actors()
                .iter()
                .map(|a| self.token.balance(a))
                .collect(),
            total_funded: stats.total_funded,
            total_paid: stats.total_paid,
            payer_score: self.iln.payer_score(&self.payer),
            lp_index: self.iln.list_invoices_by_lp(&self.lp, &0, &50).len(),
            lp2_index: self.iln.list_invoices_by_lp(&self.lp2, &0, &50).len(),
        }
    }

    /// Deploy the hostile callee, configure it for invoice `id`, and return it.
    fn hostile(&self, id: u64, trap: bool) -> ReentrantCalleeClient<'static> {
        let addr = self.env.register(ReentrantCallee, ());
        let client = ReentrantCalleeClient::new(&self.env, &addr);
        client.configure(&Probe {
            iln: self.iln.address.clone(),
            victim: self.lp.clone(),
            other: self.lp2.clone(),
            token: self.token.address.clone(),
            invoice_id: id,
            fund_amount: AMOUNT,
            trap,
        });
        client
    }

    fn lock_held(&self) -> bool {
        self.env.as_contract(&self.iln.address, || {
            self.env
                .storage()
                .instance()
                .get::<IlnKey, bool>(&IlnKey::ReentrancyLock)
                .unwrap_or(false)
        })
    }

    fn force_lock(&self, held: bool) {
        self.env.as_contract(&self.iln.address, || {
            self.env
                .storage()
                .instance()
                .set(&IlnKey::ReentrancyLock, &held)
        });
    }
}

// ---------------------------------------------------------------------------
// Layer 2: ILN's own lock, reached directly
// ---------------------------------------------------------------------------

/// While the lock is held, every guarded entry point fails closed with
/// `Reentrancy` and changes nothing; once released, normal operation resumes.
#[test]
fn locked_guard_rejects_every_guarded_entry_point_without_side_effects() {
    let w = world();
    let id = w.submit();
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    assert!(!w.lock_held(), "lock is released after a successful call");

    w.force_lock(true);
    let before = w.snapshot(id);
    let hash = BytesN::from_array(&w.env, &[2; 32]);

    assert_eq!(
        w.iln.try_fund_invoice(&w.lp2, &id, &1, &false),
        Err(Ok(ContractError::Reentrancy))
    );
    assert_eq!(
        w.iln.try_mark_paid(&id, &AMOUNT),
        Err(Ok(ContractError::Reentrancy))
    );
    assert_eq!(
        w.iln.try_claim_default(&w.lp, &id),
        Err(Ok(ContractError::Reentrancy))
    );
    assert_eq!(
        w.iln.try_cancel_invoice(&id),
        Err(Ok(ContractError::Reentrancy))
    );
    assert_eq!(
        w.iln.try_claim_pending_payout(&w.lp, &w.token.address),
        Err(Ok(ContractError::Reentrancy))
    );
    assert_eq!(
        w.iln.try_resolve_dispute(&id, &hash, &1),
        Err(Ok(ContractError::Reentrancy))
    );

    assert_eq!(w.snapshot(id), before, "rejected calls leave no trace");
    assert!(w.lock_held(), "a rejected re-entry must not release the lock");

    // Releasing the lock restores normal behaviour (no permanent lock-out).
    w.force_lock(false);
    w.iln.mark_paid(&id, &AMOUNT);
    assert_eq!(w.iln.get_invoice(&id).status, InvoiceStatus::Paid);
    assert!(!w.lock_held());
}

/// Error paths in the guarded functions return without calling
/// `unlock_reentrancy`. That is safe only because Soroban reverts the whole
/// invocation (including the lock write) on error; prove it so a failed call
/// can never wedge the protocol.
#[test]
fn failed_calls_never_leave_the_lock_held() {
    let w = world();
    let id = w.submit();
    let unknown = 9_999u64;

    assert!(w.iln.try_fund_invoice(&w.lp, &unknown, &AMOUNT, &false).is_err());
    assert!(!w.lock_held());
    assert!(w.iln.try_mark_paid(&unknown, &AMOUNT).is_err());
    assert!(!w.lock_held());
    assert!(w.iln.try_claim_default(&w.lp, &unknown).is_err());
    assert!(!w.lock_held());
    assert!(w.iln.try_cancel_invoice(&unknown).is_err());
    assert!(!w.lock_held());
    // Funded-state errors on a real invoice (not funded yet / not a funder).
    assert!(w.iln.try_mark_paid(&id, &AMOUNT).is_err());
    assert!(!w.lock_held());
    assert!(w.iln.try_claim_default(&w.lp, &id).is_err());
    assert!(!w.lock_held());
    assert!(w.iln.try_fund_invoice(&w.lp, &id, &0, &false).is_err());
    assert!(!w.lock_held());

    // The protocol is still fully usable afterwards.
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    assert_eq!(w.iln.get_invoice(&id).status, InvoiceStatus::Funded);
}

// ---------------------------------------------------------------------------
// Layer 1 + end-to-end: hostile callee at every external call site
// ---------------------------------------------------------------------------

/// Funding: a hostile distribution contract tries to slip a competing LP
/// tranche in (and call every other entry point) from inside `accrue_lp`.
/// Each funding call must be applied exactly once.
#[test]
fn hostile_distribution_cannot_reenter_funding() {
    let w = world();
    let id = w.submit();
    let hostile = w.hostile(id, false);
    w.iln.set_distribution_contract(&hostile.address);
    let supply = w.total_tokens();
    let lp2_balance = w.token.balance(&w.lp2);

    // First half: the hook runs, the re-entrant tranche must not land.
    w.iln.fund_invoice(&w.lp, &id, &(AMOUNT / 2), &false);
    assert_eq!(hostile.attempts(), PROBES, "the hook really ran");
    assert_eq!(hostile.succeeded(), 0, "no re-entry may be accepted");

    let half = w.snapshot(id);
    assert_eq!(half.invoice.status, InvoiceStatus::PartiallyFunded);
    assert_eq!(half.invoice.amount_funded, AMOUNT / 2);
    assert_eq!(half.lp_index, 1);
    assert_eq!(half.lp2_index, 0, "competing LP was never recorded");
    assert_eq!(w.token.balance(&w.lp2), lp2_balance);
    assert_eq!(half.total_funded, 0, "not fully funded yet");

    // Second half completes the invoice; `total_funded` moves exactly once.
    w.iln.fund_invoice(&w.lp, &id, &(AMOUNT / 2), &false);
    assert_eq!(hostile.attempts(), 2 * PROBES);
    assert_eq!(hostile.succeeded(), 0);
    let full = w.snapshot(id);
    assert_eq!(full.invoice.status, InvoiceStatus::Funded);
    assert_eq!(full.invoice.amount_funded, AMOUNT);
    assert_eq!(full.total_funded, 1);
    assert_eq!(full.lp2_index, 0);
    assert_eq!(w.total_tokens(), supply, "tokens conserved across funding");

    // Repeated / stale funding of a Funded invoice is rejected unchanged.
    assert!(w.iln.try_fund_invoice(&w.lp, &id, &1, &false).is_err());
    assert_eq!(w.snapshot(id), full);
}

/// Settlement: a hostile distribution contract tries to re-enter from
/// `accrue_settlement`, which runs *after* the Paid state and payouts are
/// written. Payouts, counters and reputation must still apply exactly once.
#[test]
fn hostile_distribution_cannot_reenter_settlement() {
    let w = world();
    let id = w.submit();
    let hostile = w.hostile(id, false);
    w.iln.set_distribution_contract(&hostile.address);
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    let after_funding = hostile.attempts();

    let supply = w.total_tokens();
    let before = w.snapshot(id);
    w.iln.mark_paid(&id, &AMOUNT);

    assert_eq!(hostile.attempts(), after_funding + PROBES, "settlement hook ran");
    assert_eq!(hostile.succeeded(), 0, "no re-entry may be accepted");

    let after = w.snapshot(id);
    assert_eq!(after.invoice.status, InvoiceStatus::Paid);
    assert_eq!(after.invoice.amount_paid, AMOUNT);
    assert_eq!(after.total_paid, before.total_paid + 1, "counted once");
    assert_eq!(
        after.payer_score,
        before.payer_score + 1,
        "reputation applied once"
    );
    assert!(
        w.token.balance(&w.payer) < before.balances[2],
        "payer was charged"
    );
    assert_eq!(w.total_tokens(), supply, "tokens conserved across settlement");

    // Repeated settlement is rejected and changes nothing.
    assert_eq!(
        w.iln.try_mark_paid(&id, &1),
        Err(Ok(ContractError::AlreadyPaid))
    );
    assert_eq!(w.snapshot(id), after);
}

/// Insurance claim: a hostile pool tries to re-enter `claim_default` (and the
/// other entry points) from `claim`. The default must refund the LP once,
/// move status once, penalise the payer once, and stay conserved.
#[test]
fn hostile_insurance_pool_cannot_reenter_claim_default() {
    let w = world();
    let id = w.submit();
    let hostile = w.hostile(id, false);
    w.iln.set_insurance_pool(&hostile.address);
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);

    let supply = w.total_tokens();
    let before = w.snapshot(id);
    w.iln.claim_default(&w.lp, &id);

    assert_eq!(hostile.attempts(), PROBES, "the pool hook really ran");
    assert_eq!(hostile.succeeded(), 0, "no re-entry may be accepted");

    let after = w.snapshot(id);
    assert_eq!(after.invoice.status, InvoiceStatus::Defaulted);
    assert_eq!(
        w.token.balance(&w.lp) - before.balances[3],
        AMOUNT - AMOUNT * RATE as i128 / 10_000,
        "LP principal refunded exactly once (hostile pool pays no top-up)"
    );
    assert_eq!(
        after.payer_score,
        before.payer_score.saturating_sub(5),
        "payer penalised once"
    );
    assert_eq!(w.total_tokens(), supply, "tokens conserved across default");

    // Repeated / stale default claim is rejected and changes nothing.
    assert_eq!(
        w.iln.try_claim_default(&w.lp, &id),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
    assert_eq!(w.snapshot(id), after);
    assert_eq!(hostile.attempts(), PROBES, "rejected claim never reaches the pool");
}

/// Interrupted external call: the pool re-enters and then aborts. The pool
/// call is made through `try_*`, so the default must still complete exactly
/// once; the pool's own (rolled back) writes must not leak into ILN.
#[test]
fn aborting_insurance_pool_does_not_block_or_double_apply_default() {
    let w = world();
    let id = w.submit();
    let hostile = w.hostile(id, true);
    w.iln.set_insurance_pool(&hostile.address);
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);

    let supply = w.total_tokens();
    let before = w.snapshot(id);
    w.iln.claim_default(&w.lp, &id);

    let after = w.snapshot(id);
    assert_eq!(after.invoice.status, InvoiceStatus::Defaulted);
    assert_eq!(
        w.token.balance(&w.lp) - before.balances[3],
        AMOUNT - AMOUNT * RATE as i128 / 10_000,
        "LP principal refunded exactly once"
    );
    assert_eq!(w.total_tokens(), supply);
    assert!(!w.lock_held(), "lock released after a tolerated pool failure");
    assert_eq!(
        w.iln.try_claim_default(&w.lp, &id),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
    assert_eq!(w.snapshot(id), after);
}

/// Unauthorized caller: re-entry probes name the victim LP, but a non-funder
/// is still refused after the hostile hook has run, so a hostile pool cannot
/// be used to default someone else's invoice.
#[test]
fn non_funder_cannot_claim_default_even_with_hostile_pool_wired() {
    let w = world();
    let id = w.submit();
    let hostile = w.hostile(id, false);
    w.iln.set_insurance_pool(&hostile.address);
    w.iln.fund_invoice(&w.lp, &id, &AMOUNT, &false);
    advance(&w.env, 31 * DAY, 100);

    let before = w.snapshot(id);
    assert_eq!(
        w.iln.try_claim_default(&w.lp2, &id),
        Err(Ok(ContractError::Unauthorized))
    );
    assert_eq!(w.snapshot(id), before);
    assert_eq!(hostile.attempts(), 0, "pool is never reached for a non-funder");
    assert_eq!(w.iln.get_invoice(&id).status, InvoiceStatus::Funded);
}
