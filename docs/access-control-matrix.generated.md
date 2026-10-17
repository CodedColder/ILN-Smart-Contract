<!-- GENERATED FILE — do not edit by hand. -->
<!-- Regenerate: python3 scripts/generate-access-control-matrix.py -->
<!-- CI fails if this drifts from #[contractimpl] entry points (Issue #856). -->

# Access Control Matrix (Generated)

Derived automatically from `#[contractimpl]` `pub fn` entry points by scanning
for `require_admin`, `require_auth`, and `check_rate_limit` in each function body.

Role inference is intentionally mechanical:

- **Admin** — body calls `require_admin`
- **Caller (require_auth)** — body calls `require_auth` / `.require_auth()` without `require_admin`
- **Anyone** — neither guard present (views / permissionless helpers)
- **rate-limited** — body also calls `check_rate_limit`

Narrative context, audit findings, and pause semantics remain in
[`access-control.md`](access-control.md). If this file and the hand-written
doc disagree on a function's gate, **believe the generated matrix** and update
the narrative.

## `invoice_liquidity`

| Instruction | Inferred gate | Source |
| ----------- | ------------- | ------ |
| `add_price_source` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `add_token` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `appeal_default` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `auto_resolve_dispute` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `cancel_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `cancel_signer_rotation` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `check_oracle_health` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `claim_default` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `claim_pending_payout` | Caller (require_auth) | `contracts/invoice_liquidity/src/lib.rs` |
| `claim_yield` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `convert_invoice_token` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `dispute_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `execute_proposal` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `expire_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `finalize_signer_rotation` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `fund_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_config` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_contract_stats` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_fee_tiers` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_insurance_pool` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_invoice_count` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_max_oracle_age` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_max_price_deviation_bps` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_multisig_admin` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_multisig_proposal` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_oracle_for_token` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_oracle_health` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_pending_payout` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_pending_signer_rotation` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_price_oracle` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_price_sources` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_protocol_status` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_recent_admin_actions` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_referral_stats` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_reputation` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_storage_version` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_token_decimals` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_top_payers` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_twap_price` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_twap_window` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_verified_price` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `get_version` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `initialize` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `initialize_multisig_admin` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `is_oracle_circuit_tripped` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `is_paused` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `is_twap_enabled` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `join_fund_queue` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `list_invoices_by_lp` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `list_invoices_by_submitter` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `lp_score` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `mark_paid` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `max_invoice_amount` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `migrate` | Admin | `contracts/invoice_liquidity/src/lib.rs` |
| `min_payer_reputation` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `pause` | Admin | `contracts/invoice_liquidity/src/lib.rs` |
| `payer_score` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `propose_pause` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `propose_remove_token` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `propose_rotate_signer` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `propose_set_fee_rate` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `propose_set_max_discount` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `propose_unpause` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `propose_update_multisig` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `query_nft_metadata` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `query_nft_owner` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `record_twap_sample` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `register_oracle` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `register_token_oracle` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `remove_oracle` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `remove_price_source` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `remove_token` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `remove_token_oracle` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `reset_oracle_circuit` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `resolve_appeal` | Admin | `contracts/invoice_liquidity/src/lib.rs` |
| `resolve_dispute` | Admin | `contracts/invoice_liquidity/src/lib.rs` |
| `resolve_fund_queue` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `set_admin` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_distribution_contract` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_insurance_pool` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_max_invoice_amount` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_max_oracle_age` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_max_price_deviation_bps` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `set_min_payer_reputation` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_price_oracle` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_token_volume_cap` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `set_twap_enabled` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `set_twap_window` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `sign_proposal` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `submit_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `submit_invoices_batch` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `suggested_discount_rate` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `token_volume_cap` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `transfer_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `transfer_lp_position` | Caller (require_auth) | `contracts/invoice_liquidity/src/lib.rs` |
| `unpause` | Admin | `contracts/invoice_liquidity/src/lib.rs` |
| `update_config` | Anyone; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `update_decay_params` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `update_fee_rate` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `update_fee_tiers` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `update_invoice` | Anyone | `contracts/invoice_liquidity/src/lib.rs` |
| `update_max_discount` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |
| `upgrade` | Admin; rate-limited | `contracts/invoice_liquidity/src/lib.rs` |

## `iln_governance`

| Instruction | Inferred gate | Source |
| ----------- | ------------- | ------ |
| `cast_vote` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `checkpoint_balance` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `configure_veto_multisig` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `create_proposal` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `delegate_votes` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `disable_veto_power` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `execute_proposal` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_applied_vote_weight` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_delegate` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_execution_delay` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_gov_token_total_supply` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_max_delegation_depth` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_min_proposal_balance` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_min_proposal_deposit` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_min_quorum_bps` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_proposal` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_proposal_created_ledger` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_proposal_deposit` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_proposal_deposit_sink` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_veto_approvals` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_veto_signers` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_veto_threshold` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `get_voter_checkpoint` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `has_voted` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `initialize` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `is_proposal_deposit_settled` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `is_quadratic_voting_enabled` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `is_veto_power_enabled` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `list_proposals` | Anyone | `contracts/iln_governance/src/lib.rs` |
| `set_execution_delay` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `set_gov_token_total_supply` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `set_max_delegation_depth` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `set_min_proposal_balance` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `set_min_proposal_deposit` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `set_min_quorum_bps` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `set_proposal_deposit_sink` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `set_quadratic_voting_enabled` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `undelegate_votes` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |
| `veto_proposal` | Caller (require_auth) | `contracts/iln_governance/src/lib.rs` |

## `iln_distribution`

| Instruction | Inferred gate | Source |
| ----------- | ------------- | ------ |
| `accrue_lp` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `accrue_settlement` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `claim_tokens` | Caller (require_auth) | `contracts/iln_distribution/src/lib.rs` |
| `get_accrual` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `get_freelancer_reward_rate` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `get_lp_reward_rate` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `get_payer_reward_rate` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `initialize` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `set_freelancer_reward_rate` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `set_lp_reward_rate` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `set_payer_reward_rate` | Anyone | `contracts/iln_distribution/src/lib.rs` |
| `verify_mint_authority` | Anyone | `contracts/iln_distribution/src/lib.rs` |

## `insurance_pool`

| Instruction | Inferred gate | Source |
| ----------- | ------------- | ------ |
| `calculate_premium_amount` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `calculate_premium_rate_bps` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `cancel_admin_transfer` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `cancel_coverage_change` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `cancel_risk_multiplier` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `execute_admin_transfer` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `execute_coverage_change` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `execute_risk_multiplier` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_backstop_balance` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_backstop_funding_bps` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_balance_cap` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_base_premium_rate_bps` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_claim_count` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_claim_evidence` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_coverage` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_default_count` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_min_reserve_ratio_bps` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_pair_collusion_flag` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_pair_default_count` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_pending_admin` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_pending_coverage` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_pool_health` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_premiums_paid` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_reputation_contract` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_reserve_ratio_bps` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_review_window_seconds` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_risk_multiplier_denominator` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_risk_multiplier_numerator` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_tiered_coverage` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_token_address` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `get_total_reserve` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `increment_default_count` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `initialize` | Caller (require_auth) | `contracts/insurance_pool/src/lib.rs` |
| `is_claimed` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `is_solvency_circuit_open` | Anyone | `contracts/insurance_pool/src/lib.rs` |
| `propose_admin_transfer` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `propose_coverage_change` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `propose_risk_multiplier` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `record_pair_default` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `reset_solvency_circuit` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `set_backstop_funding_bps` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `set_balance_cap` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `set_base_premium_rate_bps` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `set_coverage_via_governance` | Caller (require_auth) | `contracts/insurance_pool/src/lib.rs` |
| `set_min_reserve_ratio_bps` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `set_premium_rate_via_governance` | Caller (require_auth) | `contracts/insurance_pool/src/lib.rs` |
| `set_reputation_contract` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `set_review_window_seconds` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `set_risk_multiplier` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `submit_claim_evidence` | Admin | `contracts/insurance_pool/src/lib.rs` |
| `top_up_backstop` | Admin | `contracts/insurance_pool/src/lib.rs` |

## `reputation_bonus`

| Instruction | Inferred gate | Source |
| ----------- | ------------- | ------ |
| `get_config` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
| `get_reputation` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
| `handle_default` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
| `init` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
| `mark_paid` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
| `set_config` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
| `submit_invoice` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
| `update_config` | Anyone | `contracts/reputation_bonus/src/lib.rs` |
