#![no_std]

//! Default-protection insurance pool — stub implementation (Issue #123).
//!
//! Liquidity providers (LPs) optionally opt into this pool by paying premiums.
//! When an invoice they funded defaults, the pool compensates them out of the
//! accumulated premium balance (up to a flat per-claim coverage cap).
//!
//! This is a **design-forward stub**: it implements the full
//! [`InsurancePoolInterface`] with correct storage, auth, events and accounting
//! semantics, but deliberately keeps the economics simple:
//!   * Premiums are tracked as pool *accounting* balance rather than via an
//!     actual token transfer (token settlement is a follow-up).
//!   * Compensation is a flat per-claim cap configured at init, not a
//!     risk-priced payout.
//!
//! See `docs/insurance-pool-design.md` for the integration design and the
//! follow-up work needed before mainnet.

#[cfg(test)]
extern crate std;

mod claim_prioritization;
mod insurance_interface;
#[cfg(test)]
mod test;

pub use insurance_interface::{
    InsurancePoolInterface, InsurancePoolInterfaceClient, INSURANCE_INTERFACE_VERSION,
};

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, BytesN, Env, IntoVal, Symbol, Val,
};

/// Minimum defaults on a single (lp, payer) pair before the collusion
/// heuristic considers the pair worth flagging for off-chain monitoring
/// (Issue #829).
pub const MIN_PAIR_DEFAULTS_TO_FLAG: u32 = 3;

/// Minimum fraction (in bps) of an LP's total defaults that must be
/// attributed to one payer for the collusion heuristic to flag the pair —
/// i.e. at least 50% of an LP's defaults concentrating on a single payer.
pub const COLLUSION_PAIR_SHARE_FLAG_BPS: u32 = 5_000;

/// Timelock delay (in seconds) enforced between proposing and executing an
/// admin action, and before which a proposal may be cancelled (Issue #542).
pub const TIMELOCK_DELAY_SECONDS: u64 = 3 * 24 * 60 * 60; // 3 days

/// Seconds in an average month, used to monthly-ize historical claim data
/// for `get_pool_health`'s solvency estimate. A simplification (not
/// calendar-accurate) — fine for a rough runway estimate.
const SECONDS_PER_MONTH: u64 = 30 * 24 * 60 * 60;

/// Errors surfaced by the insurance pool stub.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum InsuranceError {
    /// Contract has not been initialised with an admin.
    NotInitialized = 1,
    /// A claim has already been processed for this invoice.
    AlreadyClaimed = 2,
    /// Premium / coverage amount must be positive.
    InvalidAmount = 3,
    /// Pool has no balance available to pay a claim.
    PoolEmpty = 4,
    /// Contract is already initialised.
    AlreadyInitialized = 5,
    /// No pending proposal exists for the requested admin action.
    NoPendingProposal = 6,
    /// The proposal's timelock has not yet expired.
    TimelockNotExpired = 7,
    /// A checked arithmetic operation overflowed.
    ArithmeticOverflow = 8,
    /// Premium deposit would exceed the configured pool balance cap.
    BalanceCapExceeded = 9,
    /// A new claim payout has been paused: the pool's reserve ratio is below
    /// the governance-configured minimum and the solvency circuit breaker is
    /// open (Issue #826). Only a governance `reset_solvency_circuit` resumes
    /// payouts.
    SolvencyCircuitOpen = 10,
    /// The minimum reserve ratio must be within 0..=10_000 bps (Issue #826).
    InvalidReserveRatio = 11,
    /// A claim gated by the review window was attempted before on-chain
    /// evidence was submitted for the invoice (Issue #828).
    EvidenceRequired = 12,
    /// A claim gated by the review window was attempted before the
    /// governance-configured review window elapsed (Issue #828).
    ReviewWindowNotElapsed = 13,
    /// A backstop top-up was requested for an amount exceeding what the
    /// funding source holds / is invalid (Issue #827).
    InvalidBackstopAmount = 14,
}

/// Storage keys for the pool.
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Admin authorised to report confirmed defaults (the liquidity contract).
    Admin,
    /// Total pool balance (sum of premiums minus payouts).
    Balance,
    /// Flat per-claim coverage cap configured at init.
    Coverage,
    /// Token address for real transfers (Issue #527).
    TokenAddress,
    /// Enrollment flag per LP.
    Enrolled(Address),
    /// Cumulative premium paid per LP.
    Premiums(Address),
    /// Whether a claim has been processed for a given invoice id.
    Claimed(u64),
    /// Proposed new coverage cap awaiting timelock expiry (Issue #542).
    PendingCoverage,
    /// Proposed new admin awaiting timelock expiry (Issue #542).
    PendingAdmin,
    /// Ledger timestamp at which the pending coverage change becomes executable.
    CoverageEta,
    /// Ledger timestamp at which the pending admin transfer becomes executable.
    AdminEta,
    /// Base premium rate in basis points (e.g., 500 = 5%) (Issue #528).
    BasePremiumRateBps,
    /// Risk multiplier numerator for premium calculation (Issue #528).
    RiskMultiplierNumerator,
    /// Risk multiplier denominator for premium calculation (Issue #528).
    RiskMultiplierDenominator,
    /// Pending (timelocked) risk multiplier numerator (Issue #542/#528).
    PendingRiskMultiplierNumerator,
    /// Pending (timelocked) risk multiplier denominator (Issue #542/#528).
    PendingRiskMultiplierDenominator,
    /// Ledger timestamp at which the pending risk multiplier becomes executable.
    RiskMultiplierEta,
    /// LP's historical default count (Issue #528).
    DefaultCount(Address),
    /// LP's historical claim count (Issue #528).
    ClaimCount(Address),
    /// Tiered coverage caps: (tier_threshold, coverage_amount) (Issue #528).
    CoverageTiers,
    /// Optional maximum pool balance cap (governance-configurable).
    BalanceCap,
    /// Ledger timestamp the pool was initialized at (Issue #pool-health).
    InitializedAt,
    /// Count of distinct LPs currently enrolled (Issue #pool-health) — since
    /// Soroban storage can't be iterated, this running counter is
    /// maintained alongside the per-LP `Enrolled` flag rather than derived.
    EnrolledCount,
    /// Running total of confirmed defaults across all LPs (Issue
    /// #pool-health) — a pool-wide rollup of the per-LP `DefaultCount`
    /// already tracked for risk-priced premiums (Issue #528), used to
    /// estimate the pool's claim rate.
    TotalDefaultCount,
    /// Minimum pool reserve ratio (bps, balance vs coverage cap) below
    /// which new claim payouts are paused (Issue #826).
    MinReserveRatioBps,
    /// Whether the solvency circuit breaker is currently open; sticky until
    /// a governance `reset_solvency_circuit` clears it (Issue #826).
    SolvencyCircuitOpenFlag,
    /// Accounting balance held aside as the protocol's capital backstop,
    /// separate from the liquid claim `Balance` (Issue #827).
    BackstopBalance,
    /// Share (bps) of each premium deposit diverted to the backstop fund
    /// (Issue #827). `0` disables automatic backstop contributions.
    BackstopFundingBps,
    /// On-chain evidence hash attached to a claim, plus when it was
    /// submitted (Issue #828).
    ClaimEvidence(u64),
    /// Governance-configurable review window (seconds) that a claim must
    /// sit in after evidence submission before payout (Issue #828);
    /// `0` disables the gate.
    ReviewWindowSeconds,
    /// Default counter for a specific (lp, payer) pair, the raw data
    /// surfaced for the collusion heuristic (Issue #829).
    PairDefaultCount(Address, Address),
    /// Address of the reputation_bonus contract for cross-contract
    /// reputation reads (ADR-015). If not set, premium calculation
    /// ignores reputation scores.
    ReputationContract,
}

/// Solvency snapshot returned by `get_pool_health`, so LPs can judge
/// coverage capacity against enrolled exposure rather than reading raw
/// balance alone.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PoolHealth {
    /// Current pool balance (premiums collected minus payouts).
    pub balance: i128,
    /// Number of distinct LPs currently enrolled in default protection.
    pub enrolled_lp_count: u32,
    /// Estimated token outflow per month from claims, projected from the
    /// pool's historical default count times the flat coverage cap. `0`
    /// when there's no default history yet.
    pub estimated_monthly_claim_rate: i128,
    /// How many months the current balance would last at
    /// `estimated_monthly_claim_rate`. `None` when there's no claim
    /// history to project a rate from — this means "not yet estimable",
    /// not "infinite coverage".
    ///
    /// Named `months_of_coverage` rather than the fuller
    /// `months_of_coverage_at_current_rate` because Soroban's
    /// `#[contracttype]` caps struct field names at 30 characters.
    pub months_of_coverage: Option<u32>,
}

/// Payload of the `solvency_tripped` event (Issue #826), emitted when the
/// governance-configured minimum reserve ratio is breached and new claim
/// payouts are automatically paused.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SolvencyCircuitTripped {
    /// Pool reserve ratio (bps, balance vs coverage cap) at the moment of
    /// the trip. Always below the configured `MinReserveRatioBps`.
    pub ratio_bps: u32,
    /// Liquid reserve (pool `Balance`) observed at the trip.
    pub reserve: i128,
}

/// Payload of the `solvency_reset` event (Issue #826), emitted when a
/// governance action resumes claim payouts after a circuit trip.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SolvencyCircuitReset {
    /// Reserve ratio at the moment of the reset.
    pub ratio_bps: u32,
    /// Liquid reserve (pool `Balance`) observed at the reset.
    pub reserve: i128,
}

/// On-chain evidence attached to a defaulted invoice's claim (Issue #828).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimEvidence {
    /// Hash (32 bytes) of the off-chain fraud/legitimacy documentation
    /// (invoice, evidence of payment attempt) backing this claim.
    pub evidence_hash: BytesN<32>,
    /// Ledger timestamp at which the evidence was submitted.
    pub submitted_at: u64,
}

/// Payload of the `pair_default_recorded` event (Issue #829), surfacing
/// per-(lp, payer) pair default counts for the off-chain collusion
/// detector.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PairDefaultRecorded {
    pub lp: Address,
    pub payer: Address,
    /// New default count for this specific pair.
    pub pair_count: u32,
}

/// Payload of the `backstop_topup` event (Issue #827).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BackstopTopUp {
    pub from: Address,
    pub amount: i128,
    /// New backstop balance after the top-up.
    pub backstop_balance: i128,
}

#[contract]
pub struct InsurancePool;

#[contractimpl]
impl InsurancePool {
/// Initialise the pool.
///
/// # Arguments
/// * `env` — host environment
/// * `admin` — see signature
/// * `coverage` — see signature
/// * `token` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn initialize(
        env: Env,
        admin: Address,
        coverage: i128,
        token: Address,
    ) -> Result<(), InsuranceError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(InsuranceError::AlreadyInitialized);
        }
        if coverage <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        admin.require_auth();
        let storage = env.storage().instance();
        storage.set(&DataKey::Admin, &admin);
        storage.set(&DataKey::Balance, &0i128);
        storage.set(&DataKey::Coverage, &coverage);
        storage.set(&DataKey::TokenAddress, &token);
        storage.set(&DataKey::InitializedAt, &env.ledger().timestamp());

        env.events()
            .publish((symbol_short!("init"), admin), coverage);
        Ok(())
    }

/// Total premium an LP has contributed over the pool's lifetime.
///
/// Access: Anyone
pub fn get_premiums_paid(env: Env, lp: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Premiums(lp))
            .unwrap_or(0)
    }

/// The configured flat per-claim coverage cap.
///
/// Access: Anyone
pub fn get_coverage(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::Coverage)
            .unwrap_or(0)
    }

/// The configured token address for real transfers (Issue #527).
///
/// Access: Anyone
pub fn get_token_address(env: Env) -> Result<Address, InsuranceError> {
        env.storage()
            .instance()
            .get(&DataKey::TokenAddress)
            .ok_or(InsuranceError::NotInitialized)
    }

    // ── Issue #528: risk-priced insurance premiums ───────────────────────
    //
    // Premiums are calculated based on LP's historical default rate.
    // Higher risk = higher premiums, lower risk = lower premiums.
    // This creates incentives for LPs to fund high-quality invoices.

/// Get the base premium rate in basis points (e.g., 500 = 5%).
///
/// Access: Anyone
pub fn get_base_premium_rate_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::BasePremiumRateBps)
            .unwrap_or(500) // Default 5%
    }

/// Set the base premium rate in basis points. Requires admin auth.
///
/// # Arguments
/// * `env` — host environment
/// * `rate_bps` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn set_base_premium_rate_bps(env: Env, rate_bps: u32) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        if rate_bps == 0 || rate_bps > 10_000 {
            return Err(InsuranceError::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::BasePremiumRateBps, &rate_bps);
        Ok(())
    }

/// Get the risk multiplier numerator for premium calculation.
///
/// Access: Anyone
pub fn get_risk_multiplier_numerator(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::RiskMultiplierNumerator)
            .unwrap_or(1) // Default 1x
    }

/// Get the risk multiplier denominator for premium calculation.
///
/// Access: Anyone
pub fn get_risk_multiplier_denominator(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::RiskMultiplierDenominator)
            .unwrap_or(1) // Default 1/1
    }

/// Set the risk multiplier for premium calculation. Requires admin auth.
///
/// # Arguments
/// * `env` — host environment
/// * `numerator` — see signature
/// * `denominator` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn set_risk_multiplier(
        env: Env,
        numerator: i128,
        denominator: i128,
    ) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        if numerator < 0 || denominator <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::RiskMultiplierNumerator, &numerator);
        env.storage()
            .instance()
            .set(&DataKey::RiskMultiplierDenominator, &denominator);
        Ok(())
    }

    /// Set the reputation_bonus contract address for cross-contract
    /// reputation reads (ADR-015). Requires admin auth.
    ///
    /// Once set, `calculate_premium_rate_bps` will read the LP's reputation
    /// score and reduce their effective default count proportionally.
    ///
    /// Pass `None` or an empty address to disable reputation integration.
    pub fn set_reputation_contract(
        env: Env,
        contract: Address,
    ) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&DataKey::ReputationContract, &contract);
        Ok(())
    }

    /// Get the configured reputation_bonus contract address, if any.
    pub fn get_reputation_contract(env: Env) -> Option<Address> {
        env.storage()
            .instance()
            .get(&DataKey::ReputationContract)
    }

/// Propose a new risk multiplier. Requires current admin auth. Overwrites
/// any previously pending risk multiplier proposal.
///
/// # Arguments
/// * `env` — host environment
/// * `numerator` — see signature
/// * `denominator` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn propose_risk_multiplier(
        env: Env,
        numerator: i128,
        denominator: i128,
    ) -> Result<u64, InsuranceError> {
        Self::require_admin(&env);
        if numerator < 0 || denominator <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }

        let eta = env
            .ledger()
            .timestamp()
            .saturating_add(TIMELOCK_DELAY_SECONDS);

        let storage = env.storage().instance();
        storage.set(&DataKey::PendingRiskMultiplierNumerator, &numerator);
        storage.set(&DataKey::PendingRiskMultiplierDenominator, &denominator);
        storage.set(&DataKey::RiskMultiplierEta, &eta);

        env.events()
            .publish((symbol_short!("rm_prop"),), (numerator, denominator, eta));
        Ok(eta)
    }

/// Execute a previously proposed risk multiplier change once its timelock
///
/// Access: Anyone
pub fn execute_risk_multiplier(env: Env) -> Result<(), InsuranceError> {
        let storage = env.storage().instance();
        let numerator: i128 = storage
            .get(&DataKey::PendingRiskMultiplierNumerator)
            .ok_or(InsuranceError::NoPendingProposal)?;
        let denominator: i128 = storage
            .get(&DataKey::PendingRiskMultiplierDenominator)
            .ok_or(InsuranceError::NoPendingProposal)?;
        let eta: u64 = storage
            .get(&DataKey::RiskMultiplierEta)
            .ok_or(InsuranceError::NoPendingProposal)?;

        if env.ledger().timestamp() < eta {
            return Err(InsuranceError::TimelockNotExpired);
        }

        let old_num = Self::get_risk_multiplier_numerator(env.clone());
        let old_den = Self::get_risk_multiplier_denominator(env.clone());

        storage.set(&DataKey::RiskMultiplierNumerator, &numerator);
        storage.set(&DataKey::RiskMultiplierDenominator, &denominator);
        storage.remove(&DataKey::PendingRiskMultiplierNumerator);
        storage.remove(&DataKey::PendingRiskMultiplierDenominator);
        storage.remove(&DataKey::RiskMultiplierEta);

        env.events().publish(
            (symbol_short!("rm_exec"),),
            (old_num, old_den, numerator, denominator),
        );
        Ok(())
    }

/// Cancel a pending risk multiplier change proposal. Requires current admin auth.
///
/// # Arguments
/// * `env` — host environment
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn cancel_risk_multiplier(env: Env) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        let storage = env.storage().instance();
        if !storage.has(&DataKey::PendingRiskMultiplierNumerator) {
            return Err(InsuranceError::NoPendingProposal);
        }
        storage.remove(&DataKey::PendingRiskMultiplierNumerator);
        storage.remove(&DataKey::PendingRiskMultiplierDenominator);
        storage.remove(&DataKey::RiskMultiplierEta);
        env.events().publish((symbol_short!("rm_cncl"),), ());
        Ok(())
    }

/// Get the LP's historical default count.
///
/// Access: Anyone
pub fn get_default_count(env: Env, lp: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::DefaultCount(lp))
            .unwrap_or(0)
    }

/// Increment the LP's default count. Admin-only.
///
/// # Arguments
/// * `env` — host environment
/// * `lp` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn increment_default_count(env: Env, lp: Address) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        let count: u32 = Self::get_default_count(env.clone(), lp.clone());
        env.storage()
            .persistent()
            .set(&DataKey::DefaultCount(lp), &(count + 1));

        let total: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDefaultCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalDefaultCount, &(total + 1));
        Ok(())
    }

/// Get the LP's historical claim count.
///
/// Access: Anyone
pub fn get_claim_count(env: Env, lp: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::ClaimCount(lp))
            .unwrap_or(0)
    }

/// Calculate the risk-priced premium for an LP based on their history.
/// Returns the premium rate in basis points.
///
/// Formula: base_rate + (effective_default_count * risk_multiplier)
///
/// The effective default count is reduced by the LP's reputation score
/// (ADR-015): higher reputation means fewer effective defaults.
///   effective_default_count = default_count × (1 - score/100)
///
/// Example: base=500 (5%), multiplier=0.5x, score=80%
///   - 0 defaults: 500 bps (5%)
///   - 1 default, score 80: effective=0.2 → 510 bps (5.1%)
///   - 2 defaults, score 50: effective=1.0 → 550 bps (5.5%)
pub fn calculate_premium_rate_bps(env: Env, lp: Address) -> u32 {
        let base_rate = Self::get_base_premium_rate_bps(env.clone());
        let default_count = Self::get_default_count(env.clone(), lp.clone()) as i128;
        let numerator = Self::get_risk_multiplier_numerator(env.clone());
        let denominator = Self::get_risk_multiplier_denominator(env.clone());

        if denominator == 0 {
            return base_rate;
        }

        // ADR-015: apply reputation discount to effective default count
        let effective_default_count =
            Self::apply_reputation_discount(env.clone(), lp, default_count);

        let risk_adjustment = effective_default_count
            .checked_mul(numerator)
            .and_then(|v| v.checked_mul(10_000))
            .and_then(|v| v.checked_div(denominator))
            .unwrap_or(10_000); // fallback to max bps on overflow

        let total_rate = (base_rate as i128).saturating_add(risk_adjustment);

        // Cap at 100% (10_000 bps)
        if total_rate > 10_000 {
            10_000
        } else {
            total_rate as u32
        }
    }

    /// Apply reputation discount to the default count (ADR-015).
    ///
    /// Reads the LP's reputation score from the configured reputation_bonus
    /// contract and reduces the effective default count proportionally.
    /// Higher scores produce larger discounts. The floor is 0 — reputation
    /// never increases the effective count.
    ///
    /// Falls back to the raw default_count if the reputation contract is
    /// not configured or the cross-contract call fails.
    fn apply_reputation_discount(env: Env, lp: Address, default_count: i128) -> i128 {
        let Some(rep_contract) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::ReputationContract)
        else {
            // No reputation contract configured — no discount
            return default_count;
        };

        // Cross-contract call: reputation_bonus.get_reputation(lp) → ReputationScore
        // We only need the `score` field (0–100).
        let args = soroban_sdk::vec![&env, lp.into_val(&env)];
        let result = env.try_invoke_contract::<Val, soroban_sdk::Error>(
            &rep_contract,
            &Symbol::new(&env, "get_reputation"),
            args,
        );

        match result {
            Ok(Ok(val)) => {
                // The ReputationScore is a ContractType; extract the score field.
                // try_invoke_contract returns a Val — we need to decode it.
                // For safety, use try-from with a fallback.
                let score: u32 = soroban_sdk::TryFromVal::try_from_val(&env, &val)
                    .unwrap_or(0u32);
                // Apply discount: effective = default_count × (100 - score) / 100
                default_count
                    .checked_mul(100 - score as i128)
                    .and_then(|v| v.checked_div(100))
                    .unwrap_or(default_count)
            }
            _ => {
                // Cross-contract call failed — no discount
                default_count
            }
        }
    }

/// Calculate the premium amount for an LP based on their risk profile.
/// The amount is the invoice amount multiplied by the risk-priced rate.
pub fn calculate_premium_amount(env: Env, lp: Address, invoice_amount: i128) -> i128 {
        let rate_bps = Self::calculate_premium_rate_bps(env, lp);
        invoice_amount
            .saturating_mul(rate_bps as i128)
            .saturating_div(10_000)
    }

/// Get the tiered coverage for an LP based on their total premiums paid.
///
/// Access: Anyone
pub fn get_tiered_coverage(env: Env, lp: Address) -> i128 {
        let premiums_paid = Self::get_premiums_paid(env.clone(), lp);
        let default_coverage = Self::get_coverage(env.clone());

        // Simple tiered system based on premiums paid:
        // Tier 1: < 10% of default coverage -> 50% of default coverage
        // Tier 2: 10-25% of default coverage -> 75% of default coverage
        // Tier 3: 25-50% of default coverage -> 100% of default coverage
        // Tier 4: > 50% of default coverage -> 150% of default coverage
        let threshold_10 = default_coverage.saturating_div(10);
        let threshold_25 = default_coverage.saturating_div(4);
        let threshold_50 = default_coverage.saturating_div(2);

        if premiums_paid >= threshold_50 {
            default_coverage.saturating_mul(150).saturating_div(100) // 150% coverage
        } else if premiums_paid >= threshold_25 {
            default_coverage // 100% coverage
        } else if premiums_paid >= threshold_10 {
            default_coverage.saturating_mul(75).saturating_div(100) // 75% coverage
        } else {
            default_coverage.saturating_mul(50).saturating_div(100) // 50% coverage
        }
    }

    /// Returns `true` if a claim has already been processed for `invoice_id`.
    pub fn is_claimed(env: Env, invoice_id: u64) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Claimed(invoice_id))
            .unwrap_or(false)
    }

    // ── Issue #542: timelocked admin actions ────────────────────────────
    //
    // Coverage cap changes and admin transfers are sensitive, LP-affecting
    // parameters. Rather than applying immediately, they are queued behind a
    // `TIMELOCK_DELAY_SECONDS` delay so LPs have advance notice and a chance
    // to exit before the change takes effect. The current admin may cancel a
    // pending proposal at any time before it executes.

/// Propose a new coverage cap. Requires current admin auth. Overwrites
///
/// # Arguments
/// * `env` — host environment
/// * `new_coverage` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn propose_coverage_change(env: Env, new_coverage: i128) -> Result<u64, InsuranceError> {
        Self::require_admin(&env);
        if new_coverage <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }

        let eta = env
            .ledger()
            .timestamp()
            .saturating_add(TIMELOCK_DELAY_SECONDS);

        let storage = env.storage().instance();
        storage.set(&DataKey::PendingCoverage, &new_coverage);
        storage.set(&DataKey::CoverageEta, &eta);

        env.events()
            .publish((symbol_short!("cov_prop"),), (new_coverage, eta));
        Ok(eta)
    }

/// Execute a previously proposed coverage change once its timelock has
///
/// Access: Anyone
pub fn execute_coverage_change(env: Env) -> Result<(), InsuranceError> {
        let storage = env.storage().instance();
        let new_coverage: i128 = storage
            .get(&DataKey::PendingCoverage)
            .ok_or(InsuranceError::NoPendingProposal)?;
        let eta: u64 = storage
            .get(&DataKey::CoverageEta)
            .ok_or(InsuranceError::NoPendingProposal)?;

        if env.ledger().timestamp() < eta {
            return Err(InsuranceError::TimelockNotExpired);
        }

        storage.set(&DataKey::Coverage, &new_coverage);
        storage.remove(&DataKey::PendingCoverage);
        storage.remove(&DataKey::CoverageEta);

        env.events()
            .publish((symbol_short!("cov_exec"),), new_coverage);
        Ok(())
    }

/// Cancel a pending coverage change proposal. Requires current admin auth.
///
/// # Arguments
/// * `env` — host environment
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn cancel_coverage_change(env: Env) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        let storage = env.storage().instance();
        if !storage.has(&DataKey::PendingCoverage) {
            return Err(InsuranceError::NoPendingProposal);
        }
        storage.remove(&DataKey::PendingCoverage);
        storage.remove(&DataKey::CoverageEta);
        env.events().publish((symbol_short!("cov_cncl"),), ());
        Ok(())
    }

/// Propose an admin transfer. Requires current admin auth. Overwrites any
///
/// # Arguments
/// * `env` — host environment
/// * `new_admin` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn propose_admin_transfer(env: Env, new_admin: Address) -> Result<u64, InsuranceError> {
        Self::require_admin(&env);

        let eta = env
            .ledger()
            .timestamp()
            .saturating_add(TIMELOCK_DELAY_SECONDS);

        let storage = env.storage().instance();
        storage.set(&DataKey::PendingAdmin, &new_admin);
        storage.set(&DataKey::AdminEta, &eta);

        env.events()
            .publish((symbol_short!("adm_prop"), new_admin), eta);
        Ok(eta)
    }

/// Execute a previously proposed admin transfer once its timelock has
///
/// Access: Anyone
pub fn execute_admin_transfer(env: Env) -> Result<(), InsuranceError> {
        let storage = env.storage().instance();
        let new_admin: Address = storage
            .get(&DataKey::PendingAdmin)
            .ok_or(InsuranceError::NoPendingProposal)?;
        let eta: u64 = storage
            .get(&DataKey::AdminEta)
            .ok_or(InsuranceError::NoPendingProposal)?;

        if env.ledger().timestamp() < eta {
            return Err(InsuranceError::TimelockNotExpired);
        }

        storage.set(&DataKey::Admin, &new_admin);
        storage.remove(&DataKey::PendingAdmin);
        storage.remove(&DataKey::AdminEta);

        env.events()
            .publish((symbol_short!("adm_exec"),), new_admin);
        Ok(())
    }

/// Cancel a pending admin transfer proposal. Requires current admin auth.
///
/// # Arguments
/// * `env` — host environment
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn cancel_admin_transfer(env: Env) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        let storage = env.storage().instance();
        if !storage.has(&DataKey::PendingAdmin) {
            return Err(InsuranceError::NoPendingProposal);
        }
        storage.remove(&DataKey::PendingAdmin);
        storage.remove(&DataKey::AdminEta);
        env.events().publish((symbol_short!("adm_cncl"),), ());
        Ok(())
    }

/// Returns the pending coverage proposal (new cap, eta), if any.
///
/// Access: Anyone
pub fn get_pending_coverage(env: Env) -> Option<(i128, u64)> {
        let storage = env.storage().instance();
        let new_coverage: i128 = storage.get(&DataKey::PendingCoverage)?;
        let eta: u64 = storage.get(&DataKey::CoverageEta)?;
        Some((new_coverage, eta))
    }

/// Returns the pending admin transfer proposal (new admin, eta), if any.
///
/// Access: Anyone
pub fn get_pending_admin(env: Env) -> Option<(Address, u64)> {
        let storage = env.storage().instance();
        let new_admin: Address = storage.get(&DataKey::PendingAdmin)?;
        let eta: u64 = storage.get(&DataKey::AdminEta)?;
        Some((new_admin, eta))
    }

/// Set coverage cap directly via governance (no timelock, single call).
///
/// # Arguments
/// * `env` — host environment
/// * `new_coverage` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn set_coverage_via_governance(env: Env, new_coverage: i128) -> Result<(), InsuranceError> {
        if new_coverage <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(InsuranceError::NotInitialized)?;
        admin.require_auth();

        let old_coverage: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Coverage)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::Coverage, &new_coverage);

        env.events()
            .publish((symbol_short!("cov_gov"),), (old_coverage, new_coverage));
        Ok(())
    }

/// Set premium rate directly via governance.
///
/// # Arguments
/// * `env` — host environment
/// * `rate_bps` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn set_premium_rate_via_governance(
        env: Env,
        rate_bps: u32,
    ) -> Result<(), InsuranceError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(InsuranceError::NotInitialized)?;
        admin.require_auth();

        if rate_bps == 0 || rate_bps > 10_000 {
            return Err(InsuranceError::InvalidAmount);
        }

        let old_rate: u32 = Self::get_base_premium_rate_bps(env.clone());
        env.storage()
            .instance()
            .set(&DataKey::BasePremiumRateBps, &rate_bps);

        env.events()
            .publish((symbol_short!("prem_gov"),), (old_rate as i128, rate_bps as i128));
        Ok(())
    }

/// Get the current pool balance cap, or `None` if uncapped.
///
/// Access: Anyone
pub fn get_balance_cap(env: Env) -> Option<i128> {
        env.storage().instance().get(&DataKey::BalanceCap)
    }

/// Set (or clear) the pool balance cap. Pass `0` to remove the cap.
///
/// # Arguments
/// * `env` — host environment
/// * `cap` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn set_balance_cap(env: Env, cap: i128) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        if cap < 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        if cap == 0 {
            env.storage().instance().remove(&DataKey::BalanceCap);
        } else {
            env.storage().instance().set(&DataKey::BalanceCap, &cap);
        }
        env.events().publish((symbol_short!("cap_set"),), cap);
        Ok(())
    }

/// Get a point-in-time solvency snapshot: balance, enrolled exposure,
///
/// Access: Anyone
pub fn get_pool_health(env: Env) -> PoolHealth {
        let balance: i128 = env.storage().instance().get(&DataKey::Balance).unwrap_or(0);
        let enrolled_lp_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::EnrolledCount)
            .unwrap_or(0);
        let total_defaults: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDefaultCount)
            .unwrap_or(0);

        // No claim history yet: rate is 0 and there's nothing to divide by.
        let estimated_monthly_claim_rate = if total_defaults == 0 {
            0i128
        } else {
            let initialized_at: u64 = env
                .storage()
                .instance()
                .get(&DataKey::InitializedAt)
                .unwrap_or_else(|| env.ledger().timestamp());
            let elapsed_seconds = env.ledger().timestamp().saturating_sub(initialized_at);
            // Floor the divisor at one month so a young pool (or defaults
            // recorded in the same ledger as initialization) can't inflate
            // the estimate by dividing by a near-zero time window.
            let elapsed_months = (elapsed_seconds / SECONDS_PER_MONTH).max(1) as i128;
            let coverage = Self::get_coverage(env.clone());
            (total_defaults as i128).saturating_mul(coverage) / elapsed_months
        };

        let months_of_coverage = if estimated_monthly_claim_rate <= 0 {
            None
        } else {
            let months = balance / estimated_monthly_claim_rate;
            Some(months.clamp(0, u32::MAX as i128) as u32)
        };

        PoolHealth {
            balance,
            enrolled_lp_count,
            estimated_monthly_claim_rate,
            months_of_coverage,
        }
    }

    // ── Issue #826: solvency circuit breaker ─────────────────────────────
    //
    // Governance can set a minimum pool reserve ratio (balance vs the
    // per-claim coverage cap). When the pool's actual ratio falls below the
    // threshold, new claim payouts are paused and the breaker trips (sticky
    // until a governance reset). Enrollments and premium deposits continue
    // regardless, so the pool can recover without forcing LPs out.

/// Set the minimum reserve ratio (in bps, 0..=10_000) below which new
///
/// # Arguments
/// * `env` — host environment
/// * `bps` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn set_min_reserve_ratio_bps(env: Env, bps: u32) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        if bps > 10_000 {
            return Err(InsuranceError::InvalidReserveRatio);
        }
        env.storage()
            .instance()
            .set(&DataKey::MinReserveRatioBps, &bps);
        env.events().publish((symbol_short!("resv_min"),), bps);
        Ok(())
    }

/// The configured minimum reserve ratio (bps). `0` means the breaker is
///
/// Access: Anyone
pub fn get_min_reserve_ratio_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MinReserveRatioBps)
            .unwrap_or(0)
    }

/// Current pool reserve ratio in bps: total claimable reserve (liquid
///
/// Access: Anyone
pub fn get_reserve_ratio_bps(env: Env) -> u32 {
        let coverage = Self::get_coverage(env.clone());
        if coverage <= 0 {
            return 0;
        }
        let reserve = Self::get_total_reserve(env);
        let ratio = reserve.saturating_mul(10_000).saturating_div(coverage);
        ratio.min(u32::MAX as i128) as u32
    }

/// Total claimable reserve: liquid claim balance plus capital backstop.
///
/// Access: Anyone
pub fn get_total_reserve(env: Env) -> i128 {
        let balance: i128 = env.storage().instance().get(&DataKey::Balance).unwrap_or(0);
        let backstop: i128 = env
            .storage()
            .instance()
            .get(&DataKey::BackstopBalance)
            .unwrap_or(0);
        balance.saturating_add(backstop)
    }

/// Whether the solvency circuit breaker is currently open (payouts
///
/// Access: Anyone
pub fn is_solvency_circuit_open(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::SolvencyCircuitOpenFlag)
            .unwrap_or(false)
    }

/// Resume claim payouts after a solvency circuit trip. Admin-only.
///
/// # Arguments
/// * `env` — host environment
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn reset_solvency_circuit(env: Env) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        if !env
            .storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::SolvencyCircuitOpenFlag)
            .unwrap_or(false)
        {
            return Ok(());
        }
        let ratio_bps = Self::get_reserve_ratio_bps(env.clone());
        let reserve = Self::get_total_reserve(env.clone());
        env.storage()
            .instance()
            .remove(&DataKey::SolvencyCircuitOpenFlag);
        env.events().publish(
            (symbol_short!("solv_rst"),),
            SolvencyCircuitReset { ratio_bps, reserve },
        );
        Ok(())
    }

    /// Reject a claim while the sticky solvency breaker is open. Called at
    /// the top of `claim()` *before* any storage write so the rejection
    /// never has state to roll back. No-op while the breaker is clear.
    fn gate_solvency_circuit(env: &Env) {
        let open: bool = env
            .storage()
            .instance()
            .get(&DataKey::SolvencyCircuitOpenFlag)
            .unwrap_or(false);
        if open {
            panic_with_error!(env, InsuranceError::SolvencyCircuitOpen);
        }
    }

    /// After an otherwise-successful payout, check whether the pool's
    /// reserve ratio has dropped to/below the governance-configured minimum
    /// and, if so, trip the sticky breaker. Emits `SolvencyCircuitTripped`
    /// exactly once per trip. This is deliberately the *last* step of a
    /// `claim()` that is about to return `Ok` — in Soroban, a storage write
    /// followed by a panic within the same invocation is rolled back (no
    /// partial commit), so the trip flag could never persist if it were set
    /// on the rejection path itself. A tripping `claim()` therefore pays its
    /// final boundary payout and then pauses *subsequent* claims until
    /// governance resets the breaker.
    fn trip_circuit_if_breached(env: &Env) {
        let min_bps = Self::get_min_reserve_ratio_bps(env.clone());
        if min_bps == 0 {
            return;
        }
        let already_open: bool = env
            .storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::SolvencyCircuitOpenFlag)
            .unwrap_or(false);
        if already_open {
            return;
        }
        let ratio_bps = Self::get_reserve_ratio_bps(env.clone());
        if ratio_bps >= min_bps {
            return;
        }
        let reserve = Self::get_total_reserve(env.clone());
        env.storage()
            .instance()
            .set(&DataKey::SolvencyCircuitOpenFlag, &true);
        env.events().publish(
            (symbol_short!("solv_trip"),),
            SolvencyCircuitTripped { ratio_bps, reserve },
        );
    }

    // ── Issue #827: protocol capital backstop ────────────────────────────
    //
    // A separate accounting balance held as a capital backstop, funded by a
    // configurable share of each premium deposit and/or governance-led
    // top-ups. Claims draw from liquid balance first, then the backstop.

/// Set the share (bps) of each premium deposit diverted to the backstop
///
/// # Arguments
/// * `env` — host environment
/// * `bps` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn set_backstop_funding_bps(env: Env, bps: u32) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        if bps > 10_000 {
            return Err(InsuranceError::InvalidReserveRatio);
        }
        env.storage()
            .instance()
            .set(&DataKey::BackstopFundingBps, &bps);
        env.events().publish((symbol_short!("back_bps"),), bps);
        Ok(())
    }

/// The configured premium-to-backstop share (bps). `0` = disabled.
///
/// Access: Anyone
pub fn get_backstop_funding_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::BackstopFundingBps)
            .unwrap_or(0)
    }

/// The current capital backstop balance.
///
/// Access: Anyone
pub fn get_backstop_balance(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::BackstopBalance)
            .unwrap_or(0)
    }

    /// Top up the capital backstop. `from` transfers `amount` tokens to the
    /// pool; the credited amount is booked to the backstop, not the liquid
    /// claim balance. Admin authorizes the ordering; `from` authorizes the
    /// transfer. Emits `BackstopTopUp`.
    pub fn top_up_backstop(env: Env, from: Address, amount: i128) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        if amount <= 0 {
            return Err(InsuranceError::InvalidBackstopAmount);
        }
        from.require_auth();

        let backstop: i128 = Self::get_backstop_balance(env.clone());
        let new_backstop = backstop
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(&env, InsuranceError::ArithmeticOverflow));
        env.storage()
            .instance()
            .set(&DataKey::BackstopBalance, &new_backstop);

        let token = Self::get_token_client(&env)?;
        token.transfer(
            &from,                           // from (caller)
            &env.current_contract_address(), // to (this contract)
            &amount,
        );

        env.events().publish(
            (symbol_short!("back_top"), from.clone()),
            BackstopTopUp {
                from,
                amount,
                backstop_balance: new_backstop,
            },
        );
        Ok(())
    }

    // ── Issue #828: claim evidence & review window ───────────────────────
    //
    // A lightweight on-chain evidence-hash can be attached to any claim at
    // any time (advisory: auditable after the fact). When governance enables
    // a review window, payout is additionally gated on evidence having been
    // submitted and the window having elapsed — opt-in per risk tier, so the
    // automatic flow is unchanged while the gate is off.

/// Attach an evidence hash (32 bytes, e.g. an IPFS CID / doc digest) to
///
/// # Arguments
/// * `env` — host environment
/// * `invoice_id` — see signature
/// * `evidence_hash` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn submit_claim_evidence(
        env: Env,
        invoice_id: u64,
        evidence_hash: BytesN<32>,
    ) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        let evidence = ClaimEvidence {
            evidence_hash: evidence_hash.clone(),
            submitted_at: env.ledger().timestamp(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::ClaimEvidence(invoice_id), &evidence.clone());
        env.events()
            .publish((symbol_short!("evidence"), invoice_id), evidence);
        Ok(())
    }

    /// The evidence hash and submission timestamp recorded for an invoice,
    /// if any.
    pub fn get_claim_evidence(env: Env, invoice_id: u64) -> Option<ClaimEvidence> {
        env.storage()
            .persistent()
            .get(&DataKey::ClaimEvidence(invoice_id))
    }

/// Set the review window (seconds) that gated claims must sit in after
///
/// # Arguments
/// * `env` — host environment
/// * `seconds` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn set_review_window_seconds(env: Env, seconds: u64) -> Result<(), InsuranceError> {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&DataKey::ReviewWindowSeconds, &seconds);
        env.events().publish((symbol_short!("rev_wnd"),), seconds);
        Ok(())
    }

/// The configured review window (seconds). `0` = gate disabled.
///
/// Access: Anyone
pub fn get_review_window_seconds(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ReviewWindowSeconds)
            .unwrap_or(0)
    }

    /// Evaluate the review-window gate on a claim. No-op while the window is
    /// disabled. When enabled, requires evidence to have been submitted and
    /// the window to have elapsed since submission.
    fn enforce_claim_review_gate(env: &Env, invoice_id: u64) {
        let window_seconds = Self::get_review_window_seconds(env.clone());
        if window_seconds == 0 {
            return;
        }
        let evidence: ClaimEvidence = match env
            .storage()
            .persistent()
            .get(&DataKey::ClaimEvidence(invoice_id))
        {
            Some(e) => e,
            None => panic_with_error!(env, InsuranceError::EvidenceRequired),
        };
        let now = env.ledger().timestamp();
        let payout_eta = evidence
            .submitted_at
            .checked_add(window_seconds)
            .unwrap_or(u64::MAX);
        if now < payout_eta {
            panic_with_error!(env, InsuranceError::ReviewWindowNotElapsed);
        }
    }

    // ── Issue #829: per-pair default tracking (collusion heuristic) ──────
    //
    // Defaults are tracked per (lp, payer) pair so an off-chain monitor can
    // detect concentration: a payer responsible for a disproportionately
    // large share of one LP's defaults is a collusion signal. On-chain data
    // is intentionally kept raw and simple — computed/viewed off-chain.

/// Record a confirmed default for an (lp, payer) pair. Also bumps the
///
/// # Arguments
/// * `env` — host environment
/// * `lp` — see signature
/// * `payer` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * `Unauthorized` if caller is not admin; plus validation errors
///
/// Access: Admin only
pub fn record_pair_default(
        env: Env,
        lp: Address,
        payer: Address,
    ) -> Result<(), InsuranceError> {
        Self::require_admin(&env);

        Self::increment_default_count(env.clone(), lp.clone())?;

        let pair_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::PairDefaultCount(lp.clone(), payer.clone()))
            .unwrap_or(0);
        let new_pair_count = pair_count.saturating_add(1);
        env.storage().persistent().set(
            &DataKey::PairDefaultCount(lp.clone(), payer.clone()),
            &new_pair_count,
        );

        env.events().publish(
            (symbol_short!("pair_def"), lp.clone(), payer.clone()),
            PairDefaultRecorded {
                lp,
                payer,
                pair_count: new_pair_count,
            },
        );
        Ok(())
    }

/// Total confirmed defaults for a specific (lp, payer) pair.
///
/// Access: Anyone
pub fn get_pair_default_count(env: Env, lp: Address, payer: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::PairDefaultCount(lp, payer))
            .unwrap_or(0)
    }

/// Collusion heuristic for a (lp, payer) pair, computed on-chain for
///
/// Access: Anyone
pub fn get_pair_collusion_flag(env: Env, lp: Address, payer: Address) -> bool {
        let pair_count = Self::get_pair_default_count(env.clone(), lp.clone(), payer);
        if pair_count < MIN_PAIR_DEFAULTS_TO_FLAG {
            return false;
        }
        let lp_defaults = Self::get_default_count(env, lp);
        if lp_defaults == 0 {
            return false;
        }
        let pair_share_bps = (pair_count as u64).saturating_mul(10_000) / (lp_defaults as u64);
        pair_share_bps >= COLLUSION_PAIR_SHARE_FLAG_BPS as u64
    }

    /// Mark `lp` as enrolled, maintaining `EnrolledCount` — a no-op if
    /// already enrolled, so repeated `enroll()`/`deposit_premium()` calls
    /// don't inflate the count.
    fn mark_enrolled(env: &Env, lp: &Address) {
        let already_enrolled: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Enrolled(lp.clone()))
            .unwrap_or(false);
        if already_enrolled {
            return;
        }
        env.storage()
            .persistent()
            .set(&DataKey::Enrolled(lp.clone()), &true);
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::EnrolledCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::EnrolledCount, &(count + 1));
    }

    fn require_admin(env: &Env) -> Address {
        match env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
        {
            Some(admin) => {
                admin.require_auth();
                admin
            }
            None => panic_with_error!(env, InsuranceError::NotInitialized),
        }
    }

    fn get_token_client(env: &Env) -> Result<token::Client, InsuranceError> {
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::TokenAddress)
            .ok_or(InsuranceError::NotInitialized)?;
        Ok(token::Client::new(env, &token_addr))
    }
}

#[contractimpl]
impl InsurancePoolInterface for InsurancePool {
    fn interface_version(_env: Env) -> u32 {
        crate::insurance_interface::INSURANCE_INTERFACE_VERSION
    }

    fn enroll(env: Env, lp: Address) {
        lp.require_auth();
        Self::mark_enrolled(&env, &lp);
        env.events().publish((symbol_short!("enrolled"), lp), ());
    }

    fn is_enrolled(env: Env, lp: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Enrolled(lp))
            .unwrap_or(false)
    }

    fn deposit_premium(env: Env, lp: Address, amount: i128) {
        lp.require_auth();
        if amount <= 0 {
            panic_with_error!(&env, InsuranceError::InvalidAmount);
        }

        // Auto-enroll on first premium so a paying LP is always covered.
        Self::mark_enrolled(&env, &lp);

        let prev_premium: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Premiums(lp.clone()))
            .unwrap_or(0);
        let new_premium = prev_premium
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(&env, InsuranceError::ArithmeticOverflow));
        env.storage()
            .persistent()
            .set(&DataKey::Premiums(lp.clone()), &new_premium);

        // Issue #827: divert a governance-configured share of the premium to
        // the capital backstop; the rest is booked to the liquid balance.
        let funding_bps: u32 = Self::get_backstop_funding_bps(env.clone());
        let backstop_share = (amount as i128)
            .saturating_mul(funding_bps as i128)
            .saturating_div(10_000);
        let liquid_share = amount.saturating_sub(backstop_share);

        let balance: i128 = env.storage().instance().get(&DataKey::Balance).unwrap_or(0);
        let new_balance = balance
            .checked_add(liquid_share)
            .unwrap_or_else(|| panic_with_error!(&env, InsuranceError::ArithmeticOverflow));

        // Enforce the optional balance cap.
        if let Some(cap) = env
            .storage()
            .instance()
            .get::<DataKey, i128>(&DataKey::BalanceCap)
        {
            if new_balance > cap {
                panic_with_error!(&env, InsuranceError::BalanceCapExceeded);
            }
        }

        env.storage()
            .instance()
            .set(&DataKey::Balance, &new_balance);

        if backstop_share > 0 {
            let backstop: i128 = Self::get_backstop_balance(env.clone());
            let new_backstop = backstop
                .checked_add(backstop_share)
                .unwrap_or_else(|| panic_with_error!(&env, InsuranceError::ArithmeticOverflow));
            env.storage()
                .instance()
                .set(&DataKey::BackstopBalance, &new_backstop);
            env.events().publish(
                (symbol_short!("back_top"), lp.clone()),
                BackstopTopUp {
                    from: lp.clone(),
                    amount: backstop_share,
                    backstop_balance: new_backstop,
                },
            );
        }

        // Transfer tokens from LP to pool (checks-effects-interactions pattern).
        // State changes above must complete before this external call.
        let token = Self::get_token_client(&env).unwrap_or_else(|e| panic_with_error!(&env, e));
        token.transfer(
            &lp,                             // from (caller)
            &env.current_contract_address(), // to (this contract)
            &amount,
        );

        env.events().publish((symbol_short!("premium"), lp), amount);
    }

    fn claim(env: Env, invoice_id: u64, lp: Address) -> i128 {
        // Only the configured admin (the liquidity contract in production) may
        // report a confirmed default and trigger compensation.
        Self::require_admin(&env);

        if Self::is_claimed(env.clone(), invoice_id) {
            panic_with_error!(&env, InsuranceError::AlreadyClaimed);
        }

        // Issue #826: pause new payouts while the solvency circuit breaker is
        // open. Run before any storage write so the rejection has no state to
        // roll back.
        Self::gate_solvency_circuit(&env);

        // Issue #828: opt-in review window — require evidence + elapsed window
        // before payout when the gate is enabled.
        Self::enforce_claim_review_gate(&env, invoice_id);

        // Issue #827: available reserve is the liquid balance plus the capital
        // backstop; claims draw from liquid first, then the backstop.
        let balance: i128 = Self::get_pool_balance(env.clone());
        let backstop: i128 = Self::get_backstop_balance(env.clone());
        let available = balance.saturating_add(backstop);
        if available > 0 {
            // Use tiered coverage based on LP's premiums paid (Issue #528).
            let coverage: i128 = Self::get_tiered_coverage(env.clone(), lp.clone());
            // Payout: tiered coverage cap, bounded by available reserve.
            let payout = if coverage < available {
                coverage
            } else {
                available
            };

            // Draw from liquid balance first, then the backstop.
            let from_balance = if payout < balance { payout } else { balance };
            let from_backstop = payout.saturating_sub(from_balance);

            // Checks-effects-interactions: update state before external call.
            env.storage()
                .instance()
                .set(&DataKey::Balance, &(balance - from_balance));
            if from_backstop > 0 {
                env.storage()
                    .instance()
                    .set(&DataKey::BackstopBalance, &(backstop - from_backstop));
            }
            env.storage()
                .persistent()
                .set(&DataKey::Claimed(invoice_id), &true);

            // Transfer tokens from pool to LP (Issue #527).
            let token = Self::get_token_client(&env).unwrap_or_else(|e| panic_with_error!(&env, e));
            token.transfer(
                &env.current_contract_address(), // from (this contract)
                &lp,                             // to
                &payout,
            );

            env.events()
                .publish((symbol_short!("claimed"), invoice_id), payout);

            // Issue #826: trip the sticky breaker if this payout dropped the
            // reserve ratio to/below the minimum. Must run last, before this
            // invocation returns Ok, so the flag write persists (a write
            // followed by a panic would be rolled back entirely).
            Self::trip_circuit_if_breached(&env);
            payout
        } else {
            panic_with_error!(&env, InsuranceError::PoolEmpty);
        }
    }

    fn get_pool_balance(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::Balance).unwrap_or(0)
    }
}
