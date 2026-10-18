#![cfg(test)]

use super::*;
use crate::access::ADMIN_ACTION_LOG_CAPACITY;
use crate::config::Config;
use crate::constants::{
    DEFAULT_RATE_LIMIT_LEDGERS, FUND_QUEUE_TTL_EXTEND_TO, FUND_QUEUE_TTL_THRESHOLD,
    MAX_FUND_QUEUE_LEN, MAX_IDENTITY_ORACLES_PER_FEED, MAX_PRICE_SOURCES_PER_FEED,
    ROLLBACK_SAMPLE_SIZE, TOP_PAYERS_CAPACITY, TTL_MIN_LEDGERS_BEFORE_UPGRADE,
    TTL_UPGRADE_SAMPLE_SIZE,
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
        envelope_cap("price_sources_per_feed"),
        Some(MAX_PRICE_SOURCES_PER_FEED)
    );
    assert_eq!(
        envelope_cap("identity_oracles_per_feed"),
        Some(MAX_IDENTITY_ORACLES_PER_FEED)
    );
    assert_eq!(
        envelope_cap("admin_action_log_entries"),
        Some(ADMIN_ACTION_LOG_CAPACITY)
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
