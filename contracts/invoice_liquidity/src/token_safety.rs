//! Token configuration and Stellar Asset Contract (SAC) safety controls.
//!
//! - Issue #1070: a token is only usable once its decimal precision is
//!   registered, and `add_token` cross-checks the declared precision against
//!   the token's own `decimals()` instead of trusting the admin argument.
//! - Issue #1071: issuer clawback can shrink the contract's balance below
//!   what it owes. Outbound payouts check the balance first and fail with a
//!   typed `InsufficientContractBalance` instead of an opaque host trap.
//! - Issue #1072: a frozen (deauthorized) trustline must not corrupt state or
//!   let one party block everyone else's settlement. Frozen *initiators* get a
//!   typed `AccountFrozen` before any transfer; frozen *third-party*
//!   recipients have their share escrowed as a pending payout they can
//!   claim once re-authorized.
//! - Issue #1073: `add_token` requires the SAC authorization interface and
//!   verifies the contract and admin are authorized holders.

use soroban_sdk::{
    contracttype,
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, Symbol,
};

use crate::errors::ContractError;
use crate::events::{PayoutEscrowed, PendingPayoutClaimed};
use crate::storage::DataKey;

/// Largest precision a token may register. `ten_pow` saturates beyond
/// 10^38, and no SEP-41 / SAC asset in use exceeds 18.
pub const MAX_TOKEN_DECIMALS: u32 = 18;

/// TTL bump for pending-payout entries, matching invoice core data.
const PENDING_PAYOUT_TTL_THRESHOLD: u32 = 1_000_000;
const PENDING_PAYOUT_TTL_EXTEND_TO: u32 = 2_000_000;

// ----------------------------------------------------------------
// Decimals (Issue #1070)
// ----------------------------------------------------------------

/// Validate `declared` against the token's self-reported `decimals()`.
///
/// Fails closed: a token that does not answer `decimals()` is rejected with
/// `TokenDecimalsUnavailable`; an out-of-range or mismatched value is
/// rejected with `InvalidTokenDecimals`.
pub fn validate_token_decimals(
    env: &Env,
    token: &Address,
    declared: u32,
) -> Result<(), ContractError> {
    if declared > MAX_TOKEN_DECIMALS {
        return Err(ContractError::InvalidTokenDecimals);
    }
    let on_chain = match TokenClient::new(env, token).try_decimals() {
        Ok(Ok(d)) => d,
        _ => return Err(ContractError::TokenDecimalsUnavailable),
    };
    if on_chain != declared {
        return Err(ContractError::InvalidTokenDecimals);
    }
    Ok(())
}

/// Registered decimals for `token`, or `TokenDecimalsNotConfigured`.
/// Replaces the former silent fallback to 6.
pub fn registered_token_decimals(env: &Env, token: &Address) -> Result<u32, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::TokenDecimals(token.clone()))
        .ok_or(ContractError::TokenDecimalsNotConfigured)
}

/// Issue #1069: a comparison between what this contract has registered for
/// `token` and what the token itself reports *right now*.
///
/// `initialize`'s three bootstrap tokens (USDC, EURC, XLM) are registered
/// with fixed decimals (6, 6, 7) that were never checked against the token
/// — unlike `add_token`, which always calls `validate_token_decimals`. See
/// docs/token-safety.md's "Known limitations": on Stellar Mainnet, classic-
/// asset SACs (including real USDC/EURC) report **7** decimals, so a
/// bootstrap deployment using the real SACs is very likely already
/// recording the wrong value for USDC/EURC specifically. This struct makes
/// that divergence observable on-chain instead of silently assumed away.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TokenDecimalsAudit {
    /// Decimals currently registered for `token` via `TokenDecimals`, if
    /// the token has ever been approved (`initialize` or `add_token`).
    pub registered: Option<u32>,
    /// `token`'s own `decimals()` report right now, if it successfully
    /// answers. `None` if the token doesn't implement `decimals()`, or
    /// there is no deployed contract at that address at all (e.g. a test
    /// placeholder passed for `eurc_token` at `initialize` time).
    pub on_chain: Option<u32>,
}

impl TokenDecimalsAudit {
    /// `true` only when both sides are known and agree. A token that can't
    /// report `decimals()` right now (`on_chain: None`) is *not* reported
    /// as mismatched — there's nothing to compare against, not evidence of
    /// a problem.
    pub fn matches(&self) -> bool {
        matches!((self.registered, self.on_chain), (Some(r), Some(o)) if r == o)
    }
}

/// Audit `token`'s registered decimals against its current on-chain
/// `decimals()`. Read-only, performs no storage write.
pub fn audit_token_decimals(env: &Env, token: &Address) -> TokenDecimalsAudit {
    let registered = env
        .storage()
        .persistent()
        .get(&DataKey::TokenDecimals(token.clone()));
    let on_chain = match TokenClient::new(env, token).try_decimals() {
        Ok(Ok(d)) => Some(d),
        _ => None,
    };
    TokenDecimalsAudit {
        registered,
        on_chain,
    }
}

// ----------------------------------------------------------------
// SAC authorization (Issues #1072, #1073)
// ----------------------------------------------------------------

/// `Some(flag)` when the token answers SAC `authorized(id)`, `None` when the
/// call fails (non-SAC token, or a classic account with no trustline).
fn query_authorized(env: &Env, token: &Address, id: &Address) -> Option<bool> {
    match StellarAssetClient::new(env, token).try_authorized(id) {
        Ok(Ok(flag)) => Some(flag),
        _ => None,
    }
}

/// True only when the token positively reports `id` as authorized.
pub fn is_authorized(env: &Env, token: &Address, id: &Address) -> bool {
    query_authorized(env, token, id) == Some(true)
}

/// Fail unless `id` may currently hold/move `token`: `AccountFrozen` when
/// deauthorized or without a trustline, `TokenAuthorizationUnavailable` when
/// the token has no SAC authorization interface at all. The extra probe only
/// runs on the failure path.
pub fn require_authorized(env: &Env, token: &Address, id: &Address) -> Result<(), ContractError> {
    match query_authorized(env, token, id) {
        Some(true) => Ok(()),
        Some(false) => Err(ContractError::AccountFrozen),
        None => {
            if query_authorized(env, token, &env.current_contract_address()).is_some() {
                Err(ContractError::AccountFrozen)
            } else {
                Err(ContractError::TokenAuthorizationUnavailable)
            }
        }
    }
}

/// `add_token` gate (Issue #1073): the token must expose the SAC
/// authorization interface, and both this contract and the admin (who
/// performs the fee-on-transfer probe) must be authorized holders.
pub fn validate_sac_authorization(
    env: &Env,
    token: &Address,
    admin: &Address,
) -> Result<(), ContractError> {
    match query_authorized(env, token, &env.current_contract_address()) {
        None => return Err(ContractError::TokenAuthorizationUnavailable),
        Some(false) => return Err(ContractError::AccountFrozen),
        Some(true) => {}
    }
    require_authorized(env, token, admin)
}

// ----------------------------------------------------------------
// Outbound payouts (Issues #1071, #1072)
// ----------------------------------------------------------------

/// Total escrowed for `token` across all recipients. These funds sit in the
/// contract's balance but are owed, so they are excluded from what new
/// payouts may spend.
pub fn pending_payout_total(env: &Env, token: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::PendingPayoutTotal(token.clone()))
        .unwrap_or(0)
}

pub fn pending_payout(env: &Env, recipient: &Address, token: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::PendingPayout(recipient.clone(), token.clone()))
        .unwrap_or(0)
}

fn set_pending(env: &Env, recipient: &Address, token: &Address, amount: i128, total: i128) {
    let key = DataKey::PendingPayout(recipient.clone(), token.clone());
    let total_key = DataKey::PendingPayoutTotal(token.clone());
    let store = env.storage().persistent();
    if amount > 0 {
        store.set(&key, &amount);
        store.extend_ttl(
            &key,
            PENDING_PAYOUT_TTL_THRESHOLD,
            PENDING_PAYOUT_TTL_EXTEND_TO,
        );
    } else {
        store.remove(&key);
    }
    if total > 0 {
        store.set(&total_key, &total);
        store.extend_ttl(
            &total_key,
            PENDING_PAYOUT_TTL_THRESHOLD,
            PENDING_PAYOUT_TTL_EXTEND_TO,
        );
    } else {
        store.remove(&total_key);
    }
}

/// Fail with `InsufficientContractBalance` unless the contract's
/// unreserved balance covers `amount` (`reserved` = escrow not being paid
/// out by this call).
fn ensure_spendable(
    env: &Env,
    client: &TokenClient,
    amount: i128,
    reserved: i128,
) -> Result<(), ContractError> {
    let balance = client.balance(&env.current_contract_address());
    if balance.saturating_sub(reserved) < amount {
        return Err(ContractError::InsufficientContractBalance);
    }
    Ok(())
}

/// Pay `to` from the contract, rejecting if `to` is frozen. Used when `to`
/// is the caller (so failing is their own outcome) or when paying a frozen
/// party would be unsafe (freelancer payout at funding time).
pub fn transfer_out_strict(
    env: &Env,
    token: &Address,
    to: &Address,
    amount: i128,
) -> Result<(), ContractError> {
    if amount <= 0 {
        return Ok(());
    }
    let client = TokenClient::new(env, token);
    ensure_spendable(env, &client, amount, pending_payout_total(env, token))?;
    require_authorized(env, token, to)?;
    client.transfer(&env.current_contract_address(), to, &amount);
    Ok(())
}

/// Pay a third-party recipient, escrowing instead of failing when their
/// trustline is frozen so they cannot block other parties' settlement.
/// Returns `true` if transferred, `false` if escrowed.
pub fn pay_or_escrow(
    env: &Env,
    token: &Address,
    to: &Address,
    amount: i128,
    invoice_id: u64,
) -> Result<bool, ContractError> {
    if amount <= 0 {
        return Ok(true);
    }
    let client = TokenClient::new(env, token);
    let total = pending_payout_total(env, token);
    ensure_spendable(env, &client, amount, total)?;

    if is_authorized(env, token, to) {
        client.transfer(&env.current_contract_address(), to, &amount);
        return Ok(true);
    }

    let owed = pending_payout(env, to, token)
        .checked_add(amount)
        .ok_or(ContractError::ArithmeticOverflow)?;
    let new_total = total
        .checked_add(amount)
        .ok_or(ContractError::ArithmeticOverflow)?;
    set_pending(env, to, token, owed, new_total);

    env.events().publish(
        (
            Symbol::new(env, "payout_escrowed"),
            to.clone(),
            token.clone(),
        ),
        PayoutEscrowed {
            invoice_id,
            recipient: to.clone(),
            token: token.clone(),
            amount,
            total_pending: owed,
        },
    );
    Ok(false)
}

/// Release an escrowed payout to `recipient` once their trustline is
/// authorized again. Caller must have authenticated `recipient`.
pub fn claim_pending(
    env: &Env,
    recipient: &Address,
    token: &Address,
) -> Result<i128, ContractError> {
    let amount = pending_payout(env, recipient, token);
    if amount <= 0 {
        return Err(ContractError::NothingToClaim);
    }
    require_authorized(env, token, recipient)?;

    let client = TokenClient::new(env, token);
    let total = pending_payout_total(env, token);
    // This claim's own escrow is part of `total`; only the rest is reserved.
    ensure_spendable(env, &client, amount, total.saturating_sub(amount))?;

    set_pending(env, recipient, token, 0, total.saturating_sub(amount));
    client.transfer(&env.current_contract_address(), recipient, &amount);

    env.events().publish(
        (
            Symbol::new(env, "pending_payout_claimed"),
            recipient.clone(),
            token.clone(),
        ),
        PendingPayoutClaimed {
            recipient: recipient.clone(),
            token: token.clone(),
            amount,
        },
    );
    Ok(amount)
}
