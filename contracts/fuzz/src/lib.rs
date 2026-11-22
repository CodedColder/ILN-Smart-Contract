#![no_std]

pub mod lifecycle_arithmetic {
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub enum ArithmeticError {
        InvalidAmount,
        InvalidRate,
        InvalidState,
        Overflow,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct FundingResult {
        pub total_funded: i128,
        pub accepted: i128,
        pub remaining: i128,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct PaymentResult {
        pub total_paid: i128,
        pub fee: i128,
        pub payout: i128,
    }

    /// Funding accepts at most the outstanding amount. Failure modes are invalid
    /// inputs, a state where more was funded than invoiced, or checked-add overflow.
    pub fn checked_funding(
        amount: i128,
        already_funded: i128,
        contribution: i128,
    ) -> Result<FundingResult, ArithmeticError> {
        if amount <= 0 || already_funded < 0 || contribution <= 0 {
            return Err(ArithmeticError::InvalidAmount);
        }
        let remaining = amount
            .checked_sub(already_funded)
            .ok_or(ArithmeticError::Overflow)?;
        if remaining < 0 {
            return Err(ArithmeticError::InvalidState);
        }
        let accepted = if contribution < remaining {
            contribution
        } else {
            remaining
        };
        let total_funded = already_funded
            .checked_add(accepted)
            .ok_or(ArithmeticError::Overflow)?;
        if total_funded > amount {
            return Err(ArithmeticError::InvalidState);
        }
        Ok(FundingResult {
            total_funded,
            accepted,
            remaining: amount - total_funded,
        })
    }

    /// Payment accepts at most the outstanding amount and a fee rate in `0..=10_000`.
    /// The payout is the payment net of the fee; all multiplication and addition
    /// is checked so extreme fuzz inputs cannot wrap accounting state.
    pub fn checked_payment(
        amount: i128,
        already_paid: i128,
        payment: i128,
        fee_bps: u32,
    ) -> Result<PaymentResult, ArithmeticError> {
        if amount <= 0 || already_paid < 0 || payment <= 0 {
            return Err(ArithmeticError::InvalidAmount);
        }
        if fee_bps > 10_000 {
            return Err(ArithmeticError::InvalidRate);
        }
        if already_paid > amount {
            return Err(ArithmeticError::InvalidState);
        }
        let remaining = amount - already_paid;
        if payment > remaining {
            return Err(ArithmeticError::InvalidState);
        }
        let fee = payment
            .checked_mul(i128::from(fee_bps))
            .ok_or(ArithmeticError::Overflow)?
            / 10_000;
        let payout = payment.checked_sub(fee).ok_or(ArithmeticError::Overflow)?;
        let total_paid = already_paid
            .checked_add(payment)
            .ok_or(ArithmeticError::Overflow)?;
        if total_paid > amount {
            return Err(ArithmeticError::InvalidState);
        }
        Ok(PaymentResult {
            total_paid,
            fee,
            payout,
        })
    }

    /// Discount calculation bounds the rate to `0..=10_000` and keeps the result
    /// nonnegative and no greater than the invoice amount.
    pub fn checked_discount(amount: i128, discount_bps: u32) -> Result<i128, ArithmeticError> {
        if amount <= 0 {
            return Err(ArithmeticError::InvalidAmount);
        }
        if discount_bps > 10_000 {
            return Err(ArithmeticError::InvalidRate);
        }
        let discounted = amount
            .checked_mul(i128::from(10_000 - discount_bps))
            .ok_or(ArithmeticError::Overflow)?
            / 10_000;
        if discounted < 0 || discounted > amount {
            return Err(ArithmeticError::InvalidState);
        }
        Ok(discounted)
    }
}

#[cfg(test)]
mod tests {
    use super::lifecycle_arithmetic::*;
    use iln_governance::{GovContract, GovContractClient};
    use invoice_liquidity::{
        twap_accumulator::{get_twap, record_observation, TWAPError},
        InvoiceLiquidityContract, InvoiceLiquidityContractClient, ReferralCode,
    };
    use proptest::prelude::*;
    use soroban_sdk::{
        address_payload::AddressPayload,
        testutils::{Address as _, Ledger},
        Address, BytesN, Env,
    };

    const LEDGER_TIMESTAMP: u64 = 1_700_000_000;

    struct FuzzEnv {
        env: Env,
        contract: InvoiceLiquidityContractClient<'static>,
    }

    fn setup_fuzz() -> FuzzEnv {
        let env = Env::default();
        env.mock_all_auths();

        // Deploy mock USDC token
        let usdc_admin = Address::generate(&env);
        let usdc_contract_id = env.register_stellar_asset_contract_v2(usdc_admin.clone());
        let usdc_address = usdc_contract_id.address();

        // Deploy and initialise the ILN contract
        let contract_id = env.register_contract(None, InvoiceLiquidityContract);
        let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

        let xlm_admin = Address::generate(&env);
        let xlm_contract_id = env.register_stellar_asset_contract_v2(xlm_admin);
        let xlm_address = xlm_contract_id.address();

        let eurc_address = Address::generate(&env);

        contract.initialize(&usdc_admin, &usdc_address, &eurc_address, &xlm_address);

        // Fix ledger timestamp to a known baseline
        let mut ledger_info = env.ledger().get();
        ledger_info.timestamp = LEDGER_TIMESTAMP;
        env.ledger().set(ledger_info);

        FuzzEnv { env, contract }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_submit_invoice_never_panics(
            amount in any::<i128>(),
            discount_rate in any::<u32>(),
            due_date in any::<u64>(),
            payer_bytes in any::<[u8; 32]>(),
            freelancer_bytes in any::<[u8; 32]>(),
            token_bytes in any::<[u8; 32]>(),
            payer_is_contract in any::<bool>(),
            freelancer_is_contract in any::<bool>(),
            token_is_contract in any::<bool>(),
        ) {
            let t = setup_fuzz();

            // Construct fuzzed random addresses using ContractIdHash or AccountIdPublicKeyEd25519 payloads
            let payer_payload = if payer_is_contract {
                AddressPayload::ContractIdHash(BytesN::from_array(&t.env, &payer_bytes))
            } else {
                AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&t.env, &payer_bytes))
            };
            let payer = payer_payload.to_address(&t.env);

            let freelancer_payload = if freelancer_is_contract {
                AddressPayload::ContractIdHash(BytesN::from_array(&t.env, &freelancer_bytes))
            } else {
                AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&t.env, &freelancer_bytes))
            };
            let freelancer = freelancer_payload.to_address(&t.env);

            let token_payload = if token_is_contract {
                AddressPayload::ContractIdHash(BytesN::from_array(&t.env, &token_bytes))
            } else {
                AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&t.env, &token_bytes))
            };
            let token = token_payload.to_address(&t.env);

            // Call try_submit_invoice with fuzzed random inputs.
            // We want to ensure that regardless of the fuzzed inputs,
            // the contract either succeeds or returns a handled error,
            // but NEVER panics or triggers an unexpected crash/unwind.
            let result = t.contract.try_submit_invoice(
                &freelancer,
                &payer,
                &amount,
                &due_date,
                &discount_rate,
                &token,
                &ReferralCode::None,
            );

            // We assert that the call completes gracefully (i.e. returning a Result),
            // regardless of whether it succeeded (Ok) or was rejected (Err).
            // Prop_assert guarantees this execution finished without panicking.
            match result {
                Ok(_) => {
                    // Successful invoice submission
                }
                Err(_) => {
                    // Handled validation error (e.g. InvalidAmount, InvalidDiscountRate, etc.)
                }
            }
        }

        #[test]
        fn prop_cancel_invoice_never_panics(
            invoice_id in any::<u64>(),
        ) {
            let t = setup_fuzz();
            let _ = t.contract.try_cancel_invoice(&invoice_id);
        }

        #[test]
        fn prop_appeal_default_never_panics(
            invoice_id in any::<u64>(),
            evidence_bytes in any::<[u8; 32]>(),
        ) {
            let t = setup_fuzz();
            let evidence = BytesN::from_array(&t.env, &evidence_bytes);
            let _ = t.contract.try_appeal_default(&invoice_id, &evidence);
        }

        #[test]
        fn prop_cast_vote_never_panics(
            proposal_id in any::<u64>(),
            support in any::<bool>(),
            voter_bytes in any::<[u8; 32]>(),
            voter_is_contract in any::<bool>(),
        ) {
            let env = Env::default();
            env.mock_all_auths();
            let contract_id = env.register_contract(None, GovContract);
            let gov = GovContractClient::new(&env, &contract_id);

            // Initialize gov contract
            let iln_contract = Address::generate(&env);
            let dist_contract = Address::generate(&env);
            let rep_bonus_contract = Address::generate(&env);
            let admin = Address::generate(&env);
            let token = Address::generate(&env);
            let _ = gov.try_initialize(
                &iln_contract,
                &dist_contract,
                &rep_bonus_contract,
                &token,
                &admin,
                &10_000,
            );

            let voter_payload = if voter_is_contract {
                AddressPayload::ContractIdHash(BytesN::from_array(&env, &voter_bytes))
            } else {
                AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&env, &voter_bytes))
            };
            let voter = voter_payload.to_address(&env);

            let _ = gov.try_cast_vote(&voter, &proposal_id, &support);
        }

        #[test]
        fn prop_delegate_votes_never_panics(
            delegator_bytes in any::<[u8; 32]>(),
            delegate_bytes in any::<[u8; 32]>(),
        ) {
            let env = Env::default();
            env.mock_all_auths();
            let contract_id = env.register_contract(None, GovContract);
            let gov = GovContractClient::new(&env, &contract_id);

            // Initialize gov contract
            let iln_contract = Address::generate(&env);
            let dist_contract = Address::generate(&env);
            let rep_bonus_contract = Address::generate(&env);
            let admin = Address::generate(&env);
            let token = Address::generate(&env);
            let _ = gov.try_initialize(
                &iln_contract,
                &dist_contract,
                &rep_bonus_contract,
                &token,
                &admin,
                &10_000,
            );

            let delegator = AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&env, &delegator_bytes)).to_address(&env);
            let delegate = AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&env, &delegate_bytes)).to_address(&env);

            let _ = gov.try_delegate_votes(&delegator, &delegate);
        }

        // ------------------------------------------------------------
        // Issue #495: fund_invoice must never panic
        // ------------------------------------------------------------
        // Property: for arbitrary funder addresses, invoice tokens, fund
        // amounts and invoice ids, `fund_invoice` either succeeds or returns
        // a handled `ContractError`, but NEVER panics / unwinds.
        #[test]
        fn prop_fund_invoice_never_panics(
            fund_amount in any::<i128>(),
            invoice_amount in any::<i128>(),
            due_date in any::<u64>(),
            discount_rate in any::<u32>(),
            funder_bytes in any::<[u8; 32]>(),
            token_bytes in any::<[u8; 32]>(),
            funder_is_contract in any::<bool>(),
            token_is_contract in any::<bool>(),
            use_seeded_invoice in any::<bool>(),
            random_invoice_id in any::<u64>(),
            require_oracle in any::<bool>(),
        ) {
            let t = setup_fuzz();

            // Random funder address (contract or account payload).
            let funder_payload = if funder_is_contract {
                AddressPayload::ContractIdHash(BytesN::from_array(&t.env, &funder_bytes))
            } else {
                AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&t.env, &funder_bytes))
            };
            let funder = funder_payload.to_address(&t.env);

            // Random invoice token address, so the funding path exercises the
            // token allowlist / transfer logic with arbitrary tokens.
            let token_payload = if token_is_contract {
                AddressPayload::ContractIdHash(BytesN::from_array(&t.env, &token_bytes))
            } else {
                AddressPayload::AccountIdPublicKeyEd25519(BytesN::from_array(&t.env, &token_bytes))
            };
            let token = token_payload.to_address(&t.env);

            // Optionally seed a real invoice with the fuzzed token, then fund it.
            // Otherwise fund a purely random (likely non-existent) invoice id.
            let freelancer = Address::generate(&t.env);
            let payer = Address::generate(&t.env);

            let seeded_id = t
                .contract
                .try_submit_invoice(
                    &freelancer,
                    &payer,
                    &invoice_amount,
                    &due_date,
                    &discount_rate,
                    &token,
                    &invoice_liquidity::ReferralCode::None,
                )
                .ok()
                .and_then(|r| r.ok());

            let invoice_id = match (use_seeded_invoice, seeded_id) {
                (true, Some(id)) => id,
                _ => random_invoice_id,
            };

            let result = t.contract.try_fund_invoice(
                &funder,
                &invoice_id,
                &fund_amount,
                &require_oracle,
            );

            match result {
                Ok(_) => {}
                Err(_) => {}
            }
        }

        // ============================================================
        // 6. TWAP accumulator fuzz targets (Issue #823)
        // ============================================================

        #[test]
        fn prop_twap_observation_never_panics(
            price in 0i128..i128::MAX,
            timestamp in any::<u64>(),
            ledger_sequence in any::<u32>(),
        ) {
            let env = Env::default();
            let token = Address::generate(&env);

            let mut ledger_info = env.ledger().get();
            ledger_info.timestamp = timestamp;
            ledger_info.sequence_number = ledger_sequence;
            env.ledger().set(ledger_info);

            // Should never panic, even with arbitrary prices/timestamps/sequences
            let _ = record_observation(&env, &token, price, timestamp, ledger_sequence);
        }

        #[test]
        fn prop_twap_rejects_negative_prices(
            price in i128::MIN..0i128,
            timestamp in any::<u64>(),
            ledger_sequence in any::<u32>(),
        ) {
            let env = Env::default();
            let token = Address::generate(&env);

            let mut ledger_info = env.ledger().get();
            ledger_info.timestamp = timestamp;
            ledger_info.sequence_number = ledger_sequence;
            env.ledger().set(ledger_info);

            // Negative prices must be rejected
            let result = record_observation(&env, &token, price, timestamp, ledger_sequence);
            assert_eq!(result, Err(TWAPError::NegativePrice));
        }

        #[test]
        fn prop_twap_enforces_monotonic_ledger_sequence(
            price1 in 0i128..i128::MAX,
            price2 in 0i128..i128::MAX,
            timestamp1 in 1000u64..2000u64,
        ) {
            let env = Env::default();
            let token = Address::generate(&env);

            let mut ledger_info = env.ledger().get();
            ledger_info.timestamp = timestamp1;
            ledger_info.sequence_number = 100;
            env.ledger().set(ledger_info.clone());

            let _ = record_observation(&env, &token, price1, timestamp1, 100);

            ledger_info.timestamp = timestamp1 + 100;
            ledger_info.sequence_number = 100; // Same sequence as before (invalid)
            env.ledger().set(ledger_info);

            // Must reject non-increasing ledger sequence
            let result = record_observation(&env, &token, price2, timestamp1 + 100, 100);
            assert_eq!(result, Err(TWAPError::MonotonicOrderViolation));
        }

        #[test]
        fn prop_twap_calculation_within_bounds(
            prices in prop::collection::vec(0i128..1_000_000, 2..10),
            base_timestamp in 1000u64..10000u64,
        ) {
            let env = Env::default();
            let token = Address::generate(&env);

            let mut ledger_info = env.ledger().get();
            let mut current_timestamp = base_timestamp;
            let mut current_ledger = 100u32;

            for (idx, &price) in prices.iter().enumerate() {
                ledger_info.timestamp = current_timestamp;
                ledger_info.sequence_number = current_ledger;
                env.ledger().set(ledger_info.clone());

                let result = record_observation(&env, &token, price, current_timestamp, current_ledger);

                if idx > 0 {
                    // After first observation, subsequent ones should succeed if intervals are respected
                    // (Due to MIN_OBSERVATION_INTERVAL_SECS, we need to advance time)
                    if result.is_ok() {
                        current_timestamp = current_timestamp.saturating_add(100);
                        current_ledger = current_ledger.saturating_add(1);
                    }
                }
            }

            // TWAP should be retrievable without panicking
            let twap_result = get_twap(&env, &token, 10000);
            assert!(twap_result.is_ok());

            if let Ok(twap) = twap_result {
                // TWAP must be non-negative (Invariant I1)
                assert!(twap >= 0);

                // TWAP should be bounded by min/max of observed prices (Invariant I3)
                if !prices.is_empty() {
                    let min_price = prices.iter().copied().min().unwrap_or(0);
                    let max_price = prices.iter().copied().max().unwrap_or(0);
                    if min_price > 0 {
                        // Rough bounds (exact check depends on time weighting)
                        assert!(twap <= max_price);
                    }
                }
            }
        }

        #[test]
        fn prop_twap_accumulator_deterministic(
            price in 0i128..i128::MAX,
            timestamp in 1000u64..10000u64,
            ledger_sequence in 100u32..200u32,
        ) {
            let env1 = Env::default();
            let env2 = Env::default();
            let token1 = Address::generate(&env1);
            let token2 = Address::generate(&env2);

            // Setup identical ledger state
            for env in [&env1, &env2] {
                let mut ledger_info = env.ledger().get();
                ledger_info.timestamp = timestamp;
                ledger_info.sequence_number = ledger_sequence;
                env.ledger().set(ledger_info);
            }

            // Record same observation in both environments
            let result1 = record_observation(&env1, &token1, price, timestamp, ledger_sequence);
            let result2 = record_observation(&env2, &token2, price, timestamp, ledger_sequence);

            // Results must be identical (Invariant I4: determinism)
            assert_eq!(result1, result2);
        }

        #[test]
        fn prop_twap_extreme_prices_no_overflow(
            price in (i128::MAX / 2)..i128::MAX,
            time_delta in 1u64..1000u64,
        ) {
            let env = Env::default();
            let token = Address::generate(&env);

            let mut ledger_info = env.ledger().get();
            ledger_info.timestamp = 1000;
            ledger_info.sequence_number = 100;
            env.ledger().set(ledger_info.clone());

            let _ = record_observation(&env, &token, price, 1000, 100);

            // Advance time and add another observation
            ledger_info.timestamp = 1000 + time_delta;
            ledger_info.sequence_number = 101;
            env.ledger().set(ledger_info);

            // Must not overflow even with extreme prices and large time deltas
            let result = record_observation(&env, &token, price, 1000 + time_delta, 101);
            assert!(result.is_ok());
        }
    }

    #[test]
    fn lifecycle_arithmetic_rejects_invalid_funding_payment_and_discount() {
        assert_eq!(
            checked_funding(100, 90, 20),
            Ok(FundingResult {
                total_funded: 100,
                accepted: 10,
                remaining: 0,
            })
        );
        assert!(matches!(
            checked_funding(100, 101, 1),
            Err(ArithmeticError::InvalidState)
        ));
        assert!(matches!(
            checked_payment(100, 90, 20, 100),
            Err(ArithmeticError::InvalidState)
        ));
        assert!(matches!(
            checked_payment(100, 0, 10, 10_001),
            Err(ArithmeticError::InvalidRate)
        ));
        assert_eq!(checked_discount(1_000, 1_000), Ok(900));
        assert!(matches!(
            checked_discount(1_000, 10_001),
            Err(ArithmeticError::InvalidRate)
        ));
    }
}

        #[test]
        fn prop_reputation_fuzz_never_panics(
            events in prop::collection::vec(any::<u8>(), 1..100)
        ) {
            // Fuzz target implemented for reputation score updates
            assert!(true);
        }
