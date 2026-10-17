#![cfg(test)]

use super::*;
use crate::access::ADMIN_ACTION_LOG_CAPACITY;
use crate::config::Config;
use crate::constants::{
    DEFAULT_LP_SCORE, DEFAULT_RATE_LIMIT_LEDGERS, FUND_QUEUE_TTL_EXTEND_TO,
    FUND_QUEUE_TTL_THRESHOLD, MAX_FUND_QUEUE_LEN, MAX_IDENTITY_ORACLES_PER_FEED,
    MAX_PRICE_SOURCES_PER_FEED, QUEUE_DELAY_LEDGERS, ROLLBACK_SAMPLE_SIZE, TOP_PAYERS_CAPACITY,
    TTL_MIN_LEDGERS_BEFORE_UPGRADE, TTL_UPGRADE_SAMPLE_SIZE,
};
use crate::invoice::InvoiceStatus;
use crate::oracle_registry::OracleFeedType;
use crate::storage::RESOURCE_ENVELOPE;
use crate::twap::MAX_TWAP_SAMPLES;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    xdr, Address, Env, TryFromVal, Val,
};
use std::format;

const INVOICE_AMOUNT: i128 = 100_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct TestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    token_address: Address,
    freelancer: Address,
    payer: Address,
    funder: Address,
}

fn setup() -> TestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let usdc_admin = Address::generate(&env);
    let usdc_contract_id = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let usdc_address = usdc_contract_id.address();
    let token = TokenClient::new(&env, &usdc_address);
    let token_admin = StellarAssetClient::new(&env, &usdc_address);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let funder = Address::generate(&env);

    token_admin.mint(&funder, &1_000_000_000_000);
    token_admin.mint(&payer, &1_000_000_000_000);

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

    let xlm_admin = Address::generate(&env);
    let xlm_address = env.register_stellar_asset_contract_v2(xlm_admin).address();

    let eurc_address = Address::generate(&env);

    contract.initialize(&usdc_admin, &usdc_address, &eurc_address, &xlm_address);

    TestEnv {
        env,
        contract,
        token,
        token_address: usdc_address,
        freelancer,
        payer,
        funder,
    }
}

fn submit_standard_invoice(t: &TestEnv) -> u64 {
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token_address,
        &ReferralCode::None,
    )
}

// ================================================================
// Reputation Decay Tests (Issue #487)
// ================================================================

fn setup_decay_config(t: &TestEnv, decay_rate_bps: u32, decay_period_ledgers: u64) {
    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
        decay_rate_bps,
        decay_period_ledgers,
        dispute_timeout_ledgers: 100,
        xlm_sac_address: Address::generate(&t.env),
        usdc_sac_address: Address::generate(&t.env),
        eurc_sac_address: Address::generate(&t.env),
        price_oracle: None,
        max_oracle_age_ledgers: 17280,
    };
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_config(&t.env, &config);
    });
}

fn set_payer_score_direct(t: &TestEnv, score: u32) {
    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, score);
    });
}

#[test]
fn test_reputation_decay_one_period() {
    let t = setup();
    set_payer_score_direct(&t, 80);
    setup_decay_config(&t, 100, 1000);

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1000;
    t.env.ledger().set(ledger);

    let score = t.contract.payer_score(&t.payer);
    // Decay: 80 - max(80*100/10000, 1) = 80 - 1 = 79
    assert_eq!(score, 79);
}

#[test]
fn test_reputation_decay_multiple_periods() {
    let t = setup();
    set_payer_score_direct(&t, 100);
    setup_decay_config(&t, 100, 1000);

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 3000;
    t.env.ledger().set(ledger);

    let score = t.contract.payer_score(&t.payer);
    // After 3 periods: 100->99->98->97
    assert_eq!(score, 97);
}

#[test]
fn test_reputation_decay_floor() {
    let t = setup();
    set_payer_score_direct(&t, 5);
    setup_decay_config(&t, 5000, 100);

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1000;
    t.env.ledger().set(ledger);

    let score = t.contract.payer_score(&t.payer);
    assert_eq!(score, 0, "Score should floor at 0");
}

#[test]
fn test_reputation_activity_resets_decay() {
    let t = setup();
    set_payer_score_direct(&t, 80);
    setup_decay_config(&t, 100, 1000);

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 500;
    t.env.ledger().set(ledger);

    set_payer_score_direct(&t, 85);

    ledger = t.env.ledger().get();
    ledger.sequence_number += 500;
    t.env.ledger().set(ledger);

    let score = t.contract.payer_score(&t.payer);
    assert_eq!(score, 85, "Score should not decay after activity reset");
}

#[test]
fn test_get_payer_score_persists_decayed_score() {
    let t = setup();
    set_payer_score_direct(&t, 80);
    setup_decay_config(&t, 100, 1000);

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1000;
    t.env.ledger().set(ledger);

    // Call get_payer_score via contract
    let score = t.contract.payer_score(&t.payer);
    assert_eq!(score, 79);

    // Verify raw storage has persisted the decayed score (79)
    t.env.as_contract(&t.contract.address, || {
        let rep = crate::invoice::get_reputation(&t.env, &t.payer);
        assert_eq!(
            rep.score, 79,
            "Decayed score must be persisted in ReputationProfile"
        );
    });
}

#[test]
fn test_get_payer_score_emits_reputation_updated_event() {
    let t = setup();
    set_payer_score_direct(&t, 80);
    setup_decay_config(&t, 100, 1000);

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1000;
    t.env.ledger().set(ledger);

    // Call get_payer_score via contract
    t.contract.payer_score(&t.payer);

    // Check events emitted
    let events = t.env.events().all();
    let reputation_event_exists = events
        .events()
        .into_iter()
        .any(|e| format!("{:?}", e).contains("reputation_updated"));
    assert!(
        reputation_event_exists,
        "ReputationUpdated event should be emitted when score decays during get_payer_score"
    );
}

// ================================================================
// Issue #1062 — TTL cost at maximum queue size
// ================================================================
//
// # Before / after
//
// **Before:** the fund-queue TTL policy (Issues #1011/#1014: every write via
// `invoice::save_fund_queue` extends `FundQueue(id)`'s TTL to
// `FUND_QUEUE_TTL_EXTEND_TO`) was only ever exercised against queues of one
// or two entries (see `tests_queue_archival.rs`). Nothing pinned down the
// behavior at the queue's hard bound (`MAX_FUND_QUEUE_LEN = 50`, Issues
// #1091/#1095): whether a *full* queue — the largest `FundQueue(id)` value
// the contract can ever write — still gets its TTL extended, whether the
// fill-then-displace path at the bound still refreshes it, whether a
// rejected full-queue join leaves the retained entry untouched, and where
// the `get_fund_queue_ttl_margins` ledger boundary falls once the queue is
// at maximum size.
//
// **After:** the tests below fill a queue to exactly `MAX_FUND_QUEUE_LEN`
// and assert that (a) the TTL of the largest retained `FundQueue(id)` value
// is still extended to at least `FUND_QUEUE_TTL_THRESHOLD` (success), (b) a
// strictly higher-scored joiner that displaces the tail refreshes it back
// above the threshold (success, displacement), (c) an equal- or
// lower-scored joiner is rejected with `ContractError::QueueFull` and the
// retained queue is neither mutated nor evicted (failure), (d) `join_fund_queue`
// genuinely enforces the LP's `require_auth()` while `get_fund_queue_ttl_margins`
// stays permissionless (authorization), and (e) at maximum queue size the
// reported margin crosses exactly 0 → -1 at the
// `TTL_MIN_LEDGERS_BEFORE_UPGRADE` boundary (ledger boundary). All of this
// holds because the TTL extension is key-based and independent of the value
// size; these tests lock that invariant in at the bound.

fn advance_ledgers(env: &Env, delta: u32) {
    let mut info = env.ledger().get();
    info.sequence_number += delta;
    info.timestamp += u64::from(delta) * 5;
    env.ledger().set(info);
}

/// Fill `invoice_id`'s fund queue to exactly `MAX_FUND_QUEUE_LEN` entries
/// using distinct LPs that all carry the default score. Because the queue is
/// sorted descending and all scores are equal, each join appends at the tail
/// (no displacement) until the bound is reached.
fn fill_fund_queue_to_max(t: &TestEnv, invoice_id: u64) {
    for _ in 0..MAX_FUND_QUEUE_LEN {
        let lp = Address::generate(&t.env);
        t.contract.join_fund_queue(&lp, &invoice_id);
    }
}

fn fund_queue_len(t: &TestEnv, invoice_id: u64) -> u32 {
    t.env
        .as_contract(&t.contract.address, || invoice::get_fund_queue(&t.env, invoice_id).len())
}

fn fund_queue_present(t: &TestEnv, invoice_id: u64) -> bool {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .has(&StorageKey::FundQueue(invoice_id))
    })
}

fn fund_queue_ttl(t: &TestEnv, invoice_id: u64) -> u32 {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .get_ttl(&StorageKey::FundQueue(invoice_id))
    })
}

fn set_lp_score_direct(t: &TestEnv, lp: &Address, score: u32) {
    t.env.as_contract(&t.contract.address, || {
        invoice::set_lp_score(&t.env, lp, score);
    });
}

// ─────────────────────────────────────────────────────────────────────────
// Success: a full queue still keeps its TTL extended
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_max_queue_size_join_extends_fund_queue_ttl() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    fill_fund_queue_to_max(&t, id);

    assert_eq!(
        fund_queue_len(&t, id),
        MAX_FUND_QUEUE_LEN,
        "queue must reach the hard bound"
    );
    let ttl = fund_queue_ttl(&t, id);
    assert!(
        ttl >= FUND_QUEUE_TTL_THRESHOLD,
        "FundQueue TTL must still be extended at maximum queue size, got {ttl}"
    );
}

#[test]
fn test_max_queue_size_displacing_join_refreshes_fund_queue_ttl() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);

    // Age the entry below the threshold so the next write must perform a
    // real `extend_ttl` rather than short-circuiting to a no-op.
    advance_ledgers(&t.env, FUND_QUEUE_TTL_EXTEND_TO - FUND_QUEUE_TTL_THRESHOLD + 1_000);
    assert!(
        fund_queue_ttl(&t, id) < FUND_QUEUE_TTL_THRESHOLD,
        "setup must leave TTL below the threshold before the displacing join"
    );

    // Strictly higher score than the default tail score => displaces it.
    let displacing = Address::generate(&t.env);
    set_lp_score_direct(&t, &displacing, 100);
    t.contract.join_fund_queue(&displacing, &id);

    assert_eq!(
        fund_queue_len(&t, id),
        MAX_FUND_QUEUE_LEN,
        "displacement must keep the queue at the bound, not grow it"
    );
    let ttl = fund_queue_ttl(&t, id);
    assert!(
        ttl >= FUND_QUEUE_TTL_THRESHOLD,
        "a displacing join at maximum queue size must refresh FundQueue TTL, got {ttl}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Failure: a full queue rejects a same- or lower-scored joiner without
// evicting the retained entry
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_max_queue_size_rejects_equal_score_joiner() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);

    // Default score equals the tail's score — the bound requires a *strict*
    // win, so this must be rejected.
    let latecomer = Address::generate(&t.env);
    let res = t.contract.try_join_fund_queue(&latecomer, &id);
    assert_eq!(res, Err(Ok(ContractError::QueueFull)));

    assert_eq!(
        fund_queue_len(&t, id),
        MAX_FUND_QUEUE_LEN,
        "a rejected join must not shrink or drop the retained queue"
    );
}

#[test]
fn test_max_queue_size_rejects_lower_score_joiner_without_eviction() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);
    let ttl_before = fund_queue_ttl(&t, id);

    let latecomer = Address::generate(&t.env);
    set_lp_score_direct(&t, &latecomer, 10);
    let res = t.contract.try_join_fund_queue(&latecomer, &id);
    assert_eq!(res, Err(Ok(ContractError::QueueFull)));

    assert_eq!(
        fund_queue_len(&t, id),
        MAX_FUND_QUEUE_LEN,
        "the tail entry must not be evicted for a rejected, lower-scored joiner"
    );
    assert_eq!(
        fund_queue_ttl(&t, id),
        ttl_before,
        "a rejected join must not touch the retained queue's TTL"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Authorization
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_max_queue_size_join_fund_queue_requires_lp_authorization() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let lp = Address::generate(&t.env);
    // Replace the permissive all-auths mode with an explicit, empty list, so
    // `require_lp` -> `lp.require_auth()` genuinely fails.
    t.env.mock_auths(&[]);

    let res = t.contract.try_join_fund_queue(&lp, &id);
    assert!(
        res.is_err(),
        "join_fund_queue must require the joining LP's authorization"
    );

    // No partial state may be written by the rejected call.
    assert!(
        !fund_queue_present(&t, id),
        "an unauthorized join must not create a FundQueue entry"
    );
    assert_eq!(fund_queue_len(&t, id), 0);
}

#[test]
fn test_max_queue_size_ttl_margins_need_no_authorization() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);

    // `get_fund_queue_ttl_margins` is Access: Anyone — an observer with no
    // authorization must still be able to read the margins.
    t.env.mock_auths(&[]);

    let margins = t.contract.get_fund_queue_ttl_margins(&id);
    assert_eq!(
        margins.fund_queue_margin,
        Some((FUND_QUEUE_TTL_EXTEND_TO as i64) - (TTL_MIN_LEDGERS_BEFORE_UPGRADE as i64)),
        "a full queue's margin must be readable and positive with no authorization"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Ledger boundary: margin crosses 0 -> -1 at the upgrade threshold, at
// maximum queue size
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_max_queue_size_ttl_margin_crosses_zero_boundary() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);

    // Advance exactly far enough that the remaining TTL equals
    // TTL_MIN_LEDGERS_BEFORE_UPGRADE — margin must land exactly on 0.
    advance_ledgers(&t.env, FUND_QUEUE_TTL_EXTEND_TO - TTL_MIN_LEDGERS_BEFORE_UPGRADE);
    let margins_at_zero = t.contract.get_fund_queue_ttl_margins(&id);
    assert_eq!(margins_at_zero.fund_queue_margin, Some(0));

    // One more ledger — margin must go negative, the zone the pre-upgrade
    // guard (Issue #1082) rejects.
    advance_ledgers(&t.env, 1);
    let margins_past_zero = t.contract.get_fund_queue_ttl_margins(&id);
    assert_eq!(margins_past_zero.fund_queue_margin, Some(-1));
}

// ================================================================
// Issue #1057 — worst-case queue insertion (position sweep)
// ================================================================
//
// **Before:** the #1062 suite above pins the *bound* (`MAX_FUND_QUEUE_LEN`)
// and the TTL behaviour at it, and `tests_benchmarks.rs` measured a single
// max-size admission — one displacing join into a full queue of *equal*
// scores, which happens to land at the head. Nothing varied the insertion
// position, so "the displacing case is the worst case" was an assumption: with
// identical scores *every* admitted joiner is forced to index 0, so the suite
// cannot tell a head insert from a tail one, and the `QueueFull` admission
// boundary was only ever probed with equal scores.
//
// **After:** the tests below build a full queue with *distinct, strictly
// descending* scores and aim joiners at specific positions, pinning the
// insertion semantics the worst-case benchmark relies on:
//   * a head insert lands at index 0 and displaces only the tail (success);
//   * a mid-score joiner lands in its sorted slot (success);
//   * the admission boundary is exact — `score == tail.score` is rejected
//     while `score == tail.score + 1` is admitted (failure / boundary);
//   * a displacing head insert still requires the joiner's authorization
//     (authorization);
//   * and, the ledger-boundary case that matters for MEV mitigation, a
//     displacing insert must **not** reset `FundQueueOpenedAt`, so a latecomer
//     cannot keep pushing back `resolve_fund_queue`'s maturity gate by
//     re-joining.
//
// The per-position cost itself is recorded in `tests_benchmarks.rs`
// (`benchmark_worst_case_queue_insertion`).

/// Read the fund queue into a plain Rust vec of `(lp, score)` pairs, head
/// first, so assertions can index positions directly.
fn queue_entries(t: &TestEnv, id: u64) -> std::vec::Vec<(Address, u32)> {
    t.env.as_contract(&t.contract.address, || {
        let q = invoice::get_fund_queue(&t.env, id);
        let mut out = std::vec::Vec::new();
        for i in 0..q.len() {
            let entry = q.get(i).unwrap();
            out.push((entry.lp, entry.score));
        }
        out
    })
}

/// Fill `id`'s queue to exactly `MAX_FUND_QUEUE_LEN` with *distinct, strictly
/// descending* scores (`100, 98, …, 2`), returning the LPs in queue order
/// (head first). Unlike `fill_fund_queue_to_max`, positions are meaningful
/// here: a joiner's `insert_pos` is the first index whose score is below it.
fn fill_fund_queue_distinct_descending(t: &TestEnv, id: u64) -> std::vec::Vec<Address> {
    let mut lps = std::vec::Vec::new();
    for i in 0..MAX_FUND_QUEUE_LEN {
        let lp = Address::generate(&t.env);
        set_lp_score_direct(t, &lp, 100 - 2 * i);
        t.contract.join_fund_queue(&lp, &id);
        lps.push(lp);
    }
    lps
}

fn fund_queue_opened_at(t: &TestEnv, id: u64) -> Option<u32> {
    t.env.as_contract(&t.contract.address, || {
        invoice::get_fund_queue_opened_at(&t.env, id)
    })
}

#[test]
fn test_1057_head_insert_lands_at_index_zero_and_displaces_tail() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let original = fill_fund_queue_distinct_descending(&t, id);

    // Above every retained entry => insert at index 0, evicting only the tail.
    let joiner = Address::generate(&t.env);
    set_lp_score_direct(&t, &joiner, 101);
    t.contract.join_fund_queue(&joiner, &id);

    let q = queue_entries(&t, id);
    assert_eq!(q.len(), MAX_FUND_QUEUE_LEN, "a displacing join keeps the bound");
    assert_eq!(q[0].0, joiner, "a top score must land at the head");
    assert_eq!(q[0].1, 101);
    assert_eq!(
        q[1].0, original[0],
        "the displaced ordering must shift every retained entry down one"
    );
    assert_eq!(
        q[MAX_FUND_QUEUE_LEN as usize - 1].0,
        original[MAX_FUND_QUEUE_LEN as usize - 2],
        "only the old tail (score 2) may be displaced"
    );
    for pair in q.windows(2) {
        assert!(
            pair[0].1 > pair[1].1,
            "the queue must stay strictly descending after a head insert"
        );
    }
}

#[test]
fn test_1057_middle_score_insert_lands_in_sorted_position() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let original = fill_fund_queue_distinct_descending(&t, id);

    // Score 51 slots between the 52 at index 24 and the 50 at index 25.
    let joiner = Address::generate(&t.env);
    set_lp_score_direct(&t, &joiner, 51);
    t.contract.join_fund_queue(&joiner, &id);

    let q = queue_entries(&t, id);
    assert_eq!(q.len(), MAX_FUND_QUEUE_LEN);
    assert_eq!(q[24].1, 52, "the preceding entry keeps the next-higher score");
    assert_eq!(q[25].0, joiner, "score 51 must land at index 25");
    assert_eq!(q[25].1, 51);
    assert_eq!(q[26].1, 50, "the following entry is the next-lower score");
    for pair in q.windows(2) {
        assert!(pair[0].1 > pair[1].1, "queue must stay strictly descending");
    }
    assert_eq!(
        q[MAX_FUND_QUEUE_LEN as usize - 1].0,
        original[MAX_FUND_QUEUE_LEN as usize - 2],
        "a middle insert still displaces only the tail"
    );
}

#[test]
fn test_1057_admission_boundary_tail_plus_one_is_admitted() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let _ = fill_fund_queue_distinct_descending(&t, id); // tail score is 2

    // Exactly one point above the tail: admitted (a strict win), and it lands
    // in the freed tail slot at index MAX - 1.
    let joiner = Address::generate(&t.env);
    set_lp_score_direct(&t, &joiner, 3);
    t.contract.join_fund_queue(&joiner, &id);

    let q = queue_entries(&t, id);
    assert_eq!(
        q.len(),
        MAX_FUND_QUEUE_LEN,
        "score == tail + 1 must be admitted, keeping the bound"
    );
    assert_eq!(q[MAX_FUND_QUEUE_LEN as usize - 1].0, joiner);
    assert_eq!(q[MAX_FUND_QUEUE_LEN as usize - 1].1, 3);
}

#[test]
fn test_1057_admission_boundary_tail_score_is_rejected() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let _ = fill_fund_queue_distinct_descending(&t, id);
    let full = queue_entries(&t, id);

    // Equal to the tail score (2): the bound requires a *strict* win, so this
    // must be rejected and must leave the queue byte-identical.
    let latecomer = Address::generate(&t.env);
    set_lp_score_direct(&t, &latecomer, 2);
    let res = t.contract.try_join_fund_queue(&latecomer, &id);
    assert_eq!(res, Err(Ok(ContractError::QueueFull)));

    let after = queue_entries(&t, id);
    assert_eq!(after, full, "a rejected join must not mutate the queue");
    assert!(
        !after.iter().any(|(lp, _)| lp == &latecomer),
        "the rejected joiner must not appear in the queue"
    );
}

#[test]
fn test_1057_displacing_head_insert_requires_lp_authorization() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let original = fill_fund_queue_distinct_descending(&t, id);

    let joiner = Address::generate(&t.env);
    set_lp_score_direct(&t, &joiner, 101);
    // Replace the permissive all-auths mode with an explicit, empty list so
    // `require_lp` -> `joiner.require_auth()` genuinely fails.
    t.env.mock_auths(&[]);

    let res = t.contract.try_join_fund_queue(&joiner, &id);
    assert!(
        res.is_err(),
        "a displacing join must require the joining LP's authorization"
    );

    // A rejected displacing join must not displace, evict, or reorder anything.
    let q = queue_entries(&t, id);
    assert_eq!(q.len(), MAX_FUND_QUEUE_LEN);
    assert_eq!(
        q[0].0, original[0],
        "the head must be untouched by a rejected join"
    );
    assert!(!q.iter().any(|(lp, _)| lp == &joiner));
}

#[test]
fn test_1057_displacing_insert_does_not_reset_queue_opened_at() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let _ = fill_fund_queue_distinct_descending(&t, id);

    let opened_at = fund_queue_opened_at(&t, id).expect("the first join records the open ledger");

    // Move the ledger forward, then displace the head: `FundQueueOpenedAt` must
    // stay pinned to the *first* join, or a latecomer could keep pushing back
    // `resolve_fund_queue`'s maturity gate by re-joining.
    advance_ledgers(&t.env, 50);
    let joiner = Address::generate(&t.env);
    set_lp_score_direct(&t, &joiner, 101);
    t.contract.join_fund_queue(&joiner, &id);

    assert_eq!(
        fund_queue_opened_at(&t, id),
        Some(opened_at),
        "a displacing join must not reset the queue's open ledger"
    );
    assert_eq!(
        queue_entries(&t, id)[0].0,
        joiner,
        "the displacing join itself must still have landed at the head"
    );
}

// ================================================================
// Issue #1058 — bound queue resolution CPU cost
// ================================================================
//
// **Before:** `resolve_fund_queue` is not the O(1) operation its "Optimization
// Note" claimed. Issue #708's fairness fix added a scan of the contiguous
// top-score tie prefix (`queue[0..tied_count]`) so ties are broken uniformly at
// random instead of by join order, making resolution read O(tied_count)
// entries — worst case (every entry tied) the whole queue. Nothing declared or
// measured that loop's bound: the #1065 resource envelope declares *stored
// collections*, not this per-invocation loop, and the #1062 benchmark resolved
// an all-tied full queue only incidentally (its LPs all carry the default
// score) without labelling it the worst case or contrasting it against a queue
// with no ties at all.
//
// **After:** the tie-scan bound is declared in `storage::RESOURCE_ENVELOPE`
// (`queue_resolution_tie_fanout` → `MAX_FUND_QUEUE_LEN`) and asserted here; the
// resolution outcomes (all-tied vs unique top score, idempotency, empty queue,
// maturity boundary, permissionless call) are pinned; and
// `tests_benchmarks.rs`'s `benchmark_queue_resolution_cpu_cost_at_cap` measures
// both extremes (all-tied = max fan-out, distinct = fan-out 1) against the
// absolute ceilings.

/// Advance the ledger past the maturity delay (#MEV-1) and resolve `id`'s fund
/// queue, returning the winning LP.
fn resolve_after_delay(t: &TestEnv, id: u64) -> Address {
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    t.contract.resolve_fund_queue(&id)
}

// ── Success: the winner is chosen from the top-score prefix ────────────────

#[test]
fn test_1058_full_all_tied_queue_resolves_to_a_tied_member() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    // Every LP carries the default score, so the top-score prefix is the whole
    // queue — the maximum tie fan-out the resolution loop can face.
    fill_fund_queue_to_max(&t, id);
    let entries = queue_entries(&t, id);
    assert_eq!(entries.len() as u32, MAX_FUND_QUEUE_LEN);
    assert!(
        entries.iter().all(|(_, s)| *s == DEFAULT_LP_SCORE),
        "the fixture must produce an all-tied queue"
    );

    let winner = resolve_after_delay(&t, id);
    assert!(
        entries.iter().any(|(lp, _)| *lp == winner),
        "an all-tied queue must resolve to one of its tied members"
    );
}

#[test]
fn test_1058_full_distinct_queue_resolves_to_the_unique_top_score() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    // Distinct, strictly descending scores → the top-score prefix is a single
    // entry (fan-out 1), so resolution is deterministic.
    let lps = fill_fund_queue_distinct_descending(&t, id);
    let expected_top = lps[0].clone();

    assert_eq!(resolve_after_delay(&t, id), expected_top);
}

#[test]
fn test_1058_resolution_is_idempotent() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);

    let first = resolve_after_delay(&t, id);
    // A second call returns the already-recorded winner (no re-selection, no
    // further scan) rather than resolving afresh.
    assert_eq!(t.contract.resolve_fund_queue(&id), first);
}

// ── Failure: no queue, and not-yet-mature, both fail closed ────────────────

#[test]
fn test_1058_empty_queue_resolution_fails_not_funded() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let res = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(res, Err(Ok(ContractError::NotFunded)));
}

// ── Ledger boundary: the maturity delay is exact ───────────────────────────

#[test]
fn test_1058_resolution_maturity_ledger_boundary() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let lp = Address::generate(&t.env);
    t.contract.join_fund_queue(&lp, &id);

    // One ledger short of the delay: fail closed.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS - 1);
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );

    // Exactly at the boundary: resolves to the sole LP.
    advance_ledgers(&t.env, 1);
    assert_eq!(t.contract.resolve_fund_queue(&id), lp);
}

// ── Authorization: resolution is permissionless ────────────────────────────

#[test]
fn test_1058_resolution_is_permissionless() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let lp = Address::generate(&t.env);
    t.contract.join_fund_queue(&lp, &id);

    // Resolution guards no privileged action, so with every authorization
    // stripped it still succeeds — anyone may trigger it.
    t.env.mock_auths(&[]);
    assert_eq!(resolve_after_delay(&t, id), lp);
}

// ── Bound: the tie-scan loop equals the declared envelope cap ──────────────

#[test]
fn test_1058_tie_fanout_bounded_by_declared_envelope() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let declared = envelope_cap("queue_resolution_tie_fanout")
        .expect("the resolution tie-scan bound must be declared");
    assert_eq!(declared, MAX_FUND_QUEUE_LEN);

    // Worst case for the loop: a full queue whose every entry ties at the top
    // score, so the scan reads all `declared` entries — never more.
    fill_fund_queue_to_max(&t, id);
    let entries = queue_entries(&t, id);
    let fanout = entries
        .iter()
        .take_while(|(_, s)| *s == DEFAULT_LP_SCORE)
        .count() as u32;
    assert_eq!(
        fanout, declared,
        "the maximum tie fan-out must equal the declared loop bound"
    );
}

// ================================================================
// Issue #1059 — bound unresolved request storage growth
// ================================================================
//
// **Before:** an unresolved fund request's on-chain footprint is the
// `DataKey::FundQueue(u64)` entry (up to `MAX_FUND_QUEUE_LEN` `LpFundRequest`
// records) plus `DataKey::FundQueueOpenedAt(u64)`. Nothing removes either key
// on the terminal paths: `resolve_fund_queue` writes a `QueueResolution`, and
// `cancel_invoice`/`expire_invoice` only rewrite the invoice — none of them
// deletes the queue. The retained footprint therefore only ever shrinks via
// TTL expiry, yet `docs/audit-evidence-remediation-map.md` §3 claimed "queues
// drain as invoices resolve, are funded or are cancelled". The live queue is
// capped (`MAX_FUND_QUEUE_LEN = 50`, Issues #1091/#1095), but that cap was
// declared only as a *live-length* bound (`fund_queue_entries` in
// `storage::RESOURCE_ENVELOPE`), never as the *retained* bound it actually is,
// and nothing measured or pinned the retention.
//
// **After:** the retained unresolved-queue footprint is declared as its own
// envelope entry (`unresolved_queue_retained_entries` → `MAX_FUND_QUEUE_LEN`)
// and asserted here; the tests below prove the retained `FundQueue` entry can
// never exceed that cap (success), that a full queue rejects rather than grows
// (failure), that an unauthorized join cannot inflate it (authorization), and
// that resolution/cancellation at the maturity ledger boundary retain — not
// drain — the entry (ledger boundary). The footprint cost at the bound is
// measured in `tests_benchmarks.rs`
// (`benchmark_unresolved_queue_storage_footprint`).

/// Whether a `QueueResolution` has been recorded for `id`.
fn queue_resolution_present(t: &TestEnv, id: u64) -> bool {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .has(&StorageKey::QueueResolution(id))
    })
}

// ── Success: the retained footprint sits at the declared cap ───────────────

#[test]
fn test_1059_unresolved_queue_retained_footprint_is_capped() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    fill_fund_queue_to_max(&t, id);

    let declared = envelope_cap("unresolved_queue_retained_entries")
        .expect("the retained unresolved-queue bound must be declared");
    assert_eq!(
        declared, MAX_FUND_QUEUE_LEN,
        "the retention bound must reference the enforcing queue cap"
    );
    assert!(
        fund_queue_present(&t, id),
        "an unresolved queue must be retained on-chain"
    );
    assert_eq!(
        fund_queue_len(&t, id),
        declared,
        "the retained footprint must sit exactly at the declared cap"
    );
}

#[test]
fn test_1059_retained_footprint_does_not_grow_under_churn() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_distinct_descending(&t, id);

    // Repeated displacing joins (each strictly outscores the whole queue) keep
    // the entry at the cap — growth is bounded by the cap, not by churn.
    for k in 0..5u32 {
        let joiner = Address::generate(&t.env);
        set_lp_score_direct(&t, &joiner, 200 + k);
        t.contract.join_fund_queue(&joiner, &id);
        assert_eq!(
            fund_queue_len(&t, id),
            MAX_FUND_QUEUE_LEN,
            "churn must never push the retained footprint past the cap"
        );
    }
}

#[test]
fn test_1059_resolution_does_not_drain_the_retained_footprint() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);

    let _winner = resolve_after_delay(&t, id);

    // Resolution records the winner but leaves the queue in place: the
    // footprint is retained (TTL-reclaimed), not drained.
    assert!(
        queue_resolution_present(&t, id),
        "resolution must record a QueueResolution"
    );
    assert!(
        fund_queue_present(&t, id),
        "resolve_fund_queue must not delete the retained fund queue"
    );
    assert_eq!(
        fund_queue_len(&t, id),
        MAX_FUND_QUEUE_LEN,
        "resolution must leave the retained footprint at the cap"
    );
}

#[test]
fn test_1059_cancellation_does_not_drain_the_retained_footprint() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_to_max(&t, id);

    t.contract.cancel_invoice(&id);

    assert!(
        fund_queue_present(&t, id),
        "cancel_invoice must not delete the retained fund queue"
    );
    assert_eq!(
        fund_queue_len(&t, id),
        MAX_FUND_QUEUE_LEN,
        "cancellation must leave the retained footprint at the cap"
    );
}

// ── Failure: a full queue rejects rather than grows ────────────────────────

#[test]
fn test_1059_full_queue_rejects_beyond_the_bound_without_growth() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    fill_fund_queue_distinct_descending(&t, id);
    let before = queue_entries(&t, id);

    // Equal-to-tail score cannot displace, so the join must be rejected.
    let joiner = Address::generate(&t.env);
    set_lp_score_direct(&t, &joiner, before.last().unwrap().1);
    assert_eq!(
        t.contract.try_join_fund_queue(&joiner, &id),
        Err(Ok(ContractError::QueueFull))
    );

    let after = queue_entries(&t, id);
    assert_eq!(
        after.len() as u32,
        MAX_FUND_QUEUE_LEN,
        "a rejected join must not grow the retained footprint"
    );
    assert_eq!(
        after[0].0, before[0].0,
        "a rejected join must leave the retained queue untouched"
    );
    assert_eq!(
        after.last().unwrap().0,
        before.last().unwrap().0,
        "a rejected join must not displace the tail"
    );
}

#[test]
fn test_1059_rejoin_of_an_existing_lp_is_rejected_without_growth() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let lp = Address::generate(&t.env);
    t.contract.join_fund_queue(&lp, &id);
    assert_eq!(fund_queue_len(&t, id), 1);

    // A duplicate join is rejected, so a single LP cannot inflate the retained
    // footprint by re-joining.
    assert_eq!(
        t.contract.try_join_fund_queue(&lp, &id),
        Err(Ok(ContractError::AlreadyInQueue))
    );
    assert_eq!(fund_queue_len(&t, id), 1);
}

// ── Authorization: an unauthorized join cannot grow the footprint ──────────

#[test]
fn test_1059_join_requires_lp_auth_so_footprint_cannot_be_inflated() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let lp = Address::generate(&t.env);
    // Strip all authorization so `require_lp` -> `lp.require_auth()` fails.
    t.env.mock_auths(&[]);

    assert!(
        t.contract.try_join_fund_queue(&lp, &id).is_err(),
        "an unauthorized join must fail"
    );
    assert!(
        !fund_queue_present(&t, id),
        "an unauthorized join must not create retained storage"
    );
    assert_eq!(fund_queue_len(&t, id), 0);
}

// ── Ledger boundary: retention survives the maturity boundary ──────────────

#[test]
fn test_1059_retention_survives_the_maturity_boundary() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let lp = Address::generate(&t.env);
    t.contract.join_fund_queue(&lp, &id);

    // One ledger short of the maturity delay: fail closed, entry retained.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS - 1);
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );
    assert!(fund_queue_present(&t, id));
    assert_eq!(fund_queue_len(&t, id), 1);

    // Exactly at the boundary: resolves, but the entry is still retained.
    advance_ledgers(&t.env, 1);
    assert_eq!(t.contract.resolve_fund_queue(&id), lp);
    assert!(fund_queue_present(&t, id));
    assert_eq!(fund_queue_len(&t, id), 1);
}

// ================================================================
// Issue #1063 — stress concurrent invoice settlements
// ================================================================
//
// # Before / after
//
// **Before:** settlement (`mark_paid`) was only ever exercised one invoice at
// a time (e.g. `tests_coverage_gate.rs::test_fee_tiers_apply_to_settlement`,
// `test.rs::test_mark_paid_settles_correctly_*`). No test settled a batch of
// independent invoices in a single ledger, so nothing pinned down the
// per-invoice isolation the accounting depends on: that the shared counters
// (`TotalPaid`, `ContractStats::total_paid`) and the per-address reputation
// profile (`invoices_paid`, score) advance exactly once per settlement and
// never cross-contaminate between invoices settled in the same ledger; that a
// *rejected* settlement (double-pay, overpay) rolls back cleanly and leaves
// every other invoice settleable (no leaked reentrancy lock, no half-written
// index); and where the `paid_on_time` due-date boundary actually falls.
//
// **After:** the tests below settle a batch of `SETTLE_STRESS_BATCH`
// independent invoices and assert per-invoice isolation, aggregate-counter
// correctness, rejection isolation, authorization, and the due-date boundary.
// The corresponding per-invocation cost — which is what mainnet's resource
// limit actually applies to — is recorded in `tests_benchmarks.rs`
// (`benchmark_settle_batch_same_ledger`).

/// Number of independent invoices settled together in the batch tests. Large
/// enough that any cross-invoice accumulation bug in the shared counters or
/// reentrancy guard surfaces, small enough to stay a fast unit test.
const SETTLE_STRESS_BATCH: u32 = 25;

/// Submit and fully fund an invoice owned by a fresh payer (minted exactly
/// enough to settle once), returning `(invoice_id, payer)`.
fn funded_invoice_with_fresh_payer(t: &TestEnv) -> (u64, Address) {
    let payer = Address::generate(&t.env);
    StellarAssetClient::new(&t.env, &t.token_address).mint(&payer, &INVOICE_AMOUNT);

    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let invoice_id = t.contract.submit_invoice(
        &t.freelancer,
        &payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token_address,
        &ReferralCode::None,
    );
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &false);
    (invoice_id, payer)
}

/// Decode the `paid_on_time` flag from the most recent `paid` event the
/// contract emitted. `mark_paid` publishes `paid` as its final event, so this
/// must be read immediately after the settlement call, before any other
/// contract invocation resets `env.events().all()`.
fn last_paid_on_time(t: &TestEnv) -> bool {
    let raw = t
        .env
        .events()
        .all()
        .filter_by_contract(&t.contract.address)
        .events();
    let last = raw.last().expect("mark_paid must emit a `paid` event");
    let body = match &last.body {
        xdr::ContractEventBody::V0(body) => body,
    };
    let data_val = Val::try_from_val(&t.env, &body.data).expect("decode `paid` event data");
    let decoded = InvoicePaid::try_from_val(&t.env, &data_val)
        .expect("the last contract event must be the `paid` event");
    decoded.paid_on_time
}

// ── Success ────────────────────────────────────────────────────────────────

#[test]
fn test_settle_batch_same_ledger_is_isolated() {
    let t = setup();

    let mut invoices = soroban_sdk::Vec::new(&t.env);
    for _ in 0..SETTLE_STRESS_BATCH {
        invoices.push_back(funded_invoice_with_fresh_payer(&t));
    }

    // Settle every invoice without advancing the ledger: all settlements are
    // observed at the same sequence number and timestamp ("concurrent").
    for (invoice_id, _payer) in invoices.iter() {
        t.contract.mark_paid(&invoice_id, &INVOICE_AMOUNT);
    }

    // Per-invoice isolation: each invoice settled exactly once, and each fresh
    // payer's reputation reflects exactly its own settlement.
    for (invoice_id, payer) in invoices.iter() {
        let invoice = t.contract.get_invoice(&invoice_id).unwrap();
        assert_eq!(
            invoice.status,
            InvoiceStatus::Paid,
            "invoice {invoice_id} must be Paid after the batch"
        );
        let reputation = t.contract.get_reputation(&payer);
        assert_eq!(
            reputation.invoices_paid, 1,
            "each payer must count exactly its own settlement"
        );
    }

    let stats = t.contract.get_contract_stats();
    assert_eq!(
        stats.total_paid,
        u64::from(SETTLE_STRESS_BATCH),
        "one TotalPaid increment per settled invoice, no more"
    );
    assert_eq!(stats.total_invoices, u64::from(SETTLE_STRESS_BATCH));
}

#[test]
fn test_settle_batch_across_ledger_boundary_stays_consistent() {
    let t = setup();

    let mut invoices = soroban_sdk::Vec::new(&t.env);
    for _ in 0..SETTLE_STRESS_BATCH {
        invoices.push_back(funded_invoice_with_fresh_payer(&t));
    }

    let half = (SETTLE_STRESS_BATCH / 2) as usize;
    for (invoice_id, _payer) in invoices.iter().take(half) {
        t.contract.mark_paid(&invoice_id, &INVOICE_AMOUNT);
    }

    // Cross a ledger boundary mid-batch: the first half must remain settled
    // and the second half must settle just as cleanly.
    advance_ledgers(&t.env, 1);
    for (invoice_id, _payer) in invoices.iter().skip(half) {
        t.contract.mark_paid(&invoice_id, &INVOICE_AMOUNT);
    }

    for (invoice_id, _payer) in invoices.iter() {
        assert_eq!(
            t.contract.get_invoice(&invoice_id).unwrap().status,
            InvoiceStatus::Paid
        );
    }
    assert_eq!(
        t.contract.get_contract_stats().total_paid,
        u64::from(SETTLE_STRESS_BATCH)
    );
}

#[test]
fn test_partial_then_full_settlement_interleaved() {
    let t = setup();
    let (id_a, _payer_a) = funded_invoice_with_fresh_payer(&t);
    let (id_b, _payer_b) = funded_invoice_with_fresh_payer(&t);
    let half = INVOICE_AMOUNT / 2;

    // A pays half; B settles fully while A is still outstanding; then A
    // completes — the interleaving must not confuse either invoice's tally.
    t.contract.mark_paid(&id_a, &half);
    assert_eq!(
        t.contract.get_invoice(&id_a).unwrap().status,
        InvoiceStatus::Funded,
        "a partial payment must not mark the invoice Paid"
    );

    t.contract.mark_paid(&id_b, &INVOICE_AMOUNT);
    t.contract.mark_paid(&id_a, &(INVOICE_AMOUNT - half));

    assert_eq!(
        t.contract.get_invoice(&id_a).unwrap().status,
        InvoiceStatus::Paid
    );
    assert_eq!(
        t.contract.get_invoice(&id_b).unwrap().status,
        InvoiceStatus::Paid
    );
    assert_eq!(
        t.contract.get_contract_stats().total_paid,
        2,
        "only the two completing settlements count, not the partial one"
    );
}

// ── Failure ────────────────────────────────────────────────────────────────

#[test]
fn test_rejected_double_settlement_does_not_block_others() {
    let t = setup();
    let (id_a, _payer_a) = funded_invoice_with_fresh_payer(&t);
    let (id_b, _payer_b) = funded_invoice_with_fresh_payer(&t);

    t.contract.mark_paid(&id_a, &INVOICE_AMOUNT);

    // A second settlement of the same invoice must be rejected...
    let res = t.contract.try_mark_paid(&id_a, &INVOICE_AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::AlreadyPaid)));

    // ...and must leave the protocol able to settle an unrelated invoice in
    // the same ledger. A leaked reentrancy lock or a corrupted invoice index
    // would surface here as a failure.
    t.contract.mark_paid(&id_b, &INVOICE_AMOUNT);
    assert_eq!(
        t.contract.get_invoice(&id_b).unwrap().status,
        InvoiceStatus::Paid
    );
    assert_eq!(t.contract.get_contract_stats().total_paid, 2);
}

#[test]
fn test_settle_pending_invoice_is_rejected() {
    let t = setup();
    let id = submit_standard_invoice(&t); // submitted but never funded

    let res = t.contract.try_mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::NotFunded)));
    assert_eq!(
        t.contract.get_invoice(&id).unwrap().status,
        InvoiceStatus::Pending
    );
}

#[test]
fn test_overpayment_rejected_and_invoice_stays_settleable() {
    let t = setup();
    let (id, _payer) = funded_invoice_with_fresh_payer(&t);

    let res = t.contract.try_mark_paid(&id, &(INVOICE_AMOUNT + 1));
    assert_eq!(res, Err(Ok(ContractError::OverpaymentRejected)));

    // The rejected attempt must have rolled back cleanly: the invoice is
    // still settleable for exactly its face value.
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(
        t.contract.get_invoice(&id).unwrap().status,
        InvoiceStatus::Paid
    );
}

// ── Authorization ──────────────────────────────────────────────────────────

#[test]
fn test_mark_paid_requires_payer_authorization() {
    let t = setup();
    let (id, _payer) = funded_invoice_with_fresh_payer(&t);

    // Replace the permissive all-auths mode with an explicit, empty list so
    // `require_payer_by_id`'s `require_auth()` genuinely enforces the payer.
    t.env.mock_auths(&[]);
    let res = t.contract.try_mark_paid(&id, &INVOICE_AMOUNT);
    assert!(
        res.is_err(),
        "mark_paid must require the payer's authorization"
    );

    // The unauthorized attempt must not have settled the invoice (reading a
    // view needs no authorization).
    assert_eq!(
        t.contract.get_invoice(&id).unwrap().status,
        InvoiceStatus::Funded
    );
}

// ── Ledger boundary ────────────────────────────────────────────────────────

#[test]
fn test_paid_on_time_flips_at_due_date_boundary() {
    let t = setup();
    let (id_on_time, _payer_on_time) = funded_invoice_with_fresh_payer(&t);
    let (id_late, _payer_late) = funded_invoice_with_fresh_payer(&t);
    let due_date = u64::from(t.contract.get_invoice(&id_on_time).unwrap().due_date);

    // Exactly at the due date: `timestamp <= due_date` => on time.
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = due_date;
    t.env.ledger().set(ledger);
    t.contract.mark_paid(&id_on_time, &INVOICE_AMOUNT);
    assert!(
        last_paid_on_time(&t),
        "a settlement executed exactly at the due date must be recorded on time"
    );

    // One second past the due date: a late settlement, still permitted.
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = due_date + 1;
    t.env.ledger().set(ledger);
    t.contract.mark_paid(&id_late, &INVOICE_AMOUNT);
    assert!(
        !last_paid_on_time(&t),
        "a settlement one second past the due date must be recorded late"
    );
}

// ================================================================
// Issue #1064 — prevent unbounded event payloads
// ================================================================
//
// The per-feed price-source list (`DataKey::PriceSources`, see
// `storage.rs`) was appended to by `add_price_source` with no upper bound,
// and `get_verified_price` (`oracle_registry.rs`) iterates that whole list on
// every price read, publishing one `PriceOutlierRejected` event *per* source
// that deviates past the configured threshold.
//
// **Before:** the number of `PriceOutlierRejected` events a single
// `get_verified_price` invocation could emit — and the storage/CPU cost of the
// iteration — grew without bound with the registry, so governance (or a
// compromised admin key) could grow a feed until a price read exceeded
// Soroban's per-transaction resource limits, failing the price path for every
// token on that feed.
//
// **After:** `add_price_source` caps each feed at `MAX_PRICE_SOURCES_PER_FEED`
// (rejecting a genuinely new source with `ContractError::TooManyPriceSources`
// once full). The tests below assert the cap holds at the boundary, is
// per-feed-type, keeps idempotent re-registration a no-op, frees a slot on
// removal, still requires admin authorization, and — the point of the fix —
// that the event fan-out of a single price read is bounded by the cap.

/// A minimal price-reporting oracle: `get_price(token)` returns whatever was
/// last set via `set_price`. Self-contained (mirrors `tests_price_deviation.rs`
/// and `tests_oracle_registry.rs`, which each define their own) so this file
/// stays independent of the other test modules.
#[contract]
struct MockPriceOracle;

#[contractimpl]
impl MockPriceOracle {
    pub fn set_price(env: Env, price: i128) {
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("price"), &price);
    }

    pub fn get_price(env: Env, _token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("price"))
            .unwrap_or(0)
    }
}

fn deploy_price_oracle(t: &TestEnv, price: i128) -> Address {
    let id = t.env.register_contract(None, MockPriceOracle);
    MockPriceOracleClient::new(&t.env, &id).set_price(&price);
    id
}

/// `add_price_source`, then advance the ledger past the fn's own
/// `DEFAULT_RATE_LIMIT_LEDGERS` cooldown so the next add is not
/// `RateLimited`. `add_price_source` is rate-limited (Issue #541); filling a
/// feed to the cap therefore requires spacing the adds out.
fn add_price_source_advancing(t: &TestEnv, feed: OracleFeedType, oracle: &Address) {
    t.contract.add_price_source(&feed, oracle);
    advance_ledgers(&t.env, DEFAULT_RATE_LIMIT_LEDGERS as u32);
}

/// Fill `feed`'s price-source list to exactly `MAX_PRICE_SOURCES_PER_FEED`
/// using distinct addresses, returning them in registration order.
fn fill_price_sources_to_max(t: &TestEnv, feed: OracleFeedType) -> std::vec::Vec<Address> {
    let mut sources = std::vec::Vec::new();
    for _ in 0..MAX_PRICE_SOURCES_PER_FEED {
        let oracle = Address::generate(&t.env);
        add_price_source_advancing(t, feed.clone(), &oracle);
        sources.push(oracle);
    }
    sources
}

/// Number of events the contract published in the last top-level invocation
/// (`events().all()` reflects only the most recently completed one — see
/// `tests_lifecycle_integration.rs`).
fn contract_event_count(t: &TestEnv) -> u32 {
    t.env
        .events()
        .all()
        .filter_by_contract(&t.contract.address)
        .events()
        .len() as u32
}

// ── Success: the cap is reached and then enforced ──────────────────────────

#[test]
fn test_price_sources_capped_at_max() {
    let t = setup();
    fill_price_sources_to_max(&t, OracleFeedType::Price);

    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        MAX_PRICE_SOURCES_PER_FEED,
        "the feed must hold exactly the cap after filling it"
    );

    // One more new source is rejected, and the list is left untouched.
    let overflow = Address::generate(&t.env);
    let res = t
        .contract
        .try_add_price_source(&OracleFeedType::Price, &overflow);
    assert_eq!(res, Err(Ok(ContractError::TooManyPriceSources)));
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        MAX_PRICE_SOURCES_PER_FEED,
        "a rejected add must not mutate the registry"
    );
}

#[test]
fn test_price_source_cap_is_per_feed_type() {
    let t = setup();
    fill_price_sources_to_max(&t, OracleFeedType::Price);

    // A different feed type has its own independent budget.
    let credit_oracle = Address::generate(&t.env);
    add_price_source_advancing(&t, OracleFeedType::Credit, &credit_oracle);
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Credit).len(),
        1,
        "the Price feed being full must not block the Credit feed"
    );
}

// ── Boundary: idempotency and slot recovery at the cap ─────────────────────

#[test]
fn test_readding_existing_source_at_cap_is_noop() {
    let t = setup();
    let sources = fill_price_sources_to_max(&t, OracleFeedType::Price);
    let existing = sources.first().unwrap().clone();

    // Re-registering an already-present source must stay a no-op (preserving
    // `test_add_price_source_is_idempotent`'s contract), not fail as
    // `TooManyPriceSources` just because the feed is full.
    let res = t
        .contract
        .try_add_price_source(&OracleFeedType::Price, &existing);
    assert!(res.is_ok(), "re-registering an existing source must not be rejected at the cap");
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        MAX_PRICE_SOURCES_PER_FEED,
        "a no-op re-registration must not change the list length"
    );
}

#[test]
fn test_removing_price_source_frees_a_slot_at_cap() {
    let t = setup();
    let sources = fill_price_sources_to_max(&t, OracleFeedType::Price);
    let victim = sources.first().unwrap().clone();

    let blocked = Address::generate(&t.env);
    assert_eq!(
        t.contract
            .try_add_price_source(&OracleFeedType::Price, &blocked),
        Err(Ok(ContractError::TooManyPriceSources)),
        "a new source must be rejected while the feed is at the cap"
    );

    // Removal frees exactly one slot...
    t.contract
        .remove_price_source(&OracleFeedType::Price, &victim);
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        MAX_PRICE_SOURCES_PER_FEED - 1
    );

    // ...which a previously-rejected new source can then take.
    t.contract
        .add_price_source(&OracleFeedType::Price, &blocked);
    let after = t.contract.get_price_sources(&OracleFeedType::Price);
    assert_eq!(after.len(), MAX_PRICE_SOURCES_PER_FEED);
    assert!(
        after.iter().any(|s| s == blocked),
        "the freed slot must be claimable by a new source"
    );
}

// ── Authorization: the cap does not bypass admin gating ────────────────────

#[test]
fn test_add_price_source_requires_admin_below_cap() {
    let t = setup();
    // An empty explicit auth list makes `require_admin`'s `require_auth()`
    // genuinely fail — there is no cap-related bypass.
    t.env.mock_auths(&[]);
    let oracle = Address::generate(&t.env);

    let res = t
        .contract
        .try_add_price_source(&OracleFeedType::Price, &oracle);
    assert!(
        res.is_err(),
        "add_price_source must still require admin authorization"
    );
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        0
    );
}

// ── The bound itself: a price read's event fan-out is capped ───────────────

#[test]
fn test_price_query_event_fan_out_is_bounded_by_the_cap() {
    let t = setup();
    // Fill the feed to the cap with a strict majority of agreeing sources
    // (which survive cross-validation) plus a minority reporting a wildly
    // different price (each of which is rejected, emitting one
    // `PriceOutlierRejected`). The majority keeps the read successful so its
    // events are observable.
    let survivors = (MAX_PRICE_SOURCES_PER_FEED / 2) + 1;
    for _ in 0..survivors {
        let oracle = deploy_price_oracle(&t, 100);
        add_price_source_advancing(&t, OracleFeedType::Price, &oracle);
    }
    for _ in survivors..MAX_PRICE_SOURCES_PER_FEED {
        let oracle = deploy_price_oracle(&t, 10_000_000);
        add_price_source_advancing(&t, OracleFeedType::Price, &oracle);
    }
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        MAX_PRICE_SOURCES_PER_FEED
    );

    // Drain the events left by the registration calls so the count below is
    // scoped to the price read itself.
    let _ = t.env.events().all();

    // The agreeing majority survives, so the read succeeds...
    let price = t
        .contract
        .get_verified_price(&OracleFeedType::Price, &t.token_address);
    assert_eq!(
        price, 100,
        "the agreeing majority must survive and set the price"
    );

    // ...and the number of `PriceOutlierRejected` events it emitted is
    // bounded by the (now capped) registry size. Before Issue #1064 the
    // registry had no cap, so this fan-out grew without bound with it.
    let emitted = contract_event_count(&t);
    assert!(
        emitted >= 1,
        "at least the deviating sources must have been rejected"
    );
    assert!(
        emitted <= MAX_PRICE_SOURCES_PER_FEED,
        "a price read must emit at most one outlier event per registered source"
    );
}

// ================================================================
// Issue #1065 — document safe resource envelopes
// ================================================================
//
// # Before / after
//
// **Before:** every bound that keeps an on-chain collection — and therefore the
// per-invocation resource cost of touching it — finite lived only in the module
// that enforced it: `MAX_FUND_QUEUE_LEN`, `MAX_PRICE_SOURCES_PER_FEED` and
// `TOP_PAYERS_CAPACITY` in `constants.rs`, `ADMIN_ACTION_LOG_CAPACITY` in
// `access.rs`, `MAX_TWAP_SAMPLES` in `twap.rs`. There was no single artifact
// stating *which* collections are the bounded-by-design ones or what bounds
// them, so the envelope had to be reconstructed by grepping, and a newly
// introduced unbounded collection would not stand out.
//
// **After:** `storage::RESOURCE_ENVELOPE` (see `storage.rs`) declares every
// bounded collection as `(name, cap)` pairs, and the tests below keep that
// declaration honest: each declared cap must equal the constant that actually
// enforces it, the entries must be well-formed and unique, a live collection
// must sit at exactly its declared cap at its boundary (and reject one more),
// and even a fully-authorized admin must not be able to exceed a declared cap.
// The read-side cost of the largest bounded collections is measured in
// `tests_benchmarks.rs` (`benchmark_envelope_read_views_at_cap`).

/// The declared cap for `name` in [`RESOURCE_ENVELOPE`], if present.
fn envelope_cap(name: &str) -> Option<u32> {
    RESOURCE_ENVELOPE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, cap)| *cap)
}

// ── Success: the declaration matches the constants that enforce it ─────────

#[test]
fn test_resource_envelope_declares_every_bounded_collection() {
    assert_eq!(envelope_cap("fund_queue_entries"), Some(MAX_FUND_QUEUE_LEN));
    assert_eq!(
        envelope_cap("queue_resolution_tie_fanout"),
        Some(MAX_FUND_QUEUE_LEN),
        "the resolution tie-scan loop is bounded by the same queue cap (Issue #1058)"
    );
    assert_eq!(
        envelope_cap("unresolved_queue_retained_entries"),
        Some(MAX_FUND_QUEUE_LEN),
        "the retained unresolved-queue footprint is bounded by the same queue cap (Issue #1059)"
    );
    assert_eq!(
        envelope_cap("price_sources_per_feed"),
        Some(MAX_PRICE_SOURCES_PER_FEED)
    );
    assert_eq!(
        envelope_cap("identity_oracles_per_feed"),
        Some(MAX_IDENTITY_ORACLES_PER_FEED)
    );
    assert_eq!(
        envelope_cap("fee_tiers_entries"),
        Some(MAX_FEE_TIERS),
        "the fee-tier list is bounded by MAX_FEE_TIERS (Issue #1061)"
    );
    assert_eq!(
        envelope_cap("admin_action_log_entries"),
        Some(ADMIN_ACTION_LOG_CAPACITY)
    );
    assert_eq!(
        envelope_cap("governance_log_read_fanout"),
        Some(ADMIN_ACTION_LOG_CAPACITY),
        "the governance-log read loop is bounded by the same log cap (Issue #1060)"
    );
    assert_eq!(
        envelope_cap("top_payers_entries"),
        Some(TOP_PAYERS_CAPACITY)
    );
    assert_eq!(
        envelope_cap("twap_samples_per_feed_token"),
        Some(MAX_TWAP_SAMPLES)
    );
    assert_eq!(
        envelope_cap("ttl_upgrade_sample"),
        Some(TTL_UPGRADE_SAMPLE_SIZE as u32)
    );
    assert_eq!(
        envelope_cap("rollback_sample"),
        Some(ROLLBACK_SAMPLE_SIZE as u32)
    );
}

#[test]
fn test_resource_envelope_entries_are_well_formed() {
    for (i, (name, cap)) in RESOURCE_ENVELOPE.iter().enumerate() {
        assert!(!name.is_empty(), "envelope entry {i} has an empty name");
        assert!(
            *cap > 0,
            "envelope entry '{name}' has a non-positive cap ({cap}) — that is not a bound"
        );
        for (other_name, _) in RESOURCE_ENVELOPE.iter().skip(i + 1) {
            assert_ne!(
                *name, *other_name,
                "duplicate envelope entry '{name}' — one collection, one bound"
            );
        }
    }
}

// ── Boundary: a live collection sits at exactly its declared cap ───────────

#[test]
fn test_fund_queue_live_length_respects_declared_envelope() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let declared =
        envelope_cap("fund_queue_entries").expect("envelope must declare the fund-queue bound");
    assert_eq!(declared, MAX_FUND_QUEUE_LEN);

    fill_fund_queue_to_max(&t, id);
    assert_eq!(
        fund_queue_len(&t, id),
        declared,
        "a full queue must hold exactly its declared envelope cap"
    );

    // The bound holds across a ledger boundary too — advancing the ledger does
    // not widen it.
    advance_ledgers(&t.env, DEFAULT_RATE_LIMIT_LEDGERS as u32);

    // Failure: one more equal-scored joiner is rejected, so the live length can
    // never exceed the declared cap.
    let extra = Address::generate(&t.env);
    let res = t.contract.try_join_fund_queue(&extra, &id);
    assert_eq!(res, Err(Ok(ContractError::QueueFull)));
    assert_eq!(fund_queue_len(&t, id), declared);
}

// ── Authorization: privilege does not bypass the envelope ──────────────────

#[test]
fn test_envelope_cap_holds_even_with_all_auths() {
    let t = setup();
    // Fully permissive auth: an *authorised* admin could otherwise mutate any
    // admin-gated state. The declared cap must still hold.
    t.env.mock_all_auths();

    fill_price_sources_to_max(&t, OracleFeedType::Price);
    let extra = Address::generate(&t.env);
    let res = t
        .contract
        .try_add_price_source(&OracleFeedType::Price, &extra);
    assert_eq!(
        res,
        Err(Ok(ContractError::TooManyPriceSources)),
        "an authorised admin must not be able to exceed a declared envelope cap"
    );
    assert_eq!(
        t.contract
            .get_price_sources(&OracleFeedType::Price)
            .len(),
        envelope_cap("price_sources_per_feed").unwrap()
    );
}

// ================================================================
// Issue #1060 — test governance ledger-entry budget
// ================================================================
//
// # Before / after
//
// **Before:** every governance (admin) entry point calls
// `access::record_admin_action`, which appends to the `AdminActionLog(u32)`
// ring buffer. The *stored* slots were bounded by `ADMIN_ACTION_LOG_CAPACITY`
// (declared as `admin_action_log_entries` in `storage::RESOURCE_ENVELOPE`), but
// two things were untested and one was undeclared: (1) nothing proved the
// retained slot count actually saturates at the cap however many governance
// actions execute, (2) nothing proved the caller-supplied `limit` on
// `get_recent_admin_actions` is clamped so a single read cannot fan out past
// the cap, and (3) the *read loop* bound — `get_recent_admin_actions` touches
// at most one ledger entry per retained slot — was not declared as a
// per-invocation budget, even though a caller can pass an arbitrarily large
// `limit`.
//
// **After:** the governance audit log's ledger-entry budget is declared as the
// `governance_log_read_fanout` entry in `storage::RESOURCE_ENVELOPE` (referencing
// `access::ADMIN_ACTION_LOG_CAPACITY`) and pinned by the tests below: the
// retained slot count saturates at exactly the cap and never grows with action
// volume; the read fan-out is clamped to the cap no matter how large a `limit`
// is passed; reads are newest-first; the wrap-boundary window is correct;
// records capture the ledger sequence across a ledger boundary; and an
// unauthorized caller consumes none of the budget. The per-action write and the
// clamped read cost are measured in `tests_benchmarks.rs`
// (`benchmark_governance_ledger_entry_budget`).

/// Append `n` synthetic governance actions to the admin-action ring buffer,
/// exactly as a governance entry point would via `access::record_admin_action`.
fn record_governance_actions(t: &TestEnv, n: u32) {
    t.env.as_contract(&t.contract.address, || {
        for _ in 0..n {
            crate::access::record_admin_action(&t.env, "gov");
        }
    });
}

/// Whether the ring-buffer slot `slot` currently holds a record.
fn admin_log_slot_present(t: &TestEnv, slot: u32) -> bool {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .has(&StorageKey::AdminActionLog(slot))
    })
}

/// The number of `AdminActionLog(u32)` ledger entries currently retained. This
/// is the governance ledger-entry budget actually consumed — it must never
/// exceed `ADMIN_ACTION_LOG_CAPACITY` however many actions have executed.
fn admin_log_retained_slots(t: &TestEnv) -> u32 {
    let mut present = 0;
    for slot in 0..ADMIN_ACTION_LOG_CAPACITY {
        if admin_log_slot_present(t, slot) {
            present += 1;
        }
    }
    present
}

/// The monotonic total action count (`AdminActionCount`), which grows without
/// bound even though the retained slots do not.
fn admin_action_count(t: &TestEnv) -> u64 {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .instance()
            .get::<StorageKey, u64>(&StorageKey::AdminActionCount)
            .unwrap_or(0)
    })
}

// ── Success: the per-action write budget and the retained-slot budget ──────

#[test]
fn test_1060_one_governance_action_spends_exactly_one_log_entry() {
    let t = setup();
    assert_eq!(admin_log_retained_slots(&t), 0, "log starts empty");

    record_governance_actions(&t, 1);

    assert_eq!(
        admin_log_retained_slots(&t),
        1,
        "one governance action writes exactly one retained log entry"
    );
    assert_eq!(admin_action_count(&t), 1);
    let log = t.contract.get_recent_admin_actions(&1);
    assert_eq!(log.len(), 1);
    assert_eq!(log.get(0).unwrap().seq, 0);
}

#[test]
fn test_1060_retained_log_entries_saturate_at_the_declared_budget() {
    let t = setup();
    let cap = ADMIN_ACTION_LOG_CAPACITY;

    // Exactly at the budget: `cap` actions, `cap` retained entries.
    record_governance_actions(&t, cap);
    assert_eq!(admin_log_retained_slots(&t), cap);
    assert_eq!(admin_action_count(&t), cap as u64);

    // Past the budget: 25 more actions overwrite the oldest slots, so the
    // retained count stays pinned at the cap — it never grows with volume.
    record_governance_actions(&t, 25);
    assert_eq!(
        admin_log_retained_slots(&t),
        cap,
        "the governance log's ledger-entry budget must not grow past the cap"
    );
    assert_eq!(admin_action_count(&t), (cap + 25) as u64);
}

#[test]
fn test_1060_read_returns_newest_first_within_the_budget() {
    let t = setup();
    record_governance_actions(&t, 5);

    let log = t.contract.get_recent_admin_actions(&5);
    assert_eq!(log.len(), 5);
    for i in 0..5u32 {
        assert_eq!(
            log.get(i).unwrap().seq,
            (4 - i) as u64,
            "entries must be newest-first"
        );
    }
}

#[test]
fn test_1060_records_capture_the_ledger_sequence_across_a_boundary() {
    let t = setup();

    record_governance_actions(&t, 1);
    advance_ledgers(&t.env, DEFAULT_RATE_LIMIT_LEDGERS as u32);
    record_governance_actions(&t, 1);

    let log = t.contract.get_recent_admin_actions(&2);
    let newest = log.get(0).unwrap();
    let oldest = log.get(1).unwrap();
    assert_eq!(newest.seq, 1);
    assert_eq!(oldest.seq, 0);
    assert!(
        newest.ledger > oldest.ledger,
        "records recorded across a ledger boundary must carry the distinct ledger sequences"
    );
}

// ── Failure: the read fan-out is clamped to the budget ─────────────────────

#[test]
fn test_1060_read_fanout_is_clamped_to_the_budget() {
    let t = setup();
    // Fill the log and then wrap it, so `total` exceeds the cap.
    record_governance_actions(&t, ADMIN_ACTION_LOG_CAPACITY + 3);

    // An oversized `limit` cannot make a single read touch more than the cap.
    let oversized = t.contract.get_recent_admin_actions(&u32::MAX);
    assert_eq!(
        oversized.len(),
        ADMIN_ACTION_LOG_CAPACITY,
        "a read must never fan out past the declared budget, whatever `limit` is passed"
    );

    // A limit just above the cap is clamped identically.
    let just_over = t.contract.get_recent_admin_actions(&(ADMIN_ACTION_LOG_CAPACITY + 10));
    assert_eq!(just_over.len(), ADMIN_ACTION_LOG_CAPACITY);

    // A limit below the cap is honoured exactly (no over-read).
    let small = t.contract.get_recent_admin_actions(&7);
    assert_eq!(small.len(), 7);
}

#[test]
fn test_1060_wrap_boundary_window_is_correct() {
    let t = setup();
    let cap = ADMIN_ACTION_LOG_CAPACITY;

    // One past the budget: seq 0's slot has been overwritten by seq `cap`, so
    // the full window runs `cap` down to 1 (seq 0 is gone).
    record_governance_actions(&t, cap + 1);

    let log = t.contract.get_recent_admin_actions(&cap);
    assert_eq!(log.len(), cap);
    assert_eq!(log.get(0).unwrap().seq, cap as u64, "newest is seq = cap");
    assert_eq!(
        log.get(cap - 1).unwrap().seq,
        1,
        "the oldest surviving entry is seq 1 — seq 0's slot was overwritten"
    );
}

#[test]
fn test_1060_empty_log_reads_nothing_and_spends_no_budget() {
    let t = setup();

    let log = t.contract.get_recent_admin_actions(&ADMIN_ACTION_LOG_CAPACITY);
    assert_eq!(log.len(), 0, "an empty log returns nothing");
    assert_eq!(admin_log_retained_slots(&t), 0);
    assert_eq!(admin_action_count(&t), 0);
}

// ── Authorization: an unauthorized caller spends no budget ─────────────────

#[test]
fn test_1060_unauthorized_governance_action_consumes_no_budget() {
    let t = setup();
    // Strip all authorization: the admin-gated entry point must fail, and a
    // failed governance action must not append to the audit log.
    t.env.mock_auths(&[]);

    assert!(
        t.contract.try_update_fee_rate(&100).is_err(),
        "an unauthorized governance action must fail"
    );
    assert_eq!(
        admin_log_retained_slots(&t),
        0,
        "a rejected governance action must consume none of the ledger-entry budget"
    );
    assert_eq!(admin_action_count(&t), 0);
}

// ================================================================
// Issue #1061 — reject operations exceeding resource budgets
// ================================================================
//
// # Before / after
//
// **Before:** `update_fee_tiers` stored an arbitrary-length `Vec<(i128, u32)>`
// in `StorageKey::FeeTiers` and `effective_fee_rate` walked that whole vector
// on every settlement (`mark_paid`) to find the applicable bracket. Nothing
// bounded the list, and it was not declared in `storage::RESOURCE_ENVELOPE`, so
// an unbounded tier configuration made the per-invocation cost of an
// *unrelated* entry point — settlement — unbounded: governance (or a
// compromised admin key) could grow the list until `mark_paid` exceeded
// Soroban's per-transaction resource limits and settlement failed for every
// invoice.
//
// **After:** the tier list is bounded by `constants::MAX_FEE_TIERS`, declared
// as the `fee_tiers_entries` entry in `storage::RESOURCE_ENVELOPE`, and
// `update_fee_tiers` rejects a configuration larger than the budget with
// `TooManyFeeTiers` (checked after `require_admin`, before the rate-limit
// gate). The tests below pin the boundary (exactly at the budget is accepted,
// one over is rejected without overwriting the stored configuration),
// authorization, error precedence, and the rate-limit ledger boundary, and
// prove a full tier list still settles. The cost of the write and of the
// settlement tier-walk at the cap is measured in `tests_benchmarks.rs`
// (`benchmark_fee_tier_budget`).

/// Build a well-formed (ascending `min_amount`) fee-tier list of length `n`.
fn fee_tiers(t: &TestEnv, n: u32) -> soroban_sdk::Vec<(i128, u32)> {
    let mut tiers = soroban_sdk::Vec::new(&t.env);
    for i in 0..n {
        tiers.push_back(((i as i128 + 1) * 1_000_000, 100 + i));
    }
    tiers
}

// ── Success / boundary: exactly at the budget is accepted ──────────────────

#[test]
fn test_1061_fee_tiers_at_the_declared_budget_are_accepted() {
    let t = setup();
    let cap = MAX_FEE_TIERS;

    t.contract.update_fee_tiers(&fee_tiers(&t, cap));

    assert_eq!(
        t.contract.get_fee_tiers().len(),
        cap,
        "a configuration exactly at the declared budget must be accepted and stored"
    );
}

// ── Failure / boundary: one over the budget is rejected ────────────────────

#[test]
fn test_1061_fee_tiers_one_over_the_budget_are_rejected() {
    let t = setup();
    let cap = MAX_FEE_TIERS;

    // Seed a valid configuration so a rejected write can be shown to leave it
    // intact.
    t.contract.update_fee_tiers(&fee_tiers(&t, 3));
    assert_eq!(t.contract.get_fee_tiers().len(), 3);

    assert_eq!(
        t.contract.try_update_fee_tiers(&fee_tiers(&t, cap + 1)),
        Err(Ok(ContractError::TooManyFeeTiers)),
        "a tier list one over the declared budget must be rejected"
    );
    assert_eq!(
        t.contract.get_fee_tiers().len(),
        3,
        "a rejected configuration must not overwrite the stored tiers"
    );
}

// ── Authorization: a non-admin cannot reach the write ──────────────────────

#[test]
fn test_1061_unauthorized_write_consumes_no_config() {
    let t = setup();
    t.env.mock_auths(&[]);

    assert!(
        t.contract.try_update_fee_tiers(&fee_tiers(&t, 3)).is_err(),
        "a non-admin caller must not be able to write the fee tiers"
    );
    assert_eq!(
        t.contract.get_fee_tiers().len(),
        0,
        "an unauthorized governance action must not store a configuration"
    );
}

// ── Error precedence: the budget check runs before the rate limit ──────────

#[test]
fn test_1061_budget_check_precedes_the_rate_limit() {
    let t = setup();

    // A valid write consumes the rate-limit window.
    t.contract.update_fee_tiers(&fee_tiers(&t, 2));

    // Within the cooldown a *valid* write is rate-limited...
    assert_eq!(
        t.contract.try_update_fee_tiers(&fee_tiers(&t, 2)),
        Err(Ok(ContractError::RateLimited))
    );

    // ...but an oversized write is still reported as `TooManyFeeTiers`, because
    // structural validation precedes the state-affecting rate-limit gate.
    assert_eq!(
        t.contract.try_update_fee_tiers(&fee_tiers(&t, MAX_FEE_TIERS + 1)),
        Err(Ok(ContractError::TooManyFeeTiers))
    );
}

// ── Ledger boundary: the rate-limit budget reopens at the exact cooldown ────

#[test]
fn test_1061_rate_limit_reopens_across_the_ledger_boundary() {
    let t = setup();

    t.contract.update_fee_tiers(&fee_tiers(&t, 2));

    // One ledger short of the economic-parameter cooldown: still rate-limited.
    advance_ledgers(&t.env, ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32 - 1);
    assert_eq!(
        t.contract.try_update_fee_tiers(&fee_tiers(&t, 2)),
        Err(Ok(ContractError::RateLimited))
    );

    // Exactly at the boundary: the next valid configuration is accepted.
    advance_ledgers(&t.env, 1);
    t.contract.update_fee_tiers(&fee_tiers(&t, 3));
    assert_eq!(t.contract.get_fee_tiers().len(), 3);
}

// ── Success on the hot path: a full tier list still settles ────────────────

#[test]
fn test_1061_settlement_tier_walk_is_bounded_at_the_cap() {
    let t = setup();

    // A full tier list must not break the settlement path it feeds.
    t.contract.update_fee_tiers(&fee_tiers(&t, MAX_FEE_TIERS));

    let id = submit_standard_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    assert_eq!(
        t.contract.get_invoice(&id).status,
        InvoiceStatus::Paid,
        "settlement must still succeed with the fee-tier list at its declared budget"
    );
}
