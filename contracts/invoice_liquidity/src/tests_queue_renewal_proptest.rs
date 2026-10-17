//! Issue #1018 — Repeated queue renewal property tests
//!
//! `tests_queue_archival.rs` (Issue #1012) proves the TTL-renewal guarantee
//! (Issue #1011) and the fail-closed archival guard (Issue #1013) against a
//! small number of fixed, hand-picked scenarios. This file generalizes both
//! into properties that hold across arbitrarily long, randomly-spaced
//! sequences of LP joins — because the actual guarantee being audited isn't
//! "renewal works for *this* sequence of joins," it's "renewal works no
//! matter how the joins are spaced out in time."
//!
//! # Before / after baseline
//!
//! **Before (#1011):** `try_set_fund_queue_opened_at` never called
//! `extend_ttl`, so *no* sequence of joins — however frequent — protected
//! `FundQueueOpenedAt` from drifting toward the default persistent TTL and
//! being archived. **After:** every join renews the key's TTL back to at
//! least `FUND_QUEUE_TTL_THRESHOLD`, so as long as consecutive joins are
//! spaced closer together than that threshold, the key provably never runs
//! out — `test_repeated_queue_renewal_never_loses_opened_at_entry` checks
//! this property directly rather than asserting it for one hand-picked
//! spacing.
//!
//! **Before (#1013):** a lost `FundQueueOpenedAt` silently skipped the MEV
//! maturity gate, regardless of how much renewal history existed before the
//! loss. **After:** `resolve_fund_queue` fails closed with
//! `QueueStateArchived` — `test_archived_queue_fails_closed_regardless_of_renewal_history`
//! checks that this holds no matter how many prior renewals occurred or how
//! long resolution is delayed after the record is lost.

#![cfg(test)]

use super::*;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

use crate::constants::FUND_QUEUE_TTL_THRESHOLD;

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days

// Stays well under MAX_FUND_QUEUE_LEN (50) so every scheduled join succeeds
// on its own merits — this file is about TTL renewal timing, not queue
// capacity, which other tests already cover.
const MAX_JOINS: usize = 40;

fn setup_renewal_env(
    env: &Env,
) -> (
    InvoiceLiquidityContractClient<'static>,
    TokenClient<'static>,
    Address,
    Address,
    std::vec::Vec<Address>,
) {
    env.mock_all_auths();

    let usdc_admin = Address::generate(env);
    let usdc_id = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let usdc_addr = usdc_id.address();
    let token = TokenClient::new(env, &usdc_addr);
    let token_admin = StellarAssetClient::new(env, &usdc_addr);

    let freelancer = Address::generate(env);
    let payer = Address::generate(env);
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(env, &contract_id);
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let xlm_admin = Address::generate(env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_addr = xlm_id.address();
    let eurc_addr = Address::generate(env);

    contract.initialize(&usdc_admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    // Distinct LPs, one per potential join, so AlreadyInQueue never fires.
    let lps: std::vec::Vec<Address> = (0..MAX_JOINS).map(|_| Address::generate(env)).collect();

    (contract, token, freelancer, payer, lps)
}

fn advance_ledgers(env: &Env, delta: u32) {
    let mut info = env.ledger().get();
    info.sequence_number = info.sequence_number.saturating_add(delta);
    info.timestamp = info.timestamp.saturating_add(u64::from(delta) * 5);
    env.ledger().set(info);
}

fn fund_queue_opened_at_ttl(env: &Env, contract_address: &Address, invoice_id: u64) -> u32 {
    env.as_contract(contract_address, || {
        env.storage()
            .persistent()
            .get_ttl(&StorageKey::FundQueueOpenedAt(invoice_id))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: as long as every gap between consecutive joins is smaller
    /// than `FUND_QUEUE_TTL_THRESHOLD`, `FundQueueOpenedAt` can never
    /// actually be lost. This follows from `extend_ttl`'s own contract —
    /// after any `extend_ttl(threshold, extend_to)` call on a live entry,
    /// its remaining TTL is guaranteed to be >= `threshold` (either it was
    /// already >= threshold and left untouched, or it was below and got
    /// bumped to `extend_to > threshold`). So if the next gap is strictly
    /// less than `threshold`, the entry cannot reach zero before the next
    /// join renews it — no matter how many joins, or how the gaps are
    /// distributed, as long as that per-gap bound holds.
    #[test]
    fn test_repeated_queue_renewal_never_loses_opened_at_entry(
        gaps in prop::collection::vec(0u32..FUND_QUEUE_TTL_THRESHOLD, 1..MAX_JOINS),
    ) {
        let env = Env::default();
        let (contract, token, freelancer, payer, lps) = setup_renewal_env(&env);

        let due_date = env.ledger().timestamp() + DUE_DATE_OFFSET;
        let invoice_id = contract.submit_invoice(
            &freelancer,
            &payer,
            &INVOICE_AMOUNT,
            &due_date,
            &DISCOUNT_RATE,
            &token.address,
            &ReferralCode::None,
        );

        for (i, gap) in gaps.iter().enumerate() {
            if *gap > 0 {
                advance_ledgers(&env, *gap);
            }
            contract.join_fund_queue(&lps[i], &invoice_id);

            // After every single join, no matter how the preceding gaps
            // were distributed, the record must still exist and its TTL
            // must have been renewed back to at least the retention
            // threshold — it must never have been silently lost along the
            // way.
            let opened_at = env.as_contract(&contract.address, || {
                crate::invoice::get_fund_queue_opened_at(&env, invoice_id)
            });
            prop_assert!(
                opened_at.is_some(),
                "FundQueueOpenedAt must survive {} repeated renewals with gaps {:?}",
                i + 1,
                gaps
            );

            let ttl = fund_queue_opened_at_ttl(&env, &contract.address, invoice_id);
            prop_assert!(
                ttl >= FUND_QUEUE_TTL_THRESHOLD,
                "FundQueueOpenedAt TTL must be renewed to >= {} after join {}, got {}",
                FUND_QUEUE_TTL_THRESHOLD,
                i + 1,
                ttl
            );
        }

        // A fully-renewed queue must resolve normally once mature — renewal
        // must never have broken the ordinary success path.
        advance_ledgers(&env, QUEUE_DELAY_LEDGERS);
        let result = contract.try_resolve_fund_queue(&invoice_id);
        prop_assert!(
            result.is_ok(),
            "final resolution must succeed after a fully-renewed queue, got {:?}",
            result
        );
    }

    /// Property: once `FundQueueOpenedAt` is lost on a non-empty queue,
    /// `resolve_fund_queue` fails closed with `QueueStateArchived` — no
    /// matter how many successful renewals (joins) preceded the loss, and
    /// no matter how long resolution is delayed afterward. The fail-closed
    /// guard (#1013) has no "history-dependent" escape hatch.
    #[test]
    fn test_archived_queue_fails_closed_regardless_of_renewal_history(
        gaps in prop::collection::vec(0u32..FUND_QUEUE_TTL_THRESHOLD, 1..MAX_JOINS),
        resolve_delay in 0u32..1_000_000u32,
    ) {
        let env = Env::default();
        let (contract, token, freelancer, payer, lps) = setup_renewal_env(&env);

        let due_date = env.ledger().timestamp() + DUE_DATE_OFFSET;
        let invoice_id = contract.submit_invoice(
            &freelancer,
            &payer,
            &INVOICE_AMOUNT,
            &due_date,
            &DISCOUNT_RATE,
            &token.address,
            &ReferralCode::None,
        );

        for (i, gap) in gaps.iter().enumerate() {
            if *gap > 0 {
                advance_ledgers(&env, *gap);
            }
            contract.join_fund_queue(&lps[i], &invoice_id);
        }

        // Simulate the record being lost, regardless of how many renewals
        // (gaps.len()) happened beforehand.
        env.as_contract(&contract.address, || {
            env.storage()
                .persistent()
                .remove(&StorageKey::FundQueueOpenedAt(invoice_id));
        });

        advance_ledgers(&env, resolve_delay);

        let result = contract.try_resolve_fund_queue(&invoice_id);
        prop_assert_eq!(
            result,
            Err(Ok(ContractError::QueueStateArchived)),
            "resolve_fund_queue must fail closed after {} renewals + {} delay ledgers once FundQueueOpenedAt is missing",
            gaps.len(),
            resolve_delay
        );
    }
}
