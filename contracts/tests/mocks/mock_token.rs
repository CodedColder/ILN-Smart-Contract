//! Mock token contract implementing the Soroban SEP-41 Token Interface.
//!
//! Provides deterministic, network-independent token behaviour for test
//! environments.  All token operations operate on in-contract persistent
//! storage so they are consistent across cross-contract calls within the
//! same test environment.
//!
//! Extra testing primitives beyond the standard interface:
//! - `mint(to, amount)` — fund a wallet without any authorization check.
//! - `fail_next_transfer()` — arm a one-shot flag that causes the very next
//!   `transfer()` call to panic, simulating a token transfer failure.
//! - `authorized(id)` / `set_authorized(id, flag)` — SAC-style trustline
//!   authorization (Issues #1072/#1073). Everyone is authorized by default.

#![allow(dead_code)]

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
enum MockTokenKey {
    /// Persistent balance ledger keyed by address.
    Balance(Address),
    /// One-shot flag: next transfer() call will panic.
    FailNext,
    /// Present when `id` has been deauthorized (frozen).
    Frozen(Address),
}

// ── Contract struct ───────────────────────────────────────────────────────────

#[contract]
pub struct MockToken;

// ── Implementation ────────────────────────────────────────────────────────────

#[contractimpl]
impl MockToken {
    // ── SEP-41 Token Interface ────────────────────────────────────────────────

    /// Returns 0 — allowance tracking is not implemented in this mock.
    pub fn allowance(_env: Env, _from: Address, _spender: Address) -> i128 {
        0
    }

    /// No-op — approval tracking is not implemented in this mock.
    pub fn approve(
        _env: Env,
        _from: Address,
        _spender: Address,
        _amount: i128,
        _expiration_ledger: u32,
    ) {
    }

    /// Returns the current balance of `id`.
    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&MockTokenKey::Balance(id))
            .unwrap_or(0_i128)
    }

    /// Transfer `amount` tokens from `from` to `to`.
    ///
    /// Requires `from.require_auth()`.  Panics if `fail_next_transfer()` was
    /// armed, clearing the flag before panicking so subsequent calls succeed.
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();

        if env
            .storage()
            .temporary()
            .get::<_, bool>(&MockTokenKey::FailNext)
            .unwrap_or(false)
        {
            env.storage().temporary().remove(&MockTokenKey::FailNext);
            panic!("mock token: forced transfer failure (fail_next_transfer was set)");
        }

        let from_bal: i128 = env
            .storage()
            .persistent()
            .get(&MockTokenKey::Balance(from.clone()))
            .unwrap_or(0);
        assert!(
            from_bal >= amount,
            "mock token: insufficient balance ({} < {})",
            from_bal,
            amount
        );
        env.storage()
            .persistent()
            .set(&MockTokenKey::Balance(from.clone()), &(from_bal - amount));

        let to_bal: i128 = env
            .storage()
            .persistent()
            .get(&MockTokenKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&MockTokenKey::Balance(to), &(to_bal + amount));
    }

    /// Spend `amount` from `from` to `to` using a pre-approved allowance.
    /// This mock does not enforce allowances; the transfer executes
    /// unconditionally.
    pub fn transfer_from(env: Env, _spender: Address, from: Address, to: Address, amount: i128) {
        let from_bal: i128 = env
            .storage()
            .persistent()
            .get(&MockTokenKey::Balance(from.clone()))
            .unwrap_or(0);
        assert!(
            from_bal >= amount,
            "mock token: insufficient balance for transfer_from"
        );
        env.storage()
            .persistent()
            .set(&MockTokenKey::Balance(from), &(from_bal - amount));

        let to_bal: i128 = env
            .storage()
            .persistent()
            .get(&MockTokenKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&MockTokenKey::Balance(to), &(to_bal + amount));
    }

    /// Burn `amount` tokens from `from`.  Requires `from.require_auth()`.
    pub fn burn(env: Env, from: Address, amount: i128) {
        from.require_auth();
        let bal: i128 = env
            .storage()
            .persistent()
            .get(&MockTokenKey::Balance(from.clone()))
            .unwrap_or(0);
        assert!(bal >= amount, "mock token: insufficient balance for burn");
        env.storage()
            .persistent()
            .set(&MockTokenKey::Balance(from), &(bal - amount));
    }

    /// No-op — allowance-based burn is not implemented in this mock.
    pub fn burn_from(_env: Env, _spender: Address, _from: Address, _amount: i128) {}

    /// Returns the token's decimal precision (7, matching Stellar conventions).
    pub fn decimals(_env: Env) -> u32 {
        7
    }

    /// Returns the token name.
    pub fn name(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, "Mock Token")
    }

    /// Returns the token symbol.
    pub fn symbol(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, "MCK")
    }

    /// SAC-compatible authorization flag; `true` unless frozen.
    pub fn authorized(env: Env, id: Address) -> bool {
        !env.storage().persistent().has(&MockTokenKey::Frozen(id))
    }

    /// SAC-compatible freeze/unfreeze (no admin check in this mock).
    pub fn set_authorized(env: Env, id: Address, authorize: bool) {
        if authorize {
            env.storage().persistent().remove(&MockTokenKey::Frozen(id));
        } else {
            env.storage()
                .persistent()
                .set(&MockTokenKey::Frozen(id), &true);
        }
    }

    // ── Mock-specific helpers ─────────────────────────────────────────────────

    /// Mint `amount` tokens into `to`'s balance without any authorization
    /// check — equivalent to a privileged admin mint for test setup.
    pub fn mint(env: Env, to: Address, amount: i128) {
        let bal: i128 = env
            .storage()
            .persistent()
            .get(&MockTokenKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&MockTokenKey::Balance(to), &(bal + amount));
    }

    /// Arm a one-shot flag: the very next call to `transfer()` on this
    /// contract will panic with a "forced transfer failure" message.
    /// The flag is cleared after it fires (or can be checked via the
    /// `FailNext` storage key).
    pub fn fail_next_transfer(env: Env) {
        env.storage()
            .temporary()
            .set(&MockTokenKey::FailNext, &true);
    }
}
