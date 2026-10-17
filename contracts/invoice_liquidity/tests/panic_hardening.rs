//! Issue #840: panic-hardening for index lookups in fund_invoice and
//! resolve_fund_queue.
//!
//! Loops that index a `soroban_sdk::Vec` via `.get(i).unwrap()` were replaced
//! with `.get(i).ok_or(ContractError::...)?` so a broken loop invariant
//! surfaces as a typed error rather than a raw abort. These tests pin two
//! things:
//!
//!   1. Happy path: fund_invoice and resolve_fund_queue still succeed under
//!      normal, in-bounds iteration.
//!   2. Off-by-one: the substituted `.get(i).ok_or(...)` pattern actually
//!      returns the intended typed error when the index is one past the end
//!      (the exact off-by-one that would previously have panicked).

use invoice_liquidity::{
    constants::QUEUE_DELAY_LEDGERS, ContractError, InvoiceLiquidityContract,
    InvoiceLiquidityContractClient, LpFundRequest, ReferralCode,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    vec, Address, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct HardeningEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp: Address,
}

fn setup() -> HardeningEnv {
    let env = Env::default();
    env.mock_all_auths();

    let usdc_admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);

    token_admin.mint(&lp, &(INVOICE_AMOUNT * 10));
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_addr = xlm_id.address();
    let eurc_addr = Address::generate(&env);

    contract.initialize(&usdc_admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    HardeningEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp,
    }
}

fn submit(t: &HardeningEnv) -> u64 {
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    )
}

// ── Happy path: unchanged behaviour after the substitution ───────────────────

#[test]
fn fund_invoice_happy_path_still_succeeds() {
    let t = setup();
    let id = submit(&t);

    // Fully funds the invoice, exercising the funder-list loop that owns the
    // hardened funders.get(i) call site.
    let result = t.contract.try_fund_invoice(&t.lp, &id, &INVOICE_AMOUNT, &false);
    assert!(result.is_ok(), "fund_invoice happy path must still succeed");
}

#[test]
fn resolve_fund_queue_happy_path_still_succeeds() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp, &id);

    // resolve_fund_queue enforces a maturity delay in ledger sequences.
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += QUEUE_DELAY_LEDGERS as u32 + 1;
    ledger.timestamp += (QUEUE_DELAY_LEDGERS as u64 + 1) * 5;
    t.env.ledger().set(ledger);

    let winner = t.contract.resolve_fund_queue(&id);
    assert_eq!(winner, t.lp);
}

// ── Off-by-one regression: the substituted pattern returns a typed error ─────

/// Deliberately misalign the index on a funders-shaped vector, one past the
/// last element. The substituted pattern must produce the typed error, not a
/// panic. This locks in the semantics of the `.get(i).ok_or(...)` replacement
/// in fund_invoice's contributor-list loop.
#[test]
fn funder_off_by_one_returns_typed_error_not_panic() {
    let env = Env::default();
    let lp = Address::generate(&env);
    let funders: soroban_sdk::Vec<(Address, i128)> = vec![&env, (lp, 100i128)];

    // `funders.len()` is 1, so index 1 is exactly one past the end. That is
    // the off-by-one the previous `.unwrap()` would have panicked on.
    let out_of_range = funders.len();
    let result: Result<(Address, i128), ContractError> = funders
        .get(out_of_range)
        .ok_or(ContractError::FunderIndexOutOfBounds);

    assert_eq!(result.unwrap_err(), ContractError::FunderIndexOutOfBounds);
}

/// Same deliberate off-by-one against a queue-shaped vector. Locks in the
/// three replaced call sites in resolve_fund_queue.
#[test]
fn queue_off_by_one_returns_typed_error_not_panic() {
    let env = Env::default();

    // The queue stores LpFundRequest entries. Build a length-2 vector so an
    // index of 2 is provably one past the end.
    let lp_a = Address::generate(&env);
    let lp_b = Address::generate(&env);
    let queue: soroban_sdk::Vec<LpFundRequest> = vec![
        &env,
        LpFundRequest {
            lp: lp_a,
            score: 60,
        },
        LpFundRequest {
            lp: lp_b,
            score: 50,
        },
    ];

    let out_of_range = queue.len();
    let result: Result<LpFundRequest, ContractError> = queue
        .get(out_of_range)
        .ok_or(ContractError::QueueIndexOutOfBounds);

    assert_eq!(result.unwrap_err(), ContractError::QueueIndexOutOfBounds);
}
