//! Mock oracle contract implementing the payer-verification oracle interface.
//!
//! Issue #94 — provides controllable, deterministic oracle behaviour for
//! test environments.  All operations work on in-contract persistent storage
//! so state is consistent across cross-contract calls within the same test
//! environment.
//!
//! Test primitives beyond the standard oracle interface:
//! - `set_feeder(address)` — configure the authorized feeder (oracle operator)
//!   address. Production oracles set this once at deployment; the mock exposes
//!   it so tests can drive the feeder gate (Issue #1046).
//! - `get_feeder()` — read the currently configured feeder, if any.
//! - `set_verified(address, bool)` — set per-address verification status
//!   without any authorization check.
//! - `set_timestamp(ts: u64)` — override the global timestamp returned for
//!   all queries, allowing staleness scenarios to be simulated.
//! - `set_should_panic()` — arm a one-shot flag that causes the very next
//!   `get_verification()` call to panic, simulating an oracle contract failure.
//!   The flag is cleared after it fires so subsequent calls succeed.
//!
//! Issue #1046 — `update_verification` is the oracle's feeder (operator) write
//! path and now enforces the interface's authorization requirement: it requires
//! the configured feeder's signature and rejects the call when no feeder has
//! been configured. The `set_*` helpers above remain unrestricted because they
//! are test-only setup primitives, not part of the `OracleInterface` surface.

#![allow(dead_code)]

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

// ── Return type ───────────────────────────────────────────────────────────────

/// Verification record returned by get_verification.
/// Mirrors oracle_interface::VerificationResult — redefined here so the mock
/// is self-contained and does not depend on the invoice_liquidity crate.
#[contracttype]
#[derive(Clone, Debug)]
pub struct VerificationResult {
    pub verified: bool,
    pub timestamp: u64,
}

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
enum MockOracleKey {
    /// Per-address verification flag.
    Verified(Address),
    /// Authorized feeder (oracle operator) address — Issue #1046.
    Feeder,
    /// Global timestamp returned for all queries.
    Timestamp,
    /// One-shot panic flag.
    ShouldPanic,
}

// ── Contract struct ───────────────────────────────────────────────────────────

#[contract]
pub struct MockOracle;

// ── Implementation ────────────────────────────────────────────────────────────

#[contractimpl]
impl MockOracle {
    // ── Oracle Interface ──────────────────────────────────────────────────────

    /// Query the verification status for `payer`.
    ///
    /// Matches `OracleInterface::get_verification`.
    /// Panics if `set_should_panic()` was armed, clearing the flag first.
    pub fn interface_version(_env: Env) -> u32 {
        1
    }

    pub fn get_verification(env: Env, payer: Address) -> VerificationResult {
        if env
            .storage()
            .temporary()
            .get::<_, bool>(&MockOracleKey::ShouldPanic)
            .unwrap_or(false)
        {
            env.storage()
                .temporary()
                .remove(&MockOracleKey::ShouldPanic);
            panic!("mock oracle: forced panic (set_should_panic was armed)");
        }

        let verified: bool = env
            .storage()
            .persistent()
            .get(&MockOracleKey::Verified(payer))
            .unwrap_or(false);

        let timestamp: u64 = env
            .storage()
            .persistent()
            .get(&MockOracleKey::Timestamp)
            .unwrap_or(0u64);

        VerificationResult {
            verified,
            timestamp,
        }
    }

    /// Update verification status for `payer`.
    ///
    /// Matches `OracleInterface::update_verification` — and, unlike the pre-#1046
    /// mock, enforces the interface's authorization requirement: only the
    /// configured feeder's signature is accepted.
    ///
    /// Panics with `"mock oracle: feeder not configured"` when `set_feeder` was
    /// never called; when a feeder is configured but did not sign, the host
    /// aborts the invocation with an auth error.
    pub fn update_verification(env: Env, payer: Address, verified: bool) {
        let feeder: Address = match env
            .storage()
            .persistent()
            .get(&MockOracleKey::Feeder)
        {
            Some(f) => f,
            None => panic!("mock oracle: feeder not configured"),
        };
        feeder.require_auth();
        env.storage()
            .persistent()
            .set(&MockOracleKey::Verified(payer), &verified);
    }

    // ── Mock-specific helpers ─────────────────────────────────────────────────

    /// Configure the authorized feeder (oracle operator) address — Issue #1046.
    ///
    /// Mock setup helper with no authorization check, exactly like
    /// `set_verified`. Production oracles set this once in their constructor /
    /// admin path; the mock exposes it so tests can drive the feeder gate.
    pub fn set_feeder(env: Env, feeder: Address) {
        env.storage()
            .persistent()
            .set(&MockOracleKey::Feeder, &feeder);
    }

    /// Read the currently configured feeder address, if any.
    pub fn get_feeder(env: Env) -> Option<Address> {
        env.storage().persistent().get(&MockOracleKey::Feeder)
    }

    /// Set verification status for `address` without any authorization check —
    /// equivalent to a privileged oracle-operator update for test setup.
    pub fn set_verified(env: Env, address: Address, verified: bool) {
        env.storage()
            .persistent()
            .set(&MockOracleKey::Verified(address), &verified);
    }

    /// Set the global timestamp returned by `get_verification` for all queries.
    /// Use this to simulate fresh data (`ts ≈ now`) or stale data (`ts` far in
    /// the past).
    pub fn set_timestamp(env: Env, ts: u64) {
        env.storage()
            .persistent()
            .set(&MockOracleKey::Timestamp, &ts);
    }

    /// Arm a one-shot panic: the very next call to `get_verification` on this
    /// contract will panic with a "forced panic" message, simulating an oracle
    /// contract failure.  The flag is cleared after it fires so subsequent
    /// calls return normally.
    pub fn set_should_panic(env: Env) {
        env.storage()
            .temporary()
            .set(&MockOracleKey::ShouldPanic, &true);
    }
}
