#![cfg(test)]

use proptest::prelude::*;
use reputation_bonus::config::Config;
use reputation_bonus::{ReputationBonusContract, ReputationBonusContractClient};
use soroban_sdk::{testutils::Address as _, Address, Env};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepBonusEvent {
    Submit,
    Pay,
    Default,
}

fn rep_bonus_event_strategy() -> impl Strategy<Value = RepBonusEvent> {
    prop_oneof![
        Just(RepBonusEvent::Submit),
        Just(RepBonusEvent::Pay),
        Just(RepBonusEvent::Default),
    ]
}

fn setup_reputation_bonus_env(
    env: &Env,
) -> (ReputationBonusContractClient<'static>, Address, Address) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let contract_id = env.register_contract(None, ReputationBonusContract);
    let client = ReputationBonusContractClient::new(env, &contract_id);
    client.init(&admin);

    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
    };
    client.set_config(&config);

    let freelancer = Address::generate(env);
    let payer = Address::generate(env);

    (client, freelancer, payer)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn test_reputation_bonus_score_bounds_and_invariants(
        events in prop::collection::vec(rep_bonus_event_strategy(), 1..100),
    ) {
        let env = Env::default();
        let (client, freelancer, payer) = setup_reputation_bonus_env(&env);

        let mut pending_invoices = std::vec::Vec::new();

        for event in events {
            match event {
                RepBonusEvent::Submit => {
                    let inv = client.submit_invoice(
                        &freelancer,
                        &payer,
                        &1000,
                        &1800000000,
                        &500,
                    );
                    pending_invoices.push(inv.id);
                }
                RepBonusEvent::Pay => {
                    let id = if let Some(invoice_id) = pending_invoices.pop() {
                        invoice_id
                    } else {
                        let inv = client.submit_invoice(
                            &freelancer,
                            &payer,
                            &1000,
                            &1800000000,
                            &500,
                        );
                        inv.id
                    };
                    let _ = client.mark_paid(&id);
                }
                RepBonusEvent::Default => {
                    let id = if let Some(invoice_id) = pending_invoices.pop() {
                        invoice_id
                    } else {
                        let inv = client.submit_invoice(
                            &freelancer,
                            &payer,
                            &1000,
                            &1800000000,
                            &500,
                        );
                        inv.id
                    };
                    let _ = client.handle_default(&id);
                }
            }

            // Assert reputation invariants for both parties
            for addr in &[&freelancer, &payer] {
                let rep = client.get_reputation(addr);
                prop_assert!(rep.score <= 100, "Reputation score exceeded 100: {}", rep.score);
                // score: u32 — cannot go negative; no >= 0 assertion needed (clippy absurd_extreme_comparisons)

                // Detailed sanity check on score calculation
                let expected_score = if rep.invoices_submitted > 0 {
                    let calculated = (rep.invoices_paid as u64)
                        .saturating_mul(100)
                        .saturating_div(rep.invoices_submitted as u64) as u32;
                    calculated.min(100)
                } else {
                    0
                };
                prop_assert_eq!(rep.score, expected_score, "Calculated score does not match stored score");
            }
        }
    }
}
