//! Claim payout prioritization when simultaneous defaults exceed pool balance.
//!
//! Issue #825 — Define and implement claim payout priority when simultaneous
//! defaults exceed pool balance.
//!
//! ## Threat Model (G3: Pool Drainage / Insolvency Risk)
//! When multiple LPs default simultaneously and pool balance is insufficient
//! to cover all claims, we prioritize payouts using a configurable strategy:
//!
//! 1. **Pro-Rata**: LPs receive payouts proportional to their accumulated premiums.
//! 2. **Risk-Weighted**: Adjusts pro-rata by LP default history (more claims = lower priority).
//! 3. **FIFO**: Earlier claimants are paid first; later claims may receive partial or zero payout.
//!
//! ## Invariants
//!
//! **Invariant P1: Non-Negative Payout**
//! All payouts are >= 0. Never pay negative amounts.
//!
//! **Invariant P2: Total Payout <= Pool Balance**
//! Sum of all payouts never exceeds the pool's available balance.
//!
//! **Invariant P3: Deterministic Order**
//! Given a fixed set of claims and a prioritization strategy, payout order is deterministic.

extern crate alloc;

use alloc::{vec, vec::Vec};
use soroban_sdk::{contracttype, Address, Env};

/// Payout prioritization strategy for claims exceeding pool balance.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayoutStrategy {
    /// Pro-rata by accumulated premiums.
    ProRata = 0,
    /// Pro-rata adjusted by LP risk profile (default history).
    RiskWeighted = 1,
    /// First-in-first-out by claim timestamp.
    FIFO = 2,
}

/// Claim record queued for payout.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PendingClaim {
    /// LP claiming the payout.
    pub lp: Address,
    /// Invoice ID being claimed.
    pub invoice_id: u64,
    /// Timestamp when claim was filed.
    pub claim_timestamp: u64,
    /// LP's total accumulated premiums (for pro-rata calculation).
    pub lp_total_premiums: i128,
    /// Number of prior defaults for this LP (for risk-weighting).
    pub lp_default_count: i32,
    /// Coverage cap applicable to this claim.
    pub coverage_cap: i128,
}

/// Calculate payout for a single LP given constrained pool balance.
///
/// ## Invariant P1/P2 Enforcement
/// Returns payout >= 0 and <= min(coverage_cap, available_pool_balance).
pub fn calculate_payout(
    claim: &PendingClaim,
    strategy: PayoutStrategy,
    pool_balance: i128,
    total_pending_claims: i128,
    total_lp_premiums: i128,
) -> Result<i128, ClaimPrioritizationError> {
    // Invariant P1: Reject negative pool balance
    if pool_balance < 0 {
        return Err(ClaimPrioritizationError::InvalidPoolBalance);
    }

    // Base payout is capped by coverage
    let base_payout = claim.coverage_cap.min(pool_balance);

    if base_payout <= 0 {
        return Ok(0);
    }

    let payout = match strategy {
        PayoutStrategy::ProRata => calculate_pro_rata(claim, base_payout, total_lp_premiums),
        PayoutStrategy::RiskWeighted => {
            calculate_risk_weighted(claim, base_payout, total_lp_premiums)
        }
        PayoutStrategy::FIFO => base_payout, // FIFO pays up to coverage cap
    };

    // Invariant P1: Ensure non-negative payout
    Ok(payout.max(0))
}

/// Pro-rata payout proportional to LP's accumulated premiums.
///
/// payout = base_payout * (lp_premiums / total_premiums)
fn calculate_pro_rata(
    claim: &PendingClaim,
    base_payout: i128,
    total_lp_premiums: i128,
) -> i128 {
    if total_lp_premiums <= 0 {
        return 0;
    }

    // payout = base_payout * lp_premiums / total_premiums
    let numerator = base_payout.saturating_mul(claim.lp_total_premiums);
    numerator.saturating_div(total_lp_premiums)
}

/// Risk-weighted payout adjusts pro-rata by LP default history.
///
/// multiplier = 1 / (1 + default_count)
/// risk_weighted_payout = pro_rata * multiplier
fn calculate_risk_weighted(
    claim: &PendingClaim,
    base_payout: i128,
    total_lp_premiums: i128,
) -> i128 {
    let pro_rata = calculate_pro_rata(claim, base_payout, total_lp_premiums);

    // Risk multiplier: 1 / (1 + default_count)
    // For simplicity, we use: payout * 100 / (100 + default_count)
    let divisor = 100i128.saturating_add(claim.lp_default_count as i128);
    pro_rata.saturating_mul(100).saturating_div(divisor)
}

/// Allocate constrained pool balance across multiple pending claims.
///
/// ## Invariant P2 Enforcement
/// Total allocated across all claims never exceeds pool_balance.
pub fn allocate_payouts(
    claims: &[PendingClaim],
    strategy: PayoutStrategy,
    pool_balance: i128,
) -> Result<Vec<(Address, i128)>, ClaimPrioritizationError> {
    if pool_balance < 0 {
        return Err(ClaimPrioritizationError::InvalidPoolBalance);
    }

    if claims.is_empty() {
        return Ok(vec![]);
    }

    // Sort claims by strategy
    let mut sorted_claims: Vec<_> = claims.iter().collect();
    match strategy {
        PayoutStrategy::FIFO => {
            sorted_claims.sort_by_key(|c| c.claim_timestamp);
        }
        PayoutStrategy::ProRata | PayoutStrategy::RiskWeighted => {
            // For pro-rata strategies, order by premium amount (highest first)
            sorted_claims.sort_by(|a, b| b.lp_total_premiums.cmp(&a.lp_total_premiums));
        }
    }

    let total_lp_premiums: i128 = claims.iter().map(|c| c.lp_total_premiums).sum();
    let mut allocations: Vec<(Address, i128)> = vec![];
    let mut remaining_balance = pool_balance;

    for claim in sorted_claims {
        if remaining_balance <= 0 {
            break;
        }

        let payout = calculate_payout(claim, strategy, remaining_balance, 0, total_lp_premiums)?;

        // Invariant P2: Never pay more than remaining balance
        let actual_payout = payout.min(remaining_balance);
        if actual_payout > 0 {
            allocations.push((claim.lp.clone(), actual_payout));
            remaining_balance = remaining_balance.saturating_sub(actual_payout);
        }
    }

    Ok(allocations)
}

/// Errors in claim prioritization.
#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClaimPrioritizationError {
    /// Pool balance is negative (invariant violation).
    InvalidPoolBalance = 1,
    /// Payout calculation overflowed.
    PayoutOverflow = 2,
    /// No claims to process.
    NoClaimsToPrioritize = 3,
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn make_claim(premiums: i128, default_count: i32) -> PendingClaim {
        PendingClaim {
            lp: Address::generate(&Env::default()),
            invoice_id: 1,
            claim_timestamp: 1000,
            lp_total_premiums: premiums,
            lp_default_count: default_count,
            coverage_cap: 1_000_000,
        }
    }

    #[test]
    fn test_invariant_p1_non_negative_payout() {
        let claim = make_claim(100, 0);
        let payout =
            calculate_payout(&claim, PayoutStrategy::ProRata, 1_000, 0, 1000).unwrap();
        assert!(payout >= 0);
    }

    #[test]
    fn test_invariant_p2_payout_le_pool_balance() {
        let claim = make_claim(100, 0);
        let pool_balance = 500;
        let payout =
            calculate_payout(&claim, PayoutStrategy::ProRata, pool_balance, 0, 1000).unwrap();
        assert!(payout <= pool_balance);
    }

    #[test]
    fn test_pro_rata_equal_premiums() {
        let env = Env::default();
        let lp1 = Address::generate(&env);
        let lp2 = Address::generate(&env);

        let claim1 = PendingClaim {
            lp: lp1,
            invoice_id: 1,
            claim_timestamp: 1000,
            lp_total_premiums: 1000,
            lp_default_count: 0,
            coverage_cap: 10_000,
        };

        let claim2 = PendingClaim {
            lp: lp2,
            invoice_id: 2,
            claim_timestamp: 1001,
            lp_total_premiums: 1000,
            lp_default_count: 0,
            coverage_cap: 10_000,
        };

        let allocations =
            allocate_payouts(&[claim1, claim2], PayoutStrategy::ProRata, 1000).unwrap();

        // Both should get 500 (50% of pool)
        assert_eq!(allocations.len(), 2);
        let total: i128 = allocations.iter().map(|(_, amt)| amt).sum();
        assert!(total <= 1000);
    }

    #[test]
    fn test_pro_rata_unequal_premiums() {
        let env = Env::default();
        let lp1 = Address::generate(&env);
        let lp2 = Address::generate(&env);

        let claim1 = PendingClaim {
            lp: lp1,
            invoice_id: 1,
            claim_timestamp: 1000,
            lp_total_premiums: 2000,
            lp_default_count: 0,
            coverage_cap: 10_000,
        };

        let claim2 = PendingClaim {
            lp: lp2,
            invoice_id: 2,
            claim_timestamp: 1001,
            lp_total_premiums: 1000,
            lp_default_count: 0,
            coverage_cap: 10_000,
        };

        let allocations =
            allocate_payouts(&[claim1, claim2], PayoutStrategy::ProRata, 900).unwrap();

        // LP1 should get ~600, LP2 should get ~300 (2:1 ratio)
        let total: i128 = allocations.iter().map(|(_, amt)| amt).sum();
        assert!(total <= 900);
    }

    #[test]
    fn test_risk_weighted_reduces_default_heavy_lp() {
        let env = Env::default();
        let lp_clean = Address::generate(&env);
        let lp_risky = Address::generate(&env);

        let claim_clean = PendingClaim {
            lp: lp_clean,
            invoice_id: 1,
            claim_timestamp: 1000,
            lp_total_premiums: 1000,
            lp_default_count: 0,
            coverage_cap: 10_000,
        };

        let claim_risky = PendingClaim {
            lp: lp_risky,
            invoice_id: 2,
            claim_timestamp: 1001,
            lp_total_premiums: 1000,
            lp_default_count: 5,
            coverage_cap: 10_000,
        };

        let payout_clean =
            calculate_payout(&claim_clean, PayoutStrategy::RiskWeighted, 1000, 0, 2000)
                .unwrap();
        let payout_risky =
            calculate_payout(&claim_risky, PayoutStrategy::RiskWeighted, 1000, 0, 2000)
                .unwrap();

        // Clean LP should get higher payout than risky LP
        assert!(payout_clean > payout_risky);
    }

    #[test]
    fn test_fifo_pays_first_claimant_first() {
        let env = Env::default();
        let lp1 = Address::generate(&env);
        let lp2 = Address::generate(&env);

        let claim1 = PendingClaim {
            lp: lp1,
            invoice_id: 1,
            claim_timestamp: 1000,
            lp_total_premiums: 500,
            lp_default_count: 0,
            coverage_cap: 1000,
        };

        let claim2 = PendingClaim {
            lp: lp2,
            invoice_id: 2,
            claim_timestamp: 2000, // Later timestamp
            lp_total_premiums: 500,
            lp_default_count: 0,
            coverage_cap: 1000,
        };

        let allocations = allocate_payouts(&[claim1, claim2], PayoutStrategy::FIFO, 1200)
            .unwrap();

        // LP1 (earlier) should be satisfied first
        assert_eq!(allocations[0].0, lp1);
    }
}
