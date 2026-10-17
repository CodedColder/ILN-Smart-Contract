use crate::errors::ContractError;
use crate::invoice::{get_invoice_funders, invoice_exists, load_invoice, StorageKey};
use soroban_sdk::{contracttype, Address, Env, Symbol, Vec};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Role {
    Submitter,
    Payer,
    LP,
    Admin,
    Governance,
    Anyone,
}

pub fn require_admin(env: &Env) -> Result<(), ContractError> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&StorageKey::Admin)
        .ok_or(ContractError::Unauthorized)?;
    admin.require_auth();
    Ok(())
}

pub fn require_submitter(_env: &Env, caller: &Address) -> Result<(), ContractError> {
    caller.require_auth();
    Ok(())
}

pub fn require_submitter_by_id(
    env: &Env,
    caller: &Address,
    invoice_id: u64,
) -> Result<(), ContractError> {
    if !invoice_exists(env, invoice_id) {
        return Err(ContractError::InvoiceNotFound);
    }
    let invoice = load_invoice(env, invoice_id);
    caller.require_auth();
    if caller != &invoice.freelancer {
        return Err(ContractError::Unauthorized);
    }
    Ok(())
}

pub fn require_payer_by_id(env: &Env, invoice_id: u64) -> Result<(), ContractError> {
    if !invoice_exists(env, invoice_id) {
        return Err(ContractError::InvoiceNotFound);
    }
    let invoice = load_invoice(env, invoice_id);
    invoice.payer.require_auth();
    Ok(())
}

pub fn require_lp(_env: &Env, caller: &Address) -> Result<(), ContractError> {
    caller.require_auth();
    Ok(())
}

pub fn require_lp_by_id(env: &Env, caller: &Address, invoice_id: u64) -> Result<(), ContractError> {
    if !invoice_exists(env, invoice_id) {
        return Err(ContractError::InvoiceNotFound);
    }
    caller.require_auth();

    let funders = get_invoice_funders(env, invoice_id);
    let mut is_funder = false;
    for i in 0..funders.len() {
        if funders.get(i).unwrap().0 == *caller {
            is_funder = true;
            break;
        }
    }
    if !is_funder {
        return Err(ContractError::Unauthorized);
    }
    Ok(())
}

pub fn require_governance(_env: &Env) -> Result<(), ContractError> {
    // Currently no governance implemented, always reject
    Err(ContractError::Unauthorized)
}

// ----------------------------------------------------------------
// Reentrancy Guard (Issue #535)
// ----------------------------------------------------------------
// CEI (Checks-Effects-Interactions) pattern enforcement:
//   All state-changing functions MUST perform state mutations BEFORE
//   any external calls (token transfers, cross-contract invocations).
//   The guards below provide defense-in-depth for critical paths.

/// Activate the reentrancy lock. Returns `Reentrancy` error if already locked.
pub fn lock_reentrancy(env: &Env) -> Result<(), ContractError> {
    let locked: bool = env
        .storage()
        .instance()
        .get(&StorageKey::ReentrancyLock)
        .unwrap_or(false);
    if locked {
        return Err(ContractError::Reentrancy);
    }
    env.storage()
        .instance()
        .set(&StorageKey::ReentrancyLock, &true);
    Ok(())
}

/// Deactivate the reentrancy lock.
pub fn unlock_reentrancy(env: &Env) {
    env.storage()
        .instance()
        .set(&StorageKey::ReentrancyLock, &false);
}

// ----------------------------------------------------------------
// Rate Limiting (Issue #541)
// ----------------------------------------------------------------
//
// Design:
//   Each rate-limited function is keyed by a Symbol (its function name).
//   On each call, we check whether enough ledgers have elapsed since the
//   last recorded call. If not, the call is rejected with RateLimited.
//
//   Cooldown defaults are set per function category:
//     - Admin transfer: ADMIN_CHANGE_COOLDOWN_LEDGERS (720 ledgers ≈ 1h)
//     - Contract upgrade: UPGRADE_COOLDOWN_LEDGERS (1440 ledgers ≈ 2h)
//     - Economic params: ECONOMIC_PARAM_COOLDOWN_LEDGERS (360 ledgers ≈ 30min)
//     - General: DEFAULT_RATE_LIMIT_LEDGERS (120 ledgers ≈ 10min)
//
//   Emergency functions (pause/unpause) are deliberately exempt.

/// Check whether the given rate-limited function may be called.
/// Returns `RateLimited` if the cooldown has not yet elapsed since this
/// function's last recorded call. Otherwise records the current ledger as
/// the last call time.
///
/// The first-ever call of a function always passes: no record exists yet,
/// so there is no prior call to space it out from. (The previous
/// `unwrap_or(0)` default made `current < 0 + cooldown` reject a cold
/// start whenever the ledger sequence was still below the cooldown —
/// only observable on low-sequence test/local networks, since mainnet's
/// sequence has always been far above every cooldown. Rate limiting
/// spaces *consecutive* calls; it must not gate a function's first use.)
pub fn check_rate_limit(
    env: &Env,
    fn_name: &str,
    cooldown_ledgers: u64,
) -> Result<(), ContractError> {
    let key = StorageKey::RateLimit(Symbol::new(env, fn_name));
    let last_ledger: Option<u32> = env.storage().instance().get(&key);
    let current_ledger = env.ledger().sequence();

    if let Some(last) = last_ledger {
        if current_ledger < last.saturating_add(cooldown_ledgers as u32) {
            return Err(ContractError::RateLimited);
        }
    }

    env.storage().instance().set(&key, &current_ledger);
    Ok(())
}

/// Clear a rate-limit record (for testing or emergency bypass by admin).
pub fn clear_rate_limit(env: &Env, fn_name: &str) {
    let key = StorageKey::RateLimit(Symbol::new(env, fn_name));
    env.storage().instance().remove(&key);
}

// ----------------------------------------------------------------
// Admin Action Audit Log (Issue #645)
// ----------------------------------------------------------------
//
// A bounded on-chain ring buffer of the most recently *executed* admin
// actions, so the log can be queried directly (`get_recent_admin_actions`)
// instead of replaying the full Horizon event stream to answer "what has
// the admin done recently". This complements, rather than replaces, the
// existing per-action events (AdminChanged, ParameterUpdated, TokenAdded,
// ...) — those remain the durable, unbounded audit trail; this view only
// retains the last ADMIN_ACTION_LOG_CAPACITY entries for cheap lookups.
//
// `record_admin_action` must only be called after `require_admin` has
// already succeeded for the current invocation. Soroban transactions are
// atomic (see threat model F1), so if the calling function later returns
// an error, this write is rolled back along with the rest of the
// transaction — only genuinely executed actions remain in the log.

/// Number of most-recent admin actions retained in the ring buffer.
pub const ADMIN_ACTION_LOG_CAPACITY: u32 = 50;

/// One entry in the admin action audit log.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminActionRecord {
    /// Monotonically increasing sequence number (never reused, even once
    /// the corresponding ring-buffer slot is overwritten).
    pub seq: u64,
    /// Name of the admin entry point that was called (e.g. "pause",
    /// "add_token").
    pub action: Symbol,
    /// The admin address that authorized the call.
    pub admin: Address,
    /// Ledger sequence at which the action executed.
    pub ledger: u32,
    /// Ledger close timestamp at which the action executed.
    pub timestamp: u64,
}

/// Append an entry to the admin action audit log. See module-level docs
/// above for the atomicity argument that makes this safe to call
/// immediately after `require_admin` succeeds, before the rest of the
/// calling function's logic has run.
pub fn record_admin_action(env: &Env, action: &str) {
    let admin: Address = match env.storage().instance().get(&StorageKey::Admin) {
        Some(admin) => admin,
        None => return,
    };
    let seq: u64 = env
        .storage()
        .instance()
        .get(&StorageKey::AdminActionCount)
        .unwrap_or(0);
    let record = AdminActionRecord {
        seq,
        action: Symbol::new(env, action),
        admin,
        ledger: env.ledger().sequence(),
        timestamp: env.ledger().timestamp(),
    };
    let slot = (seq % ADMIN_ACTION_LOG_CAPACITY as u64) as u32;
    env.storage()
        .persistent()
        .set(&StorageKey::AdminActionLog(slot), &record);
    env.storage()
        .instance()
        .set(&StorageKey::AdminActionCount, &(seq + 1));
}

/// Return up to `limit` most recent admin actions, newest first. `limit`
/// is capped at `ADMIN_ACTION_LOG_CAPACITY` (the ring buffer never holds
/// more than that regardless of how many actions have ever executed).
pub fn get_recent_admin_actions(env: &Env, limit: u32) -> Vec<AdminActionRecord> {
    let total: u64 = env
        .storage()
        .instance()
        .get(&StorageKey::AdminActionCount)
        .unwrap_or(0);
    let count = (limit.min(ADMIN_ACTION_LOG_CAPACITY) as u64).min(total);
    let mut out = Vec::new(env);
    for i in 0..count {
        let seq = total - 1 - i;
        let slot = (seq % ADMIN_ACTION_LOG_CAPACITY as u64) as u32;
        if let Some(record) = env
            .storage()
            .persistent()
            .get::<StorageKey, AdminActionRecord>(&StorageKey::AdminActionLog(slot))
        {
            out.push_back(record);
        }
    }
    out
}
