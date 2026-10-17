use soroban_sdk::{contracttype, Address, BytesN, Symbol};

use crate::invoice::{InvoiceStatus, ReferralCode};
use crate::oracle_registry::OracleFeedType;

/// Issue #640: emitted when a multisig-approved `RotateSigner` proposal
/// executes, scheduling (but not yet applying) the signer swap.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SignerRotationScheduled {
    pub old_signer: Address,
    pub new_signer: Address,
    pub effective_at: u64,
}

/// Issue #640: emitted once a scheduled rotation's timelock has elapsed
/// and the signer swap has actually been applied.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SignerRotationFinalized {
    pub old_signer: Address,
    pub new_signer: Address,
}

/// Issue #640: emitted when a pending signer rotation is cancelled before
/// it takes effect.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SignerRotationCancelled {
    pub old_signer: Address,
    pub new_signer: Address,
}

/// Emitted when an oracle is registered for a feed type, either as the
/// feed-type-wide default (`token: None`) or a per-token override
/// (`token: Some(..)`) (Issue #532).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleRegistered {
    pub feed_type: OracleFeedType,
    pub token: Option<Address>,
    pub oracle: Address,
}

/// Emitted when an oracle registration is removed (Issue #532).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleUnregistered {
    pub feed_type: OracleFeedType,
    pub token: Option<Address>,
}

/// Emitted every time `fund_invoice` (or another caller) queries an oracle,
/// recording its staleness at that moment (Issue #532).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleHealthRecorded {
    pub feed_type: OracleFeedType,
    pub token: Address,
    pub is_stale: bool,
    pub last_data_age_ledgers: u64,
    pub consecutive_stale_count: u32,
}

/// Emitted once when a feed type + token's oracle resolution channel is
/// automatically circuit-tripped after `MAX_CONSECUTIVE_STALE_QUERIES`
/// consecutive stale responses from the same oracle. `token` is included
/// alongside the task-specified `feed_type`/`consecutive_stale_count`
/// fields — matching `OracleHealthRecorded`'s shape — since a bare
/// feed-type-only event wouldn't identify which resolution channel tripped
/// in a deployment with multiple per-token overrides.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleCircuitTripped {
    pub feed_type: OracleFeedType,
    pub token: Address,
    pub consecutive_stale_count: u32,
}

/// Emitted when governance resets a tripped oracle circuit breaker.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleCircuitReset {
    pub feed_type: OracleFeedType,
    pub token: Address,
}

/// Emitted when a price source is added to a feed type's multi-source list
/// (Issue #price-deviation).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceSourceAdded {
    pub feed_type: OracleFeedType,
    pub oracle: Address,
}

/// Emitted when a price source is removed from a feed type's multi-source list.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceSourceRemoved {
    pub feed_type: OracleFeedType,
    pub oracle: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TwapEnabledForFeed {
    pub feed_type: OracleFeedType,
    pub enabled: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TwapWindowUpdated {
    pub old_window: u64,
    pub new_window: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TwapInsufficientData {
    pub feed_type: OracleFeedType,
    pub token: Address,
    pub observations: u32,
    pub min_required: u32,
}


/// Emitted when a registered price source's reported price is rejected as
/// an outlier relative to the cross-source median (Issue #price-deviation).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceOutlierRejected {
    pub feed_type: OracleFeedType,
    pub token: Address,
    pub oracle: Address,
    pub reported_price: i128,
    pub median_price: i128,
    pub deviation_bps: u32,
}

/// Emitted after `claim_default` attempts to compensate the claiming LP from
/// the configured insurance pool. `compensated` is `false` when the LP
/// wasn't enrolled, no pool is configured, or the pool call failed/was
/// unavailable (Issue #529).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InsuranceClaimAttempted {
    pub invoice_id: u64,
    pub lp: Address,
    pub compensated: bool,
    pub payout: i128,
}

/// Emitted when an enrolled LP's insurance `claim()` call fails (panic, error,
/// or incompatible pool) so operators can observe compensation failure
/// without the core default path reverting.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InsuranceCompensationFailed {
    pub invoice_id: u64,
    pub lp: Address,
    pub pool: Address,
}

/// Emitted when governance adds a token to the funding allowlist (Issue #19).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TokenAdded {
    pub token: Address,
    /// Number of decimal places for this token (e.g. 6 for USDC, 7 for XLM).
    pub decimals: u32,
}

/// Issue #1072: a payout to a frozen recipient was escrowed instead of
/// transferred.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PayoutEscrowed {
    pub invoice_id: u64,
    pub recipient: Address,
    pub token: Address,
    pub amount: i128,
    /// Recipient's total pending balance for this token after the credit.
    pub total_pending: i128,
}

/// Issue #1072: an escrowed payout was released to its recipient.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PendingPayoutClaimed {
    pub recipient: Address,
    pub token: Address,
    pub amount: i128,
}

/// Emitted when governance removes a token from the funding allowlist (Issue #19).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TokenRemoved {
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceSubmitted {
    pub invoice_id: u64,
    pub freelancer: Address,
    pub payer: Address,
    pub token: Address,
    pub amount: i128,
    pub due_date: u64,
    pub discount_rate: u32,
    pub referral_code: ReferralCode,
    pub status: InvoiceStatus,
    /// Ledger timestamp when the invoice was submitted.  Included so indexers
    /// can reconstruct the full invoice record from events alone.
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceUpdated {
    pub invoice_id: u64,
    pub freelancer: Address,
    pub payer: Address,
    pub token: Address,
    pub amount: i128,
    pub due_date: u64,
    pub discount_rate: u32,
    pub status: InvoiceStatus,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceFunded {
    pub invoice_id: u64,
    pub funder: Address,
    pub freelancer: Address,
    pub payer: Address,
    pub token: Address,
    pub fund_amount: i128,
    pub amount_funded: i128,
    pub invoice_amount: i128,
    pub due_date: u64,
    pub discount_rate: u32,
    pub funded_at: Option<u64>,
    pub status: InvoiceStatus,
    // NEW FIELDS
    pub lp: Address,
    pub effective_yield_bps: u32,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoicePaid {
    pub invoice_id: u64,
    pub payer: Address,
    pub lp: Address,
    pub freelancer: Address,
    pub token: Address,
    /// Full amount settled by payer
    pub amount_paid: i128,
    /// LP earnings = amount_paid - amount_funded
    pub lp_earned: i128,
    /// Total amount distributed to LP
    pub lp_payout: i128,
    /// Settlement ledger timestamp
    pub settlement_timestamp: u64,
    pub paid_on_time: bool,
    pub status: InvoiceStatus,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoicePartiallyPaid {
    pub invoice_id: u64,
    pub payer: Address,
    pub amount_paid_now: i128,
    pub total_amount_paid: i128,
    pub remaining_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractPaused {
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractUnpaused {
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceDefaulted {
    pub invoice_id: u64,
    pub funder: Address,
    pub freelancer: Address,
    pub payer: Address,
    pub token: Address,
    pub amount: i128,
    pub due_date: u64,
    pub defaulted_at: u64,
    pub discount_amount: i128,
    pub status: InvoiceStatus,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceTransferred {
    pub invoice_id: u64,
    pub old_freelancer: Address,
    pub new_freelancer: Address,
    pub status: InvoiceStatus,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceCancelled {
    pub invoice_id: u64,
    pub freelancer: Address,
    pub status: InvoiceStatus,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LPPositionTransferred {
    pub invoice_id: u64,
    pub old_lp: Address,
    pub new_lp: Address,
    pub status: InvoiceStatus,
}

/// Emitted whenever the contract admin address is updated.
/// Provides a permanent on-chain audit trail for admin transitions.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AdminChanged {
    pub old_admin: Address,
    pub new_admin: Address,
    /// Ledger timestamp of the change.
    pub timestamp: u64,
}

/// Emitted whenever a governance-controlled numeric parameter changes.
///
/// The `param_name` topic is a stable audit identifier. Keep these strings
/// unique per parameter so off-chain indexers can reconstruct config history.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ParameterUpdated {
    pub param_name: Symbol,
    pub old_value: i128,
    pub new_value: i128,
    pub updated_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractUpgraded {
    pub admin: Address,
    pub new_wasm_hash: BytesN<32>,
    pub timestamp: u64,
}

/// Emitted when the admin sets/changes the distribution contract address
/// (Issue #538: event emission completeness audit).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DistributionContractUpdated {
    pub old_distribution_contract: Option<Address>,
    pub new_distribution_contract: Address,
    pub updated_by: Address,
}

/// Emitted when the admin sets/changes the price oracle address
/// (Issue #538: event emission completeness audit).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceOracleUpdated {
    pub old_oracle: Option<Address>,
    pub new_oracle: Address,
    pub updated_by: Address,
}

/// Emitted once, when the contract is initialised
/// (Issue #538: event emission completeness audit).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractInitialized {
    pub admin: Address,
    pub usdc_token: Address,
    pub eurc_token: Address,
    pub xlm_token: Address,
    pub timestamp: u64,
}

// ── Issue #36: appeal_default events ──────────────────────────────────────────

/// Emitted when a payer files an appeal against an unfair default marking.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultAppealed {
    pub invoice_id: u64,
    pub payer: Address,
    /// SHA-256 hash of off-chain evidence provided by the payer.
    pub evidence_hash: BytesN<32>,
    pub appealed_at: u64,
}

/// Emitted when governance resolves a payer's appeal.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AppealResolved {
    pub invoice_id: u64,
    pub payer: Address,
    /// true = appeal upheld (default reversed); false = appeal rejected.
    pub upheld: bool,
    pub resolved_at: u64,
}

// ── Dispute events ──────────────────────────────────────────────────────────

/// Emitted when a payer disputes an invoice before settlement.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceDisputed {
    pub invoice_id: u64,
    pub payer: Address,
    /// SHA-256 hash of off-chain dispute evidence.
    pub reason_hash: BytesN<32>,
    pub disputed_at: u64,
}

/// Emitted when governance resolves a dispute.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DisputeResolved {
    pub invoice_id: u64,
    pub resolution_hash: BytesN<32>, // Optional hash of resolution details
    pub resolution: u32, // Ruling: 1 = Upheld (Payer right), 2 = Rejected (Freelancer right)
    pub resolved_at: u64,
}

/// Emitted when a dispute resolution in favor of the payer refunds partial payments to the payer.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DisputeUpheldPayerRefund {
    pub invoice_id: u64,
    pub payer: Address,
    pub amount: i128,
}

// ── Issue #34: LP priority queue events ───────────────────────────────────────

/// Emitted when an LP registers their intent to fund via the priority queue.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FundRequested {
    pub invoice_id: u64,
    pub lp: Address,
    /// LP's reputation score at the time of registration.
    pub score: u32,
}

/// Emitted when the priority queue is resolved and a winning LP is selected.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FundQueueResolved {
    pub invoice_id: u64,
    pub approved_lp: Address,
    /// Winning score that secured priority.
    pub score: u32,
}

/// Emitted whenever `resolve_fund_queue` is called, regardless of outcome.
/// `success=true` means a winner was selected; `success=false` means the
/// call was rejected (e.g. maturity delay not yet elapsed).
///
/// Useful for off-chain monitoring to detect MEV attempts and track queue
/// activity (Issue #MEV-1).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FundQueueResolutionAttempted {
    pub invoice_id: u64,
    /// Caller that triggered the resolution attempt.
    pub caller_ledger: u32,
    /// Ledger sequence when the attempt was made.
    pub attempted_at_ledger: u32,
    /// Whether the resolution succeeded.
    pub success: bool,
}

/// Emitted by `recover_fund_queue` (Issues #1020/#1021). `evicted_lp` is the
/// stalled approved LP removed from the queue (if any); `purged` is true when
/// the invoice was no longer fundable and all queue state was deleted rather
/// than reopened for the remaining `remaining_lps`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FundQueueRecovered {
    pub invoice_id: u64,
    pub evicted_lp: Option<Address>,
    pub remaining_lps: u32,
    pub purged: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceExpired {
    pub invoice_id: u64,
    pub freelancer: Address,
    pub status: InvoiceStatus,
}

/// Emitted when an address's reputation score or counters are updated (Issue #32).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ReputationUpdated {
    pub address: Address,
    pub old_score: u32,
    pub new_score: u32,
    pub invoices_submitted: u32,
    pub invoices_paid: u32,
    pub invoices_defaulted: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceTokenChanged {
    pub invoice_id: u64,
    pub old_token: Address,
    pub new_token: Address,
}

// ── Invoice NFT lifecycle events (nft.rs) ─────────────────────────────────────

/// Emitted when an invoice NFT is minted (invoice submitted).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceNftMinted {
    pub invoice_id: u64,
    pub owner: Address,
    pub amount: i128,
    pub due_date: u32,
    pub timestamp: u64,
}

/// Emitted when an invoice NFT is transferred (e.g. freelancer → LP on funding).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceNftTransferred {
    pub invoice_id: u64,
    pub from: Address,
    pub to: Address,
    pub timestamp: u64,
}

/// Emitted when an invoice NFT is burned (invoice marked paid).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceNftBurned {
    pub invoice_id: u64,
    pub owner: Address,
    pub timestamp: u64,
}

// ── Issue #1083: Rollback rehearsal events ────────────────────────────────────

/// The outcome of a `rehearse_rollback()` call.
///
/// Returned by `rehearse_rollback()` and included in the
/// `RollbackRehearsalCompleted` event so off-chain monitors can assess
/// rollback readiness without replaying the full storage scan.
///
/// # Fields
///
/// * `invoices_checked`   — total number of invoice IDs sampled
///   (capped at `ROLLBACK_SAMPLE_SIZE`).
/// * `split_format_count` — invoices stored as `InvoiceCore` + `InvoiceMetadata`
///   (new format, both halves present).
/// * `legacy_format_count`— invoices stored as monolithic `Invoice` key
///   (old format, fully rollback-safe by itself).
/// * `orphaned_count`     — invoices with only one half of the split format
///   (either `InvoiceCore` without `InvoiceMetadata`, or vice versa).
///   **Any non-zero value means rollback is unsafe.**
/// * `rollback_safe`      — `true` iff `orphaned_count == 0` AND every
///   checked invoice satisfies at least one of the two safe invariants.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RollbackRehearsalReport {
    pub invoices_checked: u32,
    pub split_format_count: u32,
    pub legacy_format_count: u32,
    pub orphaned_count: u32,
    pub rollback_safe: bool,
}

/// Emitted by `rehearse_rollback()` when the rollback safety scan completes.
///
/// The `rollback_safe` field mirrors `RollbackRehearsalReport::rollback_safe`
/// and is surfaced as a top-level event field so indexers can filter on it
/// without deserializing the full report body.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RollbackRehearsalCompleted {
    pub admin: Address,
    pub report: RollbackRehearsalReport,
    pub timestamp: u64,
}

// ----------------------------------------------------------------
// Issue #1031: Equal-score manipulation economics simulation
// ----------------------------------------------------------------

/// Emitted by `simulate_equal_score_attack()`.
///
/// Describes the economic cost–benefit breakdown of an equal-score
/// queue manipulation attempt. Off-chain analytics and dashboards can
/// consume this event to monitor whether the configured `tie_score_margin`
/// is producing an adequate deterrent.
///
/// # Fields
/// * `attacker_score`     — score the simulated attacker starts with
/// * `defender_score`     — score of the legitimate LP being challenged
/// * `n_attackers`        — number of colluding attacker accounts in the simulation
/// * `invoices_to_tie`    — funded invoices needed to reach `defender_score`
///                          (0 when `attacker_score >= defender_score`)
/// * `invoices_to_beat`   — funded invoices needed to outscore `defender_score`
///                          by at least 1 (including any active `tie_score_margin`)
/// * `tie_win_probability_pct` — expected win rate at `defender_score` with
///                          `n_attackers` colluding (0 when margin prevents joining)
/// * `tie_score_margin`   — active margin at simulation time
/// * `attacker_can_join`  — `true` iff the attacker score satisfies the margin
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct EqualScoreManipulationSimulated {
    pub attacker_score: u32,
    pub defender_score: u32,
    pub n_attackers: u32,
    pub invoices_to_tie: u32,
    pub invoices_to_beat: u32,
    pub tie_win_probability_pct: u32,
    pub tie_score_margin: u32,
    pub attacker_can_join: bool,
}

/// Emitted by `set_queue_tie_score_margin()` when an admin updates the margin.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct QueueTieScoreMarginUpdated {
    pub old_margin: u32,
    pub new_margin: u32,
    pub updated_by: Address,
}
