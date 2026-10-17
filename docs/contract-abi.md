# Contract ABI Documentation

## InvoiceLiquidityContract

### Functions

| Function | Parameters | Returns | Description |
|----------|------------|---------|-------------|
| initialize | env: Env, admin: Address, usdc_token: Address, eurc_token: Address, xlm_token: Address, | Result<(), ContractError> | Access: Anyone |
| get_version | env: Env | soroban_sdk::String | Access: Anyone |
| set_admin | env: Env, new_admin: Address | Result<(), ContractError> | Access: Admin only |
| update_fee_rate | env: Env, rate: u32 | Result<(), ContractError> | Access: Admin only |
| update_max_discount | env: Env, rate: u32 | Result<(), ContractError> | Access: Admin only |
| set_distribution_contract | env: Env, distribution_contract: Address, | Result<(), ContractError> | Access: Admin only |
| set_price_oracle | env: Env, oracle: Address | Result<(), ContractError> | Access: Admin only |
| get_price_oracle | env: Env | Option<Address> | Access: Anyone |
| set_max_oracle_age | env: Env, max_age_ledgers: u64 | Result<(), ContractError> | Access: Admin only |
| get_max_oracle_age | env: Env | u64 | Access: Anyone |
| add_token | env: Env, token: Address, decimals: u32 | Result<(), ContractError> | Access: Admin only |
| remove_token | env: Env, token: Address | Result<(), ContractError> | Access: Admin only |
| get_token_decimals | env: Env, token: Address | Option<u32> | /// Access: Anyone |
| audit_token_decimals | env: Env, token: Address | token_safety::TokenDecimalsAudit | Access: Anyone |
| correct_token_decimals | env: Env, token: Address, decimals: u32 | Result<(), ContractError> | Access: Admin only |
| pause | env: Env | Result<(), ContractError> | Access: Admin only |
| unpause | env: Env | Result<(), ContractError> | Access: Admin only |
| upgrade | env: Env, new_wasm_hash: BytesN<32> | Result<(), ContractError> | /// Access: Admin only |
| get_contract_stats | env: Env | ContractStats | Access: Anyone |
| list_invoices_by_submitter | env: Env, submitter: Address, page: u32, page_size: u32, | Vec<Invoice> | Access: Anyone |
| list_invoices_by_lp | env: Env, lp: Address, page: u32, page_size: u32 | Vec<Invoice> | Access: Anyone |
| submit_invoice | env: Env, freelancer: Address, payer: Address, amount: i128, due_date: u64, discount_rate: u32, token: Address, referral_code: ReferralCode, | Result<u64, ContractError> | Access: Submitter only |
| update_invoice | env: Env, freelancer: Address, invoice_id: u64, amount: i128, due_date: u64, discount_rate: u32, | Result<(), ContractError> | Access: Submitter only |
| convert_invoice_token | env: Env, freelancer: Address, invoice_id: u64, new_token: Address, | Result<(), ContractError> | Access: Submitter only |
| submit_invoices_batch | env: Env, invoices: Vec<InvoiceParams>, | Result<Vec<u64>, ContractError> | Access: Submitter only |
| get_referral_stats | env: Env, code: BytesN<32> | u64 | Access: Anyone |
| join_fund_queue | env: Env, lp: Address, invoice_id: u64 | Result<(), ContractError> | Access: LP only |
| resolve_fund_queue | env: Env, invoice_id: u64 | Result<Address, ContractError> | Access: Anyone |
| get_fund_queue_ttl_margins | env: Env, invoice_id: u64 | FundQueueTtlMargins | Access: Anyone |
| fund_invoice | env: Env, funder: Address, invoice_id: u64, fund_amount: i128, require_oracle_verification: bool, | Result<(), ContractError> | consulted and the existing behaviour is preserved. |
| transfer_invoice | env: Env, invoice_id: u64, new_freelancer: Address, | Result<(), ContractError> | Access: Submitter only |
| transfer_lp_position | env: Env, invoice_id: u64, new_lp: Address, | Result<(), ContractError> | Access: Current LP only |
| cancel_invoice | env: Env, invoice_id: u64 | Result<(), ContractError> | Access: Submitter only |
| expire_invoice | env: Env, invoice_id: u64 | Result<(), ContractError> | Access: Anyone |
| mark_paid | env: Env, invoice_id: u64, amount: i128 | Result<(), ContractError> | Access: Payer only |
| claim_yield | env: Env, invoice_id: u64 | Result<i128, ContractError> | Access: LP only |
| claim_default | env: Env, funder: Address, invoice_id: u64 | Result<(), ContractError> | Access: LP only |
| appeal_default | env: Env, invoice_id: u64, evidence_hash: BytesN<32>, | Result<(), ContractError> | Access: Payer only |
| resolve_appeal | env: Env, invoice_id: u64, upheld: bool | Result<(), ContractError> | Access: Admin only |
| dispute_invoice | env: Env, invoice_id: u64, reason_hash: BytesN<32>, | Result<(), ContractError> | Access: Payer only |
| resolve_dispute | env: Env, invoice_id: u64, resolution_hash: BytesN<32>, resolution: u32, | Result<(), ContractError> | Access: Admin only |
| auto_resolve_dispute | env: Env, invoice_id: u64 | Result<(), ContractError> | Access: Anyone |
| update_config | env: Env, caller: Address, high_rep_threshold: u32, bonus_bps: u32, min_discount_rate_bps: u32, decay_rate_bps: u32, decay_period_ledgers: u64, dispute_timeout_ledgers: u64, xlm_sac_address: Address, usdc_sac_address: Address, eurc_sac_address: Address, | Result<(), ContractError> | No description |
| get_config | env: Env | Result<Config, ContractError> | No description |
| payer_score | env: Env, payer: Address | u32 | Access: Anyone |
| lp_score | env: Env, lp: Address | u32 | Access: Anyone |
| get_top_payers | env: Env, limit: u32 | Vec<TopPayerEntry> | Access: Anyone |
| get_reputation | env: Env, address: Address | ReputationProfile | Access: Anyone |
| min_payer_reputation | env: Env | u32 | Access: Anyone |
| set_min_payer_reputation | env: Env, value: u32 | Result<(), ContractError> | Access: Admin only |
| suggested_discount_rate | env: Env, payer: Address | u32 | Access: Anyone |
| get_invoice | env: Env, invoice_id: u64 | Result<Invoice, ContractError> | Access: Anyone |
| get_invoice_count | env: Env | u64 | Access: Anyone |
| query_nft_metadata | env: Env, invoice_id: u64 | Option<crate::nft::InvoiceNftMetadata> | Anyone |
| query_nft_owner | env: Env, invoice_id: u64 | Option<Address> | Anyone |

### Contract Errors

- InvoiceNotFound = 1
- AlreadyFunded = 2
- AlreadyPaid = 3
- NotFunded = 4
- Unauthorized = 5
- InvalidAmount = 6
- InvalidDiscountRate = 7
- InvalidDueDate = 8
- InvoiceDefaulted = 9
- NothingToClaim = 10
- NotYetDefaulted = 11
- OverfundingRejected = 12
- InvoiceExpired = 13
- BatchTooLarge = 14
- AlreadyCancelled = 15
- AlreadyInitialized = 16
- AlreadyAppealed = 17
- AppealWindowClosed = 18
- NotDefaulted = 19
- AlreadyInQueue = 20
- NotApprovedFunder = 21
- InvoiceAppealed = 22
- AlreadyDisputed = 23
- NotDisputed = 24
- InvoiceDisputed = 25
- ContractPaused = 26
- DueDateTooSoon = 27
- DueDateTooFar = 28
- SelfInvoice = 29
- OverpaymentRejected = 30
- PayerReputationTooLow = 31
- ArithmeticOverflow = 32
- FeeOnTransferToken = 33
- PayerUnverified = 34
- OracleDataStale = 35
- AmountTooSmall = 36

---

## InsurancePool

### Functions

| Function | Parameters | Returns | Description |
|----------|------------|---------|-------------|
| initialize | env: Env, admin: Address, coverage: i128 | Result<(), InsuranceError> | * `coverage` — flat per-claim compensation cap (in token stroops). |
| get_premiums_paid | env: Env, lp: Address | i128 | Total premium an LP has contributed over the pool's lifetime. |
| get_coverage | env: Env | i128 | The configured flat per-claim coverage cap. |
| is_claimed | env: Env, invoice_id: u64 | bool | Returns `true` if a claim has already been processed for `invoice_id`. |
| propose_coverage_change | env: Env, new_coverage: i128 | Result<u64, InsuranceError> | any previously pending coverage proposal. |
| execute_coverage_change | env: Env | Result<(), InsuranceError> | expired. Callable by anyone once the delay has elapsed. |
| cancel_coverage_change | env: Env | Result<(), InsuranceError> | Cancel a pending coverage change proposal. Requires current admin auth. |
| propose_admin_transfer | env: Env, new_admin: Address | Result<u64, InsuranceError> | previously pending admin proposal. |
| execute_admin_transfer | env: Env | Result<(), InsuranceError> | expired. Callable by anyone once the delay has elapsed. |
| cancel_admin_transfer | env: Env | Result<(), InsuranceError> | Cancel a pending admin transfer proposal. Requires current admin auth. |
| get_pending_coverage | env: Env | Option<(i128, u64)> | Returns the pending coverage proposal (new cap, eta), if any. |
| get_pending_admin | env: Env | Option<(Address, u64)> | Returns the pending admin transfer proposal (new admin, eta), if any. |

### Contract Errors

- NotInitialized = 1
- AlreadyClaimed = 2
- InvalidAmount = 3
- PoolEmpty = 4
- AlreadyInitialized = 5
- NoPendingProposal = 6
- TimelockNotExpired = 7

---
