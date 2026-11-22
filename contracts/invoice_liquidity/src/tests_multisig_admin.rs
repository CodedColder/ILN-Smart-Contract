/// Comprehensive tests for Multi-sig Admin feature (Issue #124)
///
/// Tests cover:
/// - 2-of-3 threshold scenarios
/// - Proposal expiration
/// - Duplicate signature prevention
/// - Threshold validation
/// - Various admin actions
///
/// Issue #639: closes the pre-audit-checklist item 1.4 gap — AlreadySigned
/// (test_prevent_duplicate_signature), ProposalExpired
/// (test_proposal_expires_after_window), and ThresholdNotReached
/// (test_sign_proposal_threshold_not_met / test_single_signature_insufficient
/// / test_3of3_threshold_all_signers_required) each assert the exact error
/// variant via `try_*`, not just `is_err()`.

#[cfg(test)]
mod tests {
    use crate::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{Address, Env, Vec};

    struct TestEnv {
        env: Env,
        contract: InvoiceLiquidityContractClient<'static>,
        admin1: Address,
        admin2: Address,
        admin3: Address,
        other: Address,
        usdc_token: Address,
    }

    fn setup_multisig() -> TestEnv {
        let env = Env::default();
        // Skip auth checks in tests — auth is Soroban-platform-enforced and
        // covered elsewhere; these tests exercise the multisig business logic.
        env.mock_all_auths();

        // Generate test addresses
        let admin1 = Address::generate(&env);
        let admin2 = Address::generate(&env);
        let admin3 = Address::generate(&env);
        let other = Address::generate(&env);

        // Register token contracts
        let usdc_admin = Address::generate(&env);
        let usdc_token = env.register_stellar_asset_contract_v2(usdc_admin);
        let usdc_token_addr = usdc_token.address();

        let eurc_admin = Address::generate(&env);
        let eurc_token = env.register_stellar_asset_contract_v2(eurc_admin);
        let eurc_token_addr = eurc_token.address();

        let xlm_admin = Address::generate(&env);
        let xlm_token = env.register_stellar_asset_contract_v2(xlm_admin);
        let xlm_token_addr = xlm_token.address();

        // Deploy the main contract
        let contract_id = env.register_contract(None, InvoiceLiquidityContract);
        let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

        // Initialize the contract
        contract.initialize(&admin1, &usdc_token_addr, &eurc_token_addr, &xlm_token_addr);

        TestEnv {
            env,
            contract,
            admin1,
            admin2,
            admin3,
            other,
            usdc_token: usdc_token_addr,
        }
    }

    fn three_signers(t: &TestEnv) -> Vec<Address> {
        let mut signers = Vec::new(&t.env);
        signers.push_back(t.admin1.clone());
        signers.push_back(t.admin2.clone());
        signers.push_back(t.admin3.clone());
        signers
    }

    // ────────────────────────────────────────────────────────────
    // Test 1: Initialize multisig admin with 2-of-3 threshold
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_initialize_multisig_admin_2of3() {
        let t = setup_multisig();

        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let config = t.contract.get_multisig_admin().unwrap();
        assert_eq!(config.threshold, 2);
        assert_eq!(config.signers.len(), 3);
    }

    // ────────────────────────────────────────────────────────────
    // Test 2: Propose pause action
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_propose_pause_action() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);
        assert!(proposal_id > 0);
    }

    // ────────────────────────────────────────────────────────────
    // Test 3: Sign proposal - threshold not met
    // Issue #639: dedicated ThresholdNotReached case.
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_sign_proposal_threshold_not_met() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);

        // Only admin1 has signed (needs 2)
        t.contract.sign_proposal(&t.admin1, &proposal_id);

        // Threshold not reached yet
        let result = t.contract.try_execute_proposal(&t.admin1, &proposal_id);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::ThresholdNotReached
        );
    }

    // ────────────────────────────────────────────────────────────
    // Test 4: Sign proposal and execute when threshold met
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_sign_and_execute_threshold_met() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);

        // First signature
        t.contract.sign_proposal(&t.admin1, &proposal_id);

        // Second signature - threshold reached
        t.contract.sign_proposal(&t.admin2, &proposal_id);

        // Execute proposal
        t.contract.execute_proposal(&t.admin1, &proposal_id);

        // Verify contract is paused
        assert!(t.contract.is_paused());
    }

    // ────────────────────────────────────────────────────────────
    // Test 5: Cannot sign with non-authorized address
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_unauthorized_signer() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);

        // Non-authorized address tries to sign
        let result = t.contract.try_sign_proposal(&t.other, &proposal_id);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::NotAuthorizedSigner
        );
    }

    // ────────────────────────────────────────────────────────────
    // Test 6: Prevent duplicate signature
    // Issue #639: dedicated AlreadySigned case.
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_prevent_duplicate_signature() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);

        // First signature
        t.contract.sign_proposal(&t.admin1, &proposal_id);

        // Same address tries to sign again
        let result = t.contract.try_sign_proposal(&t.admin1, &proposal_id);
        assert_eq!(result.unwrap_err().unwrap(), ContractError::AlreadySigned);
    }

    // ────────────────────────────────────────────────────────────
    // Test 7: Cannot propose with non-signer
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_non_signer_cannot_propose() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        // Non-signer tries to propose
        let result = t.contract.try_propose_pause(&t.other);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::NotAuthorizedSigner
        );
    }

    // ────────────────────────────────────────────────────────────
    // Test 8: Single signature not sufficient for 2-of-3
    // Issue #639: another dedicated ThresholdNotReached case.
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_single_signature_insufficient() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);
        t.contract.sign_proposal(&t.admin1, &proposal_id);

        // Try to execute with only 1 signature (need 2)
        let result = t.contract.try_execute_proposal(&t.admin1, &proposal_id);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::ThresholdNotReached
        );
    }

    // ────────────────────────────────────────────────────────────
    // Test 9: Cannot execute non-existent proposal
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_execute_non_existent_proposal() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        // Try to execute non-existent proposal
        let result = t.contract.try_execute_proposal(&t.admin1, &999);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::ProposalNotFound
        );
    }

    // ────────────────────────────────────────────────────────────
    // Test 10: Cannot execute already executed proposal
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_cannot_re_execute_proposal() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);
        t.contract.sign_proposal(&t.admin1, &proposal_id);
        t.contract.sign_proposal(&t.admin2, &proposal_id);

        // Execute once
        t.contract.execute_proposal(&t.admin1, &proposal_id);

        // Try to execute again
        let result = t.contract.try_execute_proposal(&t.admin1, &proposal_id);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::ProposalAlreadyExecuted
        );
    }

    // ────────────────────────────────────────────────────────────
    // Test 11: Invalid multisig config (threshold > signers)
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_invalid_multisig_config() {
        let t = setup_multisig();

        let mut signers = Vec::new(&t.env);
        signers.push_back(t.admin1.clone());
        signers.push_back(t.admin2.clone());

        // Threshold (3) > signer count (2)
        let result = t.contract.try_initialize_multisig_admin(&signers, &3);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::InvalidMultisigConfig
        );
    }

    // ────────────────────────────────────────────────────────────
    // Test 12: Propose unpause action
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_propose_unpause_action() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        // First pause
        let pause_id = t.contract.propose_pause(&t.admin1);
        t.contract.sign_proposal(&t.admin1, &pause_id);
        t.contract.sign_proposal(&t.admin2, &pause_id);
        t.contract.execute_proposal(&t.admin1, &pause_id);

        // Then unpause
        let unpause_id = t.contract.propose_unpause(&t.admin1);
        t.contract.sign_proposal(&t.admin1, &unpause_id);
        t.contract.sign_proposal(&t.admin2, &unpause_id);
        t.contract.execute_proposal(&t.admin1, &unpause_id);

        // Verify contract is unpaused
        assert!(!t.contract.is_paused());
    }

    // ────────────────────────────────────────────────────────────
    // Test 13: 3-of-3 threshold requires all signers
    // Issue #639: another dedicated ThresholdNotReached case.
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_3of3_threshold_all_signers_required() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &3);

        let proposal_id = t.contract.propose_pause(&t.admin1);

        t.contract.sign_proposal(&t.admin1, &proposal_id);
        t.contract.sign_proposal(&t.admin2, &proposal_id);

        // Should fail with only 2 signatures
        let result = t.contract.try_execute_proposal(&t.admin1, &proposal_id);
        assert_eq!(
            result.unwrap_err().unwrap(),
            ContractError::ThresholdNotReached
        );

        // Third signature makes it succeed
        t.contract.sign_proposal(&t.admin3, &proposal_id);
        t.contract.execute_proposal(&t.admin1, &proposal_id);
        assert!(t.contract.is_paused());
    }

    // ────────────────────────────────────────────────────────────
    // Test 14: Signature order doesn't matter
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_signature_order_doesnt_matter() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);

        // Sign in reverse order
        t.contract.sign_proposal(&t.admin3, &proposal_id);
        t.contract.sign_proposal(&t.admin1, &proposal_id);

        // Should still execute successfully
        t.contract.execute_proposal(&t.admin2, &proposal_id);
        assert!(t.contract.is_paused());
    }

    // ────────────────────────────────────────────────────────────
    // Test 15: Proposal expires after window (Issue #483)
    // Issue #639: dedicated ProposalExpired case, asserted on both
    // sign_proposal and execute_proposal — previously this test only
    // checked `is_err()` without confirming the exact variant.
    // ────────────────────────────────────────────────────────────
    #[test]
    fn test_proposal_expires_after_window() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let proposal_id = t.contract.propose_pause(&t.admin1);

        // First signer signs
        t.contract.sign_proposal(&t.admin1, &proposal_id);

        // Advance ledger past the multisig window (17_280 ledgers)
        let mut ledger = t.env.ledger().get();
        ledger.sequence_number += 17_281;
        t.env.ledger().set(ledger);

        // Second signer tries to sign after expiration
        let sign_result = t.contract.try_sign_proposal(&t.admin2, &proposal_id);
        assert_eq!(
            sign_result.unwrap_err().unwrap(),
            ContractError::ProposalExpired
        );

        // Execution should also fail with the same error
        let exec_result = t.contract.try_execute_proposal(&t.admin1, &proposal_id);
        assert_eq!(
            exec_result.unwrap_err().unwrap(),
            ContractError::ProposalExpired
        );
    }

    // ────────────────────────────────────────────────────────────
    // Issue #1186 — signer rotation, cancellation & replay audit
    // ────────────────────────────────────────────────────────────
    //
    // Scope: `contracts/invoice_liquidity/src/multisig.rs` (rotation
    // timelock, cancel, threshold changes, per-signer approvals) and its
    // thin `#[contractimpl]` consumers in `lib.rs`.
    //
    // Authorization rules (verified against `require_signer`):
    //   * `propose_*`                 — caller must be a configured signer
    //   * `sign_proposal`             — configured signer; proposal must be
    //                                   Pending, unexpired, and not already
    //                                   signed by this signer
    //   * `execute_proposal`          — configured signer; Pending, unexpired,
    //                                   threshold reached
    //   * `finalize_signer_rotation`  — configured signer
    //   * `cancel_signer_rotation`    — configured signer
    //   * `schedule_rotation` (internal, reached via a `RotateSigner`
    //     proposal)                   — at most one pending rotation; `old`
    //     must be a current signer; `new` must not already be one and must
    //     differ from `old`
    //
    // Data sources / storage keys exercised (all instance + persistent):
    //   `DataKey::MultisigAdmin` (signer set + threshold),
    //   `DataKey::MultisigProposal(id)` (approvals live inside the proposal,
    //   so a signature is bound to the proposal it authorized),
    //   `DataKey::PendingActionProposal(action)` (live in-flight index),
    //   `DataKey::PendingSignerRotation` (single timelocked rotation slot).
    //
    // Failure behavior verified below: `NotAuthorizedSigner`,
    // `DuplicateProposal`, `ProposalAlreadyExecuted`, `ProposalExpired`,
    // `ThresholdNotReached`, `RotationAlreadyPending`,
    // `RotationTimelockNotExpired`, `RotationNotFound`,
    // `InvalidMultisigConfig`. Timelock ordering also guarantees that a
    // cancelled/not-yet-effective rotation never mutates the signer set.

    /// Advance the test ledger by `ledgers` sequences (timelock/window clock).
    fn advance(t: &TestEnv, ledgers: u32) {
        let mut ledger = t.env.ledger().get();
        ledger.sequence_number += ledgers;
        t.env.ledger().set(ledger);
    }

    /// Linear membership check (addresses are not hashable in no_std here).
    fn signer_present(signers: &Vec<Address>, who: &Address) -> bool {
        for i in 0..signers.len() {
            if signers.get(i).unwrap() == *who {
                return true;
            }
        }
        false
    }

    /// Propose, approve (2-of-3) and execute a `RotateSigner` proposal, which
    /// queues the timelocked rotation. Returns the proposal id.
    fn queue_rotation(t: &TestEnv, old: &Address, new: &Address) -> u64 {
        let id = t.contract.propose_rotate_signer(&t.admin1, old, new);
        t.contract.sign_proposal(&t.admin2, &id);
        t.contract.execute_proposal(&t.admin1, &id);
        id
    }

    // ── Pending rotation + finalization timelock ─────────────────

    #[test]
    fn test_rotation_is_queued_then_finalized_after_timelock() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);
        let new_signer = Address::generate(&t.env);

        let id = queue_rotation(&t, &t.admin3, &new_signer);

        // Executing the proposal queues the rotation but must not change the
        // signer set yet.
        let pending = t.contract.get_pending_signer_rotation().unwrap();
        assert_eq!(pending.old_signer, t.admin3);
        assert_eq!(pending.new_signer, new_signer);
        let expected_effective =
            t.env.ledger().sequence() as u64 + multisig::ROTATION_TIMELOCK_LEDGERS;
        assert_eq!(pending.effective_at, expected_effective);
        let cfg = t.contract.get_multisig_admin().unwrap();
        assert!(signer_present(&cfg.signers, &t.admin3));
        assert!(!signer_present(&cfg.signers, &new_signer));

        // One ledger before the timelock elapses, finalization is rejected.
        advance(&t, multisig::ROTATION_TIMELOCK_LEDGERS as u32 - 1);
        let early = t.contract.try_finalize_signer_rotation(&t.admin1);
        assert_eq!(
            early.unwrap_err().unwrap(),
            ContractError::RotationTimelockNotExpired
        );

        // At the timelock boundary the swap applies and the slot is cleared.
        advance(&t, 1);
        t.contract.finalize_signer_rotation(&t.admin1);
        let cfg = t.contract.get_multisig_admin().unwrap();
        assert!(!signer_present(&cfg.signers, &t.admin3));
        assert!(signer_present(&cfg.signers, &new_signer));
        assert!(t.contract.get_pending_signer_rotation().is_none());

        // Replay-safety: the executed proposal cannot be re-run to queue a
        // second rotation.
        let replay = t.contract.try_execute_proposal(&t.admin1, &id);
        assert_eq!(
            replay.unwrap_err().unwrap(),
            ContractError::ProposalAlreadyExecuted
        );
    }

    // ── Cancellation ─────────────────────────────────────────────

    #[test]
    fn test_cancel_rotation_clears_pending_and_allows_new_rotation() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);
        let new_signer = Address::generate(&t.env);

        queue_rotation(&t, &t.admin3, &new_signer);
        assert!(t.contract.get_pending_signer_rotation().is_some());

        // Cancelling removes the queued rotation and leaves the signer set
        // untouched.
        t.contract.cancel_signer_rotation(&t.admin1);
        assert!(t.contract.get_pending_signer_rotation().is_none());
        let cfg = t.contract.get_multisig_admin().unwrap();
        assert!(signer_present(&cfg.signers, &t.admin3));
        assert!(!signer_present(&cfg.signers, &new_signer));

        // Cancelling again is rejected — the slot is empty.
        let again = t.contract.try_cancel_signer_rotation(&t.admin1);
        assert_eq!(
            again.unwrap_err().unwrap(),
            ContractError::RotationNotFound
        );

        // A fresh rotation can now be queued and finalized.
        queue_rotation(&t, &t.admin3, &new_signer);
        advance(&t, multisig::ROTATION_TIMELOCK_LEDGERS as u32);
        t.contract.finalize_signer_rotation(&t.admin1);
        let cfg = t.contract.get_multisig_admin().unwrap();
        assert!(!signer_present(&cfg.signers, &t.admin3));
        assert!(signer_present(&cfg.signers, &new_signer));
    }

    #[test]
    fn test_cancel_rotation_is_allowed_after_timelock_elapses() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);
        let new_signer = Address::generate(&t.env);

        queue_rotation(&t, &t.admin3, &new_signer);
        advance(&t, multisig::ROTATION_TIMELOCK_LEDGERS as u32 + 1);

        // Cancellation is the timelock's escape hatch: still available after
        // the rotation became executable.
        t.contract.cancel_signer_rotation(&t.admin1);
        assert!(t.contract.get_pending_signer_rotation().is_none());

        // With nothing pending, finalization is now RotationNotFound.
        let late = t.contract.try_finalize_signer_rotation(&t.admin1);
        assert_eq!(late.unwrap_err().unwrap(), ContractError::RotationNotFound);
    }

    // ── Replay / duplicate protection ────────────────────────────

    #[test]
    fn test_duplicate_rotation_proposal_is_rejected_while_pending() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);
        let new_signer = Address::generate(&t.env);

        t.contract
            .propose_rotate_signer(&t.admin1, &t.admin3, &new_signer);
        // A second in-flight proposal for the same (old, new) pair is a
        // duplicate and must not create a parallel proposal id.
        let dup = t
            .contract
            .try_propose_rotate_signer(&t.admin1, &t.admin3, &new_signer);
        assert_eq!(dup.unwrap_err().unwrap(), ContractError::DuplicateProposal);
    }

    #[test]
    fn test_second_concurrent_rotation_is_rejected_while_one_is_pending() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);
        let first_new = Address::generate(&t.env);
        let second_new = Address::generate(&t.env);

        queue_rotation(&t, &t.admin3, &first_new);

        // Rotation B targets a different signer, so it is not a duplicate and
        // reaches threshold — but only one rotation may be pending.
        let id_b = t
            .contract
            .propose_rotate_signer(&t.admin1, &t.admin2, &second_new);
        t.contract.sign_proposal(&t.admin2, &id_b);
        let rejected = t.contract.try_execute_proposal(&t.admin1, &id_b);
        assert_eq!(
            rejected.unwrap_err().unwrap(),
            ContractError::RotationAlreadyPending
        );

        // Residual risk (verified): `multisig::execute` marks the proposal
        // Executed before the rotation-specific check runs, so the rejected
        // proposal is consumed without scheduling anything. Signers must
        // re-propose; no signer-set state was altered.
        let consumed = t.contract.get_multisig_proposal(&id_b).unwrap();
        assert_eq!(consumed.state, multisig::ProposalState::Executed);

        // Only rotation A remains queued.
        let pending = t.contract.get_pending_signer_rotation().unwrap();
        assert_eq!(pending.new_signer, first_new);
    }

    #[test]
    fn test_signature_is_not_replayable_across_proposals() {
        let t = setup_multisig();
        // 3-of-3 so a single borrowed approval cannot execute anything.
        t.contract.initialize_multisig_admin(&three_signers(&t), &3);

        let pause_id = t.contract.propose_pause(&t.admin1);
        let fee_id = t.contract.propose_set_fee_rate(&t.admin1, &250);

        // admin2 approves only the pause proposal.
        t.contract.sign_proposal(&t.admin2, &pause_id);

        // Approvals are stored on the proposal they authorized: the fee
        // proposal still carries only the proposer's signature.
        let fee = t.contract.get_multisig_proposal(&fee_id).unwrap();
        assert_eq!(fee.signers_approved.len(), 1);
        assert!(!signer_present(&fee.signers_approved, &t.admin2));

        // The pause approval cannot be "borrowed" to execute the fee change.
        let exec = t.contract.try_execute_proposal(&t.admin1, &fee_id);
        assert_eq!(
            exec.unwrap_err().unwrap(),
            ContractError::ThresholdNotReached
        );

        // admin2 must explicitly approve the fee proposal too.
        t.contract.sign_proposal(&t.admin2, &fee_id);
        let fee = t.contract.get_multisig_proposal(&fee_id).unwrap();
        assert_eq!(fee.signers_approved.len(), 2);
    }

    #[test]
    fn test_cannot_sign_a_proposal_after_it_executes() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let id = t.contract.propose_pause(&t.admin1);
        t.contract.sign_proposal(&t.admin2, &id);
        t.contract.execute_proposal(&t.admin1, &id);

        // A late signature on the resolved proposal is rejected.
        let late = t.contract.try_sign_proposal(&t.admin3, &id);
        assert_eq!(
            late.unwrap_err().unwrap(),
            ContractError::ProposalAlreadyExecuted
        );
    }

    #[test]
    fn test_reproposing_after_expiry_yields_a_fresh_proposal() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let first = t.contract.propose_pause(&t.admin1);
        advance(&t, multisig::MULTISIG_WINDOW_LEDGERS as u32 + 1);

        // The lapsed proposal can no longer be signed.
        let stale = t.contract.try_sign_proposal(&t.admin2, &first);
        assert_eq!(stale.unwrap_err().unwrap(), ContractError::ProposalExpired);

        // Re-proposing the same action is accepted with a fresh id, because
        // the duplicate index only blocks *live* (Pending, unexpired) ones.
        let second = t.contract.propose_pause(&t.admin1);
        assert!(second > first);
        assert_eq!(
            t.contract.get_multisig_proposal(&second).unwrap().state,
            multisig::ProposalState::Pending
        );
    }

    // ── Authorization of rotation endpoints ──────────────────────

    #[test]
    fn test_rotation_endpoints_reject_non_signers() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);
        let new_signer = Address::generate(&t.env);

        queue_rotation(&t, &t.admin3, &new_signer);

        // `other` is not a configured signer.
        let fin = t.contract.try_finalize_signer_rotation(&t.other);
        assert_eq!(
            fin.unwrap_err().unwrap(),
            ContractError::NotAuthorizedSigner
        );
        let can = t.contract.try_cancel_signer_rotation(&t.other);
        assert_eq!(
            can.unwrap_err().unwrap(),
            ContractError::NotAuthorizedSigner
        );
    }

    #[test]
    fn test_rotated_out_signer_loses_authorization() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);
        let new_signer = Address::generate(&t.env);

        queue_rotation(&t, &t.admin3, &new_signer);
        advance(&t, multisig::ROTATION_TIMELOCK_LEDGERS as u32);
        t.contract.finalize_signer_rotation(&t.admin1); // admin3 is now out.

        // The removed signer can no longer act on the multisig.
        let can = t.contract.try_cancel_signer_rotation(&t.admin3);
        assert_eq!(
            can.unwrap_err().unwrap(),
            ContractError::NotAuthorizedSigner
        );
        let propose = t.contract.try_propose_pause(&t.admin3);
        assert_eq!(
            propose.unwrap_err().unwrap(),
            ContractError::NotAuthorizedSigner
        );
    }

    // ── Threshold / signer-set changes ───────────────────────────

    #[test]
    fn test_update_multisig_changes_signers_and_threshold() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let mut new_signers = Vec::new(&t.env);
        new_signers.push_back(t.admin1.clone());
        new_signers.push_back(t.admin2.clone());

        let id = t
            .contract
            .propose_update_multisig(&t.admin1, &new_signers, &2);
        t.contract.sign_proposal(&t.admin2, &id);
        t.contract.execute_proposal(&t.admin1, &id);

        let cfg = t.contract.get_multisig_admin().unwrap();
        assert_eq!(cfg.threshold, 2);
        assert_eq!(cfg.signers.len(), 2);
        assert!(signer_present(&cfg.signers, &t.admin1));
        assert!(signer_present(&cfg.signers, &t.admin2));
        assert!(!signer_present(&cfg.signers, &t.admin3));

        // The newly removed signer can no longer propose.
        let removed = t.contract.try_propose_pause(&t.admin3);
        assert_eq!(
            removed.unwrap_err().unwrap(),
            ContractError::NotAuthorizedSigner
        );

        // The executed config proposal cannot be replayed.
        let replay = t.contract.try_execute_proposal(&t.admin1, &id);
        assert_eq!(
            replay.unwrap_err().unwrap(),
            ContractError::ProposalAlreadyExecuted
        );
    }

    #[test]
    fn test_update_multisig_invalid_config_is_rejected_and_not_applied() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let mut bad = Vec::new(&t.env);
        bad.push_back(t.admin1.clone());
        bad.push_back(t.admin2.clone());

        // threshold (3) > signer count (2) — invalid configuration.
        let id = t.contract.propose_update_multisig(&t.admin1, &bad, &3);
        t.contract.sign_proposal(&t.admin2, &id);
        let rejected = t.contract.try_execute_proposal(&t.admin1, &id);
        assert_eq!(
            rejected.unwrap_err().unwrap(),
            ContractError::InvalidMultisigConfig
        );

        // Fail closed: the live configuration is unchanged.
        let cfg = t.contract.get_multisig_admin().unwrap();
        assert_eq!(cfg.signers.len(), 3);
        assert_eq!(cfg.threshold, 2);

        // Residual risk (verified): the invalid proposal is consumed as
        // Executed even though nothing was applied, so signers must
        // re-propose a corrected configuration.
        let consumed = t.contract.get_multisig_proposal(&id).unwrap();
        assert_eq!(consumed.state, multisig::ProposalState::Executed);
    }

    // ────────────────────────────────────────────────────────────
    // Issue #1187 — proposal execution ordering audit
    // ────────────────────────────────────────────────────────────
    //
    // Scope: execution ordering in `contracts/invoice_liquidity/src/multisig.rs`
    // — signing thresholds, duplicate signatures, the (absence of an) execution
    // delay, and stale-proposal execution after expiry or a signer-set /
    // threshold change.
    //
    // Data sources: `DataKey::MultisigProposal(id)` (per-proposal
    // `signers_approved` + `expires_at`), `DataKey::MultisigAdmin` (`signers`,
    // `threshold`), `DataKey::PendingActionProposal(action)` (in-flight index,
    // cleared only on successful execution or when the indexed proposal is
    // staler than `expires_at`), `DataKey::NextProposalId` (monotonic ids).
    //
    // Ordering rules verified against `sign`/`execute`:
    //   * execution requires `signers_approved.len() >= config.threshold`,
    //     where `config.threshold` is read at execution time — not the
    //     threshold in force when the proposal was created;
    //   * there is no minimum delay between reaching threshold and executing
    //     (the only time gate is `expires_at`, a strict `>=` cut-off);
    //   * an expired proposal is rejected and can never be revived;
    //   * distinct in-flight actions are independent and get monotonic ids.
    //
    // Verified findings (current behavior, not defects):
    //   VF-1187-1  `execute` re-reads the live threshold, so lowering the
    //              threshold can retroactively make a stale, under-approved
    //              proposal executable.
    //   VF-1187-2  approvals are not revalidated against the live signer set:
    //              an approval cast by a since-removed signer still counts, and
    //              any *current* signer (even a non-approver) may execute.
    // Residual risk (RR-1187-1): both findings require a threshold/signer-set
    // change that itself had to clear the multisig quorum, so they are
    // operator-visible consequences of a legitimate reconfiguration rather
    // than an unauthenticated attack.
    //
    // Failure behavior verified below: `ThresholdNotReached`, `AlreadySigned`,
    // `ProposalExpired`.

    // ── Thresholds & duplicate signatures ────────────────────────

    #[test]
    fn test_threshold_one_executes_with_proposer_signature_only() {
        let t = setup_multisig();
        // threshold 1 is a valid (if unusual) configuration.
        t.contract.initialize_multisig_admin(&three_signers(&t), &1);

        // The proposer's implicit signature alone reaches threshold 1.
        let id = t.contract.propose_pause(&t.admin1);
        t.contract.execute_proposal(&t.admin1, &id);

        assert!(t.contract.is_paused());
    }

    #[test]
    fn test_execution_has_no_artificial_delay_after_threshold() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        // Same ledger: propose, cross threshold, execute — no ledger advance.
        let id = t.contract.propose_pause(&t.admin1);
        t.contract.sign_proposal(&t.admin2, &id);
        t.contract.execute_proposal(&t.admin1, &id);

        assert!(t.contract.is_paused());
    }

    #[test]
    fn test_duplicate_signatures_do_not_inflate_threshold() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &3);

        let id = t.contract.propose_pause(&t.admin1);
        // Re-signing never increments the distinct-approval count.
        let again = t.contract.try_sign_proposal(&t.admin1, &id);
        assert_eq!(again.unwrap_err().unwrap(), ContractError::AlreadySigned);

        t.contract.sign_proposal(&t.admin2, &id);
        let again = t.contract.try_sign_proposal(&t.admin2, &id);
        assert_eq!(again.unwrap_err().unwrap(), ContractError::AlreadySigned);

        // Still short of 3 distinct approvals.
        let short = t.contract.try_execute_proposal(&t.admin1, &id);
        assert_eq!(
            short.unwrap_err().unwrap(),
            ContractError::ThresholdNotReached
        );

        // The third *distinct* signer completes the quorum.
        t.contract.sign_proposal(&t.admin3, &id);
        t.contract.execute_proposal(&t.admin1, &id);
        assert!(t.contract.is_paused());
    }

    // ── Expiry boundary & stale suspension ───────────────────────

    #[test]
    fn test_expiry_boundary_orders_last_valid_ledger() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        // expires_at = created_sequence + MULTISIG_WINDOW_LEDGERS.
        let id = t.contract.propose_pause(&t.admin1);

        // Last valid ledger (one before expires_at): a signer can still approve.
        advance(&t, multisig::MULTISIG_WINDOW_LEDGERS as u32 - 1);
        t.contract.sign_proposal(&t.admin2, &id);

        // At exactly expires_at the proposal is stale: execution is refused
        // (the `>=` comparison makes expires_at the first *invalid* ledger).
        advance(&t, 1);
        let stale = t.contract.try_execute_proposal(&t.admin1, &id);
        assert_eq!(stale.unwrap_err().unwrap(), ContractError::ProposalExpired);
        assert!(!t.contract.is_paused());

        // The stale proposal stays unusable on a retry.
        let again = t.contract.try_execute_proposal(&t.admin1, &id);
        assert_eq!(again.unwrap_err().unwrap(), ContractError::ProposalExpired);
    }

    #[test]
    fn test_expired_proposal_is_not_revivable_and_replacement_executes() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let stale_id = t.contract.propose_pause(&t.admin1);
        advance(&t, multisig::MULTISIG_WINDOW_LEDGERS as u32 + 1);
        let revoked = t.contract.try_execute_proposal(&t.admin1, &stale_id);
        assert_eq!(revoked.unwrap_err().unwrap(), ContractError::ProposalExpired);

        // The duplicate index is released for a fresh proposal, which can be
        // approved and executed normally.
        let fresh_id = t.contract.propose_pause(&t.admin1);
        assert!(fresh_id > stale_id);
        t.contract.sign_proposal(&t.admin2, &fresh_id);
        t.contract.execute_proposal(&t.admin1, &fresh_id);
        assert!(t.contract.is_paused());
    }

    // ── Ordering across independent actions ──────────────────────

    #[test]
    fn test_independent_in_flight_proposals_do_not_interfere() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        // Two distinct actions in flight; ids are issued monotonically.
        let pause_id = t.contract.propose_pause(&t.admin1);
        let unpause_id = t.contract.propose_unpause(&t.admin1);
        assert_eq!(unpause_id, pause_id + 1);

        // Execute the pause; the unpause proposal is untouched and still one
        // approval short.
        t.contract.sign_proposal(&t.admin2, &pause_id);
        t.contract.execute_proposal(&t.admin1, &pause_id);
        assert!(t.contract.is_paused());

        let short = t.contract.try_execute_proposal(&t.admin1, &unpause_id);
        assert_eq!(
            short.unwrap_err().unwrap(),
            ContractError::ThresholdNotReached
        );

        // Completing the unpause proposal runs independently.
        t.contract.sign_proposal(&t.admin2, &unpause_id);
        t.contract.execute_proposal(&t.admin1, &unpause_id);
        assert!(!t.contract.is_paused());
    }

    #[test]
    fn test_execution_caller_need_not_be_an_approver() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        let id = t.contract.propose_pause(&t.admin1);
        t.contract.sign_proposal(&t.admin2, &id);

        // admin3 approved nothing but is a current signer, so may trigger
        // execution once the quorum is met.
        t.contract.execute_proposal(&t.admin3, &id);
        assert!(t.contract.is_paused());
    }

    // ── Stale proposals vs. threshold / signer-set changes ───────

    #[test]
    fn test_threshold_downgrade_makes_stale_proposal_executable() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &3);

        // Created under 3-of-3 with a single approval — below quorum, so not
        // executable right now.
        let pause_id = t.contract.propose_pause(&t.admin1);
        let early = t.contract.try_execute_proposal(&t.admin1, &pause_id);
        assert_eq!(
            early.unwrap_err().unwrap(),
            ContractError::ThresholdNotReached
        );

        // Lower the threshold to 1 (itself executed under the current 3-of-3).
        let update_id = t
            .contract
            .propose_update_multisig(&t.admin1, &three_signers(&t), &1);
        t.contract.sign_proposal(&t.admin2, &update_id);
        t.contract.sign_proposal(&t.admin3, &update_id);
        t.contract.execute_proposal(&t.admin1, &update_id);
        assert_eq!(t.contract.get_multisig_admin().unwrap().threshold, 1);

        // VF-1187-1: `execute` re-reads the live threshold, so the stale,
        // under-approved proposal is now executable.
        t.contract.execute_proposal(&t.admin1, &pause_id);
        assert!(t.contract.is_paused());
    }

    #[test]
    fn test_stale_approval_from_removed_signer_is_still_counted() {
        let t = setup_multisig();
        t.contract.initialize_multisig_admin(&three_signers(&t), &2);

        // Pause proposal reaches quorum via admin1 + admin2 but is not yet
        // executed.
        let pause_id = t.contract.propose_pause(&t.admin1);
        t.contract.sign_proposal(&t.admin2, &pause_id);

        // Rotate the signer set, removing admin2 (who approved the pause).
        let replacement = Address::generate(&t.env);
        let mut new_signers = Vec::new(&t.env);
        new_signers.push_back(t.admin1.clone());
        new_signers.push_back(t.admin3.clone());
        new_signers.push_back(replacement.clone());
        let update_id = t
            .contract
            .propose_update_multisig(&t.admin1, &new_signers, &2);
        t.contract.sign_proposal(&t.admin2, &update_id);
        t.contract.execute_proposal(&t.admin1, &update_id);

        // admin2 is gone from the signer set.
        let cfg = t.contract.get_multisig_admin().unwrap();
        assert!(!signer_present(&cfg.signers, &t.admin2));

        // VF-1187-2: the stale approval cast by the now-removed admin2 still
        // counts, and any current signer may execute the proposal.
        t.contract.execute_proposal(&t.admin3, &pause_id);
        assert!(t.contract.is_paused());
    }
}
