#![cfg(test)]

use super::*;
use proptest::prelude::*;
use soroban_sdk::{
    contract, contractimpl, testutils::Address as _, token::Client as TokenClient, Address, Env,
};

#[contract]
pub struct MockIln;

#[contractimpl]
impl MockIln {
    pub fn accrue_lp(env: Env, dist: Address, lp: Address, amount: i128) {
        IlnDistributionClient::new(&env, &dist).accrue_lp(&lp, &amount);
    }

    pub fn accrue_settlement(
        env: Env,
        dist: Address,
        freelancer: Address,
        payer: Address,
        on_time: bool,
    ) {
        IlnDistributionClient::new(&env, &dist).accrue_settlement(&freelancer, &payer, &on_time);
    }
}

#[derive(Debug, Clone, Copy)]
enum DistEvent {
    AccrueLp {
        amount: i128,
    },
    AccrueSettlement {
        on_time: bool,
    },
    ClaimTokens,
    UpdateRates {
        lp_rate: i128,
        freelancer_rate: i128,
        payer_rate: i128,
    },
}

fn dist_event_strategy() -> impl Strategy<Value = DistEvent> {
    prop_oneof![
        (0i128..=2_000_000_000_000i128).prop_map(|amount| DistEvent::AccrueLp { amount }),
        any::<bool>().prop_map(|on_time| DistEvent::AccrueSettlement { on_time }),
        Just(DistEvent::ClaimTokens),
        (
            0i128..=100_000_000i128,
            0i128..=100_000_000i128,
            0i128..=100_000_000i128
        )
            .prop_map(
                |(lp_rate, freelancer_rate, payer_rate)| DistEvent::UpdateRates {
                    lp_rate,
                    freelancer_rate,
                    payer_rate
                }
            ),
    ]
}

fn setup_dist_env(
    env: &Env,
) -> (
    IlnDistributionClient<'static>,
    MockIlnClient<'static>,
    TokenClient<'static>,
    Address,
    Address,
    Address,
) {
    env.mock_all_auths();

    let iln_id = env.register_contract(None, MockIln);
    let dist_id = env.register_contract(None, IlnDistribution);
    let dist = IlnDistributionClient::new(env, &dist_id);
    let iln = MockIlnClient::new(env, &iln_id);

    let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
    let gov_token = gov_token_id.address();
    let token_client = TokenClient::new(env, &gov_token);

    dist.initialize(&iln_id, &gov_token);

    let lp = Address::generate(env);
    let freelancer = Address::generate(env);
    let payer = Address::generate(env);

    (dist, iln, token_client, lp, freelancer, payer)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn test_distribution_rewards_bounds_and_invariants(
        events in prop::collection::vec(dist_event_strategy(), 1..100),
    ) {
        let env = Env::default();
        let (dist, iln, token, lp, freelancer, payer) = setup_dist_env(&env);

        let mut expected_claimed = 0i128;

        for event in events {
            match event {
                DistEvent::AccrueLp { amount } => {
                    iln.accrue_lp(&dist.address, &lp, &amount);
                }
                DistEvent::AccrueSettlement { on_time } => {
                    iln.accrue_settlement(&dist.address, &freelancer, &payer, &on_time);
                }
                DistEvent::ClaimTokens => {
                    let claimed = dist.claim_tokens(&lp);
                    expected_claimed = expected_claimed.saturating_add(claimed);

                    // Assert LP claimed tokens matches balance in gov token contract
                    let balance = token.balance(&lp);
                    prop_assert_eq!(balance, expected_claimed, "Claimed balance does not match token balance");
                }
                DistEvent::UpdateRates { lp_rate, freelancer_rate, payer_rate } => {
                    // Update rates via governance/ILN authority
                    dist.set_lp_reward_rate(&lp_rate);
                    dist.set_freelancer_reward_rate(&freelancer_rate);
                    dist.set_payer_reward_rate(&payer_rate);
                }
            }

            // Assert distribution invariants
            for participant in &[&lp, &freelancer, &payer] {
                let accrual = dist.get_accrual(participant);
                prop_assert!(accrual >= 0, "Accrual must never be negative: {}", accrual);
            }
        }
    }
}
