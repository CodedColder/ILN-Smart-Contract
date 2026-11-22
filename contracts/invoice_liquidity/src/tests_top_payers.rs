#![cfg(test)]

//! Audit: top-payer ranking determinism (`top_payers.rs`).
//!
//! Documented model (see docs/top-payers-audit.md):
//!  - The ranking order is total: descending score, ties broken by ascending
//!    address. The result never depends on insertion/update history.
//!  - The heap holds at most `TOP_PAYERS_CAPACITY` unique addresses; a full
//!    heap admits a newcomer only if it strictly outranks the lowest entry.
//!  - Replaying the same score update is idempotent.
//!  - The heap follows score changes made through `set_payer_score` *and*
//!    lazy reputation decay.
//!  - Residual: the heap only learns about payers when *their* score changes,
//!    so an evicted payer is not promoted when a member later drops.

use super::*;
use crate::constants::TOP_PAYERS_CAPACITY;
use crate::test::setup;
use crate::top_payers::{get_top_payers, get_top_payers_heap, update_top_payers_on_score_change};
use soroban_sdk::{testutils::{Address as _, Ledger}, Address, Env, Vec};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

fn new_contract(env: &Env) -> Address {
    env.register(InvoiceLiquidityContract, ())
}

fn addrs(env: &Env, n: u32) -> Vec<Address> {
    let mut v = Vec::new(env);
    for _ in 0..n {
        v.push_back(Address::generate(env));
    }
    v
}

fn ranked(env: &Env, contract: &Address, limit: u32) -> Vec<TopPayerEntry> {
    env.as_contract(contract, || get_top_payers(env, limit))
}

fn contains(list: &Vec<TopPayerEntry>, a: &Address) -> bool {
    list.iter().any(|e| &e.address == a)
}

fn assert_strictly_ranked(list: &Vec<TopPayerEntry>) {
    for i in 1..list.len() {
        let hi = list.get(i - 1).unwrap();
        let lo = list.get(i).unwrap();
        assert!(
            hi.score > lo.score || (hi.score == lo.score && hi.address < lo.address),
            "entries {} and {} violate (score desc, address asc)",
            i - 1,
            i
        );
    }
}

// ── Tie-breaking ─────────────────────────────────────────────────────────────

#[test]
fn test_equal_scores_rank_by_address_regardless_of_insertion_order() {
    let env = Env::default();
    let payers = addrs(&env, 6);
    let c1 = new_contract(&env);
    let c2 = new_contract(&env);
    let c3 = new_contract(&env);

    env.as_contract(&c1, || {
        for p in payers.iter() {
            update_top_payers_on_score_change(&env, &p, 70);
        }
    });
    env.as_contract(&c2, || {
        for p in payers.iter().rev() {
            update_top_payers_on_score_change(&env, &p, 70);
        }
    });
    env.as_contract(&c3, || {
        for i in 0..payers.len() {
            let p = payers.get((i + 3) % payers.len()).unwrap();
            update_top_payers_on_score_change(&env, &p, 70);
        }
    });

    let r1 = ranked(&env, &c1, 10);
    assert_eq!(r1.len(), 6);
    assert_strictly_ranked(&r1);
    assert_eq!(r1, ranked(&env, &c2, 10));
    assert_eq!(r1, ranked(&env, &c3, 10));
}

#[test]
fn test_higher_score_beats_lower_address() {
    let env = Env::default();
    let c = new_contract(&env);
    let payers = addrs(&env, 2);
    let (a, b) = (payers.get(0).unwrap(), payers.get(1).unwrap());
    let (low_addr, high_addr) = if a < b { (a, b) } else { (b, a) };

    env.as_contract(&c, || {
        update_top_payers_on_score_change(&env, &low_addr, 40);
        update_top_payers_on_score_change(&env, &high_addr, 41);
    });

    let r = ranked(&env, &c, 2);
    assert_eq!(r.get(0).unwrap().address, high_addr);
    assert_eq!(r.get(1).unwrap().address, low_addr);
}

#[test]
fn test_full_heap_tie_eviction_is_independent_of_arrival_order() {
    // CAP + 1 payers, all tied. Whatever the arrival order, the one left out is
    // the highest address (the lowest-ranked under the tie-break).
    let env = Env::default();
    let payers = addrs(&env, TOP_PAYERS_CAPACITY + 1);
    let fwd = new_contract(&env);
    let rev = new_contract(&env);

    env.as_contract(&fwd, || {
        for p in payers.iter() {
            update_top_payers_on_score_change(&env, &p, 10);
        }
    });
    env.as_contract(&rev, || {
        for p in payers.iter().rev() {
            update_top_payers_on_score_change(&env, &p, 10);
        }
    });

    let mut highest = payers.get(0).unwrap();
    for p in payers.iter() {
        if p > highest {
            highest = p;
        }
    }

    let a = ranked(&env, &fwd, TOP_PAYERS_CAPACITY);
    let b = ranked(&env, &rev, TOP_PAYERS_CAPACITY);
    assert_eq!(a.len(), TOP_PAYERS_CAPACITY);
    assert!(!contains(&a, &highest), "highest address must be the one evicted");
    assert_eq!(a, b, "result must not depend on arrival order");
    assert_strictly_ranked(&a);
}

// ── Replay / repeated events ─────────────────────────────────────────────────

#[test]
fn test_replayed_identical_update_is_idempotent() {
    let env = Env::default();
    let c = new_contract(&env);
    let payers = addrs(&env, 4);

    env.as_contract(&c, || {
        for (i, p) in payers.iter().enumerate() {
            update_top_payers_on_score_change(&env, &p, 20 + i as u32 * 10);
        }
    });
    let before = ranked(&env, &c, 10);

    env.as_contract(&c, || {
        for _ in 0..3 {
            for (i, p) in payers.iter().enumerate() {
                update_top_payers_on_score_change(&env, &p, 20 + i as u32 * 10);
            }
        }
    });

    assert_eq!(ranked(&env, &c, 10), before);
    assert_eq!(env.as_contract(&c, || get_top_payers_heap(&env).len()), 4);
}

#[test]
fn test_repeated_updates_never_duplicate_a_payer() {
    let env = Env::default();
    let c = new_contract(&env);
    let p = Address::generate(&env);

    env.as_contract(&c, || {
        for s in [10u32, 90, 10, 55, 55, 0, 100] {
            update_top_payers_on_score_change(&env, &p, s);
        }
        let heap = get_top_payers_heap(&env);
        assert_eq!(heap.len(), 1);
        assert_eq!(heap.get(0).unwrap().score, 100);
    });
}

#[test]
fn test_score_decrease_and_increase_reorders_correctly() {
    let env = Env::default();
    let c = new_contract(&env);
    let payers = addrs(&env, 3);
    let (a, b, d) = (
        payers.get(0).unwrap(),
        payers.get(1).unwrap(),
        payers.get(2).unwrap(),
    );

    env.as_contract(&c, || {
        update_top_payers_on_score_change(&env, &a, 90);
        update_top_payers_on_score_change(&env, &b, 60);
        update_top_payers_on_score_change(&env, &d, 30);
        update_top_payers_on_score_change(&env, &a, 10); // top drops to bottom
        update_top_payers_on_score_change(&env, &d, 95); // bottom jumps to top
    });

    let r = ranked(&env, &c, 3);
    assert_eq!(r.get(0).unwrap().address, d);
    assert_eq!(r.get(1).unwrap().address, b);
    assert_eq!(r.get(2).unwrap().address, a);
}

// ── Bounds / overflow ────────────────────────────────────────────────────────

#[test]
fn test_heap_never_exceeds_capacity_and_keeps_the_best() {
    let env = Env::default();
    let c = new_contract(&env);
    let payers = addrs(&env, TOP_PAYERS_CAPACITY + 20);

    env.as_contract(&c, || {
        for (i, p) in payers.iter().enumerate() {
            // Distinct scores 1..=CAP+20 (all within u32).
            update_top_payers_on_score_change(&env, &p, i as u32 + 1);
        }
        assert_eq!(get_top_payers_heap(&env).len(), TOP_PAYERS_CAPACITY);
    });

    let r = ranked(&env, &c, TOP_PAYERS_CAPACITY);
    assert_eq!(r.get(0).unwrap().score, TOP_PAYERS_CAPACITY + 20);
    assert_eq!(r.get(TOP_PAYERS_CAPACITY - 1).unwrap().score, 21);
    assert_strictly_ranked(&r);
}

#[test]
fn test_extreme_scores_and_limits_do_not_overflow() {
    let env = Env::default();
    let c = new_contract(&env);
    let payers = addrs(&env, 3);

    env.as_contract(&c, || {
        update_top_payers_on_score_change(&env, &payers.get(0).unwrap(), u32::MAX);
        update_top_payers_on_score_change(&env, &payers.get(1).unwrap(), 0);
        update_top_payers_on_score_change(&env, &payers.get(2).unwrap(), u32::MAX - 1);
    });

    let r = ranked(&env, &c, u32::MAX);
    assert_eq!(r.len(), 3, "limit is clamped to the heap size");
    assert_eq!(r.get(0).unwrap().score, u32::MAX);
    assert_eq!(r.get(1).unwrap().score, u32::MAX - 1);
    assert_eq!(r.get(2).unwrap().score, 0);

    assert_eq!(ranked(&env, &c, 0).len(), 0);
    assert_eq!(ranked(&env, &new_contract(&env), 5).len(), 0, "empty heap");
}

// ── Stale profiles ───────────────────────────────────────────────────────────

#[test]
fn test_idle_decay_is_reflected_in_the_ranking() {
    // Lazy decay rewrites the payer's score on read. The ranking must follow,
    // otherwise an idle payer stays ranked at its pre-decay score.
    let t = setup();
    let payer = t.payer.clone();
    let id = t.contract.submit_invoice(
        &t.freelancer,
        &payer,
        &INVOICE_AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let before = t.contract.payer_score(&payer);
    let ranked_before = t.contract.get_top_payers(&10);
    assert_eq!(ranked_before.get(0).unwrap().address, payer);
    assert_eq!(ranked_before.get(0).unwrap().score, before);

    let mut info = t.env.ledger().get();
    info.sequence_number += (365 * 24 * 60 * 60 / 5) as u32;
    t.env.ledger().set(info);

    let after = t.contract.payer_score(&payer);
    assert!(after < before, "precondition: score decayed");
    let ranked_after = t.contract.get_top_payers(&10);
    assert_eq!(ranked_after.get(0).unwrap().score, after);
}

#[test]
fn test_payer_unchanged_from_default_is_not_tracked() {
    // The heap is fed by score *changes*; a payer that never moved off the
    // default score has no entry.
    let t = setup();
    assert_eq!(t.contract.get_top_payers(&10).len(), 0);
    let _ = t.contract.payer_score(&t.payer);
    assert_eq!(t.contract.get_top_payers(&10).len(), 0);
}

// ── Residual risk: evicted payers are not re-promoted ────────────────────────

#[test]
fn test_evicted_payer_is_not_promoted_when_a_member_drops() {
    // Pinned limitation. The heap only sees a payer when that payer's own
    // score changes. An outsider rejected for being below the minimum stays
    // out even after a member later falls beneath the outsider's score.
    let env = Env::default();
    let c = new_contract(&env);
    let members = addrs(&env, TOP_PAYERS_CAPACITY);
    let outsider = Address::generate(&env);

    env.as_contract(&c, || {
        for (i, m) in members.iter().enumerate() {
            update_top_payers_on_score_change(&env, &m, 11 + i as u32); // 11..=60
        }
        update_top_payers_on_score_change(&env, &outsider, 5); // rejected
    });
    assert!(!contains(&ranked(&env, &c, TOP_PAYERS_CAPACITY), &outsider));

    // The lowest member drops to 0 — below the outsider's 5.
    env.as_contract(&c, || {
        update_top_payers_on_score_change(&env, &members.get(0).unwrap(), 0);
    });

    let r = ranked(&env, &c, TOP_PAYERS_CAPACITY);
    assert_eq!(r.len(), TOP_PAYERS_CAPACITY);
    assert!(contains(&r, &members.get(0).unwrap()));
    assert!(
        !contains(&r, &outsider),
        "known limitation: outsider (5) is not promoted over the 0-score member"
    );
}
