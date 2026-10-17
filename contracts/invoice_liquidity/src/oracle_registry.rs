//! Governance-controlled oracle registry (Issue #532).
//!
//! Before this, the contract had a single `Config.price_oracle` field used
//! for payer identity/creditworthiness verification in `fund_invoice`. That
//! doesn't scale to a protocol that wants multiple *kinds* of oracle data
//! (price feeds, identity verification, credit scoring) or different oracle
//! providers per token (e.g. a USDC price feed vs an XLM price feed).
//!
//! This module adds a registry keyed by `OracleFeedType`, with an optional
//! per-token override on top of a feed-type-wide default. Resolution order
//! (see `resolve_oracle`):
//!   1. Per-token override for this feed type, if registered.
//!   2. Feed-type-wide default, if registered.
//!   3. The legacy `Config.price_oracle` field (kept for backwards
//!      compatibility with contracts/tests that only ever called
//!      `set_price_oracle`).
//!
//! Registration is governance-controlled the same way `update_fee_rate` /
//! `add_token` are: gated by `require_admin`, with governance driving it via
//! a cross-contract proposal execution where the ILN contract's stored admin
//! is the governance contract's own address.

use soroban_sdk::{contracttype, vec, Address, Env, IntoVal, Symbol};

use crate::access::{check_rate_limit, require_admin};
use crate::constants::DEFAULT_RATE_LIMIT_LEDGERS;
use crate::errors::ContractError;
use crate::events::{
    OracleCircuitReset, OracleCircuitTripped, OracleHealthRecorded, OracleRegistered,
    OracleUnregistered, PriceOutlierRejected, PriceSourceAdded, PriceSourceRemoved,
    TwapEnabledForFeed, TwapWindowUpdated, TwapInsufficientData,
};
use crate::oracle_interface::{OracleClient, ORACLE_INTERFACE_VERSION};
use crate::storage::DataKey;
use crate::OracleVerificationResponse;

/// Number of consecutive stale queries against the same oracle before its
/// resolution channel is automatically circuit-tripped: further oracle-gated
/// funding treats it as unavailable and falls back through the priority
/// chain (or is rejected if nothing else resolves) until governance calls
/// `reset_oracle_circuit`.
pub const MAX_CONSECUTIVE_STALE_QUERIES: u32 = 3;

/// The kind of off-chain data an oracle provides.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OracleFeedType {
    /// Token/asset price feed (e.g. for USD normalisation).
    Price,
    /// Payer identity verification (the pre-#532 `price_oracle` use case).
    Identity,
    /// Payer creditworthiness / credit scoring.
    Credit,
}

/// Point-in-time health snapshot for an oracle, recorded whenever
/// `fund_invoice` (or another caller) queries it. There is no network
/// latency to measure on-chain — "response time" here means data
/// staleness: how many ledgers old the oracle's returned timestamp was
/// at query time.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleHealthStatus {
    pub oracle: Address,
    /// Ledger sequence at which this snapshot was recorded.
    pub last_checked_ledger: u32,
    /// The `timestamp` field the oracle returned in its response.
    pub last_data_timestamp: u32,
    /// `last_checked_ledger - last_data_timestamp`, i.e. how stale the data
    /// was at query time.
    pub last_data_age_ledgers: u64,
    /// Whether `last_data_age_ledgers` exceeded the configured max age.
    pub is_stale: bool,
    /// Number of consecutive queries (across calls) that returned stale
    /// data. Resets to 0 on a fresh (non-stale) response.
    pub consecutive_stale_count: u32,
}

/// Register (or update) the default oracle for `feed_type`. Applies to every
/// token that doesn't have a more specific per-token override.
///
/// Access: Admin only (in production, the ILN contract's stored admin is set
/// to the governance contract's address, so this is effectively
/// governance-controlled via a proposal).
///
/// Rejects oracles that do not report a compatible [`ORACLE_INTERFACE_VERSION`].
pub fn register_oracle(
    env: &Env,
    feed_type: OracleFeedType,
    oracle: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "register_oracle", DEFAULT_RATE_LIMIT_LEDGERS)?;
    let version = verify_oracle_interface_version(env, &oracle)?;
    env.storage()
        .instance()
        .set(&DataKey::OracleRegistry(feed_type), &oracle);
    crate::storage::set_oracle_interface_version(env, feed_type, version);
    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "oracle_registered"),
            feed_type,
        ),
        OracleRegistered {
            feed_type,
            token: None,
            oracle,
        },
    );
    Ok(())
}

/// Remove the default oracle for `feed_type`. Per-token overrides for that
/// feed type, if any, are untouched.
///
/// Access: Admin only.
pub fn remove_oracle(env: &Env, feed_type: OracleFeedType) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "remove_oracle", DEFAULT_RATE_LIMIT_LEDGERS)?;
    env.storage()
        .instance()
        .remove(&DataKey::OracleRegistry(feed_type));
    env.events().publish(
        (soroban_sdk::Symbol::new(env, "oracle_removed"), feed_type),
        OracleUnregistered {
            feed_type,
            token: None,
        },
    );
    Ok(())
}

/// Register (or update) a per-token override oracle for `feed_type`. Takes
/// priority over the feed-type-wide default when resolving the oracle for
/// this exact token.
///
/// Access: Admin only.
///
/// Rejects oracles that do not report a compatible [`ORACLE_INTERFACE_VERSION`].
pub fn register_token_oracle(
    env: &Env,
    feed_type: OracleFeedType,
    token: Address,
    oracle: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "register_token_oracle", DEFAULT_RATE_LIMIT_LEDGERS)?;
    let version = verify_oracle_interface_version(env, &oracle)?;
    env.storage()
        .persistent()
        .set(&DataKey::TokenOracle(feed_type, token.clone()), &oracle);
    crate::storage::set_oracle_interface_version(env, feed_type, version);
    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "oracle_registered"),
            feed_type,
        ),
        OracleRegistered {
            feed_type,
            token: Some(token),
            oracle,
        },
    );
    Ok(())
}

/// Remove a per-token override oracle for `feed_type`. The feed-type-wide
/// default (if any) then applies again for this token.
///
/// Access: Admin only.
pub fn remove_token_oracle(
    env: &Env,
    feed_type: OracleFeedType,
    token: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "remove_token_oracle", DEFAULT_RATE_LIMIT_LEDGERS)?;
    env.storage()
        .persistent()
        .remove(&DataKey::TokenOracle(feed_type, token.clone()));
    env.events().publish(
        (soroban_sdk::Symbol::new(env, "oracle_removed"), feed_type),
        OracleUnregistered {
            feed_type,
            token: Some(token),
        },
    );
    Ok(())
}

/// Resolve the oracle address to query for `feed_type` + `token`, in
/// priority order: per-token override, then feed-type default, then (for
/// `Identity` only) the legacy `Config.price_oracle` field.
pub fn resolve_oracle(env: &Env, feed_type: OracleFeedType, token: &Address) -> Option<Address> {
    if let Some(addr) = env
        .storage()
        .persistent()
        .get(&DataKey::TokenOracle(feed_type, token.clone()))
    {
        return Some(addr);
    }
    if let Some(addr) = env
        .storage()
        .instance()
        .get(&DataKey::OracleRegistry(feed_type))
    {
        return Some(addr);
    }
    if feed_type == OracleFeedType::Identity {
        return crate::storage::get_config(env).and_then(|c| c.price_oracle);
    }
    None
}

/// Public getter mirroring `resolve_oracle`, for external callers / SDK.
pub fn get_oracle_for_token(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
) -> Option<Address> {
    resolve_oracle(&env, feed_type, &token)
}

/// Record a health snapshot for the oracle that served `feed_type` + `token`
/// at the current ledger, given the data timestamp it returned and the max
/// age threshold that was applied.
///
/// Note: Soroban rolls back ALL storage writes made during an invocation
/// that returns `Err` (there is no partial commit) — so a health snapshot
/// written here is only durable if the *overall* call that invoked this
/// function goes on to return `Ok`. `fund_invoice` calls this before its own
/// staleness check, so health IS recorded whenever funding succeeds, but a
/// rejected (stale/unverified) `fund_invoice` call rolls its health write
/// back along with everything else. For monitoring that must observe
/// staleness even when funding would be rejected, use
/// `check_oracle_health` instead, which never errors.
pub fn record_oracle_health(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
    oracle: &Address,
    data_timestamp: u32,
    max_age_ledgers: u64,
) {
    let current_ledger = env.ledger().sequence();
    let age = (current_ledger as u64).saturating_sub(data_timestamp as u64);
    let is_stale = max_age_ledgers > 0 && age >= max_age_ledgers;

    let key = DataKey::OracleHealth(feed_type, token.clone());
    let previous: Option<OracleHealthStatus> = env.storage().persistent().get(&key);
    // A streak only counts against the *same* oracle address: if resolution
    // just fell back to a different oracle (e.g. because the prior one
    // tripped the circuit breaker below), that oracle's own reliability
    // hasn't been observed yet and shouldn't inherit the old one's count.
    let consecutive_stale_count = match &previous {
        Some(prev) if prev.oracle == *oracle && is_stale => {
            prev.consecutive_stale_count.saturating_add(1)
        }
        _ if is_stale => 1,
        _ => 0,
    };

    let status = OracleHealthStatus {
        oracle: oracle.clone(),
        last_checked_ledger: current_ledger,
        last_data_timestamp: data_timestamp,
        last_data_age_ledgers: age,
        is_stale,
        consecutive_stale_count,
    };
    env.storage().persistent().set(&key, &status);

    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "oracle_health_recorded"),
            feed_type,
        ),
        OracleHealthRecorded {
            feed_type,
            token: token.clone(),
            is_stale,
            last_data_age_ledgers: age,
            consecutive_stale_count,
        },
    );

    // Circuit breaker: trip the first time the streak crosses the
    // threshold. Guarded on the *currently persisted* flag (read fresh here,
    // not inferred from the numeric streak) so this fires once per trip —
    // not on every subsequent stale query while already tripped — but
    // still fires again promptly if governance resets the breaker and the
    // very next query is still stale, even though the raw counter (which
    // `reset_oracle_circuit` deliberately doesn't touch) never dipped below
    // threshold in between. The breaker stays tripped after firing
    // regardless of what consecutive_stale_count does next (including
    // resetting to 0 on a later fresh query) until that explicit reset.
    let already_tripped = is_oracle_circuit_tripped(env, feed_type, token);
    if is_stale && consecutive_stale_count >= MAX_CONSECUTIVE_STALE_QUERIES && !already_tripped {
        let circuit_key = DataKey::OracleCircuitTripped(feed_type, token.clone());
        env.storage().persistent().set(&circuit_key, &true);
        env.events().publish(
            (
                soroban_sdk::Symbol::new(env, "oracle_circuit_tripped"),
                feed_type,
            ),
            OracleCircuitTripped {
                feed_type,
                token: token.clone(),
                consecutive_stale_count,
            },
        );
    }
}

/// Whether the oracle circuit breaker for `feed_type` + `token` is
/// currently tripped. Sticky — sees only `reset_oracle_circuit`, never a
/// fresh query, per Issue requirement to avoid flapping.
pub fn is_oracle_circuit_tripped(env: &Env, feed_type: OracleFeedType, token: &Address) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::OracleCircuitTripped(feed_type, token.clone()))
        .unwrap_or(false)
}

/// Governance-gated reset of a tripped oracle circuit breaker. Requires an
/// explicit call — there is no automatic recovery on a single fresh query,
/// so a flapping oracle can't quietly resume being trusted without someone
/// (governance, in production, via the admin=governance-contract
/// convention used throughout this registry) affirmatively deciding it's
/// safe again.
///
/// Access: Admin only.
pub fn reset_oracle_circuit(
    env: &Env,
    feed_type: OracleFeedType,
    token: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "reset_oracle_circuit", DEFAULT_RATE_LIMIT_LEDGERS)?;
    env.storage()
        .persistent()
        .remove(&DataKey::OracleCircuitTripped(feed_type, token.clone()));
    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "oracle_circuit_reset"),
            feed_type,
        ),
        OracleCircuitReset { feed_type, token },
    );
    Ok(())
}

/// Resolution outcome for `fund_invoice`'s oracle-gated funding check
/// specifically. Unlike the plain `resolve_oracle` (used for read-only
/// views and `check_oracle_health`, which must stay circuit-agnostic so
/// monitoring keeps observing through failures instead of going blind the
/// moment a breaker trips), this respects an open circuit and falls back
/// to the next entry in the priority chain — or signals that funding must
/// be rejected outright if nothing usable remains.
#[derive(Debug, PartialEq, Eq)]
pub enum OracleResolution {
    /// Nothing registered at any priority level — existing fail-open
    /// behavior applies (verification is a no-op).
    Unconfigured,
    /// A usable (not circuit-tripped) oracle resolved.
    Available(Address),
    /// Every candidate in the priority chain is circuit-tripped, with
    /// nothing left to fall back to.
    CircuitOpen,
}

/// Circuit-aware counterpart to `resolve_oracle`, used only by
/// `fund_invoice`. Walks the same three-level priority chain
/// (per-token override, feed-type default, legacy `price_oracle` for
/// `Identity`), skipping exactly the oracle address recorded as having
/// caused the trip (if any), and returning the first candidate that isn't
/// it.
///
/// Known simplification: if governance replaces the tripped registration
/// with a new address *without* also calling `reset_oracle_circuit`, this
/// correctly resolves to the new address immediately (it's never been
/// observed stale), but `is_oracle_circuit_tripped` keeps reporting `true`
/// until the explicit reset call — a harmless staleness in the flag itself,
/// not in funding behavior.
pub fn resolve_oracle_for_verification(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
) -> OracleResolution {
    let tripped = is_oracle_circuit_tripped(env, feed_type, token);
    let tripped_oracle: Option<Address> = if tripped {
        get_oracle_health(env.clone(), feed_type, token.clone()).map(|h| h.oracle)
    } else {
        None
    };
    // Fail closed if we somehow can't identify what tripped — see doc above.
    let is_excluded = |addr: &Address| -> bool {
        if !tripped {
            return false;
        }
        match &tripped_oracle {
            Some(bad) => addr == bad,
            None => true,
        }
    };

    let mut saw_candidate = false;

    if let Some(addr) = env
        .storage()
        .persistent()
        .get::<DataKey, Address>(&DataKey::TokenOracle(feed_type, token.clone()))
    {
        saw_candidate = true;
        if !is_excluded(&addr) {
            return OracleResolution::Available(addr);
        }
    }

    if let Some(addr) = env
        .storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::OracleRegistry(feed_type))
    {
        saw_candidate = true;
        if !is_excluded(&addr) {
            return OracleResolution::Available(addr);
        }
    }

    if feed_type == OracleFeedType::Identity {
        if let Some(addr) = crate::storage::get_config(env).and_then(|c| c.price_oracle) {
            saw_candidate = true;
            if !is_excluded(&addr) {
                return OracleResolution::Available(addr);
            }
        }
    }

    if saw_candidate {
        OracleResolution::CircuitOpen
    } else {
        OracleResolution::Unconfigured
    }
}

/// Public getter for the last recorded health snapshot of `feed_type` +
/// `token`. `None` if never queried.
pub fn get_oracle_health(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
) -> Option<OracleHealthStatus> {
    env.storage()
        .persistent()
        .get(&DataKey::OracleHealth(feed_type, token))
}

/// Actively query the oracle resolved for `feed_type` + `token` (for
/// `payer`'s record) and record + return its current health status.
///
/// Unlike `fund_invoice`'s inline oracle check, this NEVER errors on
/// stale/unverified data — it just reports the observation — so the write
/// always commits. This is the entrypoint off-chain monitors/keepers
/// should poll to track oracle staleness over time, independent of (and
/// without needing to trigger or revert) any funding activity.
///
/// Returns `None` if no oracle resolves for this `feed_type` + `token`.
pub fn check_oracle_health(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
    payer: Address,
) -> Option<OracleHealthStatus> {
    let oracle_addr = resolve_oracle(&env, feed_type, &token)?;
    let response: OracleVerificationResponse = env.invoke_contract(
        &oracle_addr,
        &Symbol::new(&env, "get_payer_data"),
        vec![&env, payer.into_val(&env)],
    );
    let max_age = crate::storage::get_config(&env)
        .map(|c| c.max_oracle_age_ledgers)
        .unwrap_or(crate::DEFAULT_MAX_ORACLE_AGE_LEDGERS);
    record_oracle_health(
        &env,
        feed_type,
        &token,
        &oracle_addr,
        response.timestamp,
        max_age,
    );
    env.storage()
        .persistent()
        .get(&DataKey::OracleHealth(feed_type, token))
}

/// Query `oracle.interface_version()` and reject incompatible / missing
/// implementations before persisting a registry entry.
fn verify_oracle_interface_version(env: &Env, oracle: &Address) -> Result<u32, ContractError> {
    let client = OracleClient::new(env, oracle);
    let version = match client.try_interface_version() {
        Ok(Ok(v)) => v,
        _ => return Err(ContractError::IncompatibleInterfaceVersion),
    };
    if version != ORACLE_INTERFACE_VERSION {
        return Err(ContractError::IncompatibleInterfaceVersion);
    }
    Ok(version)
}

// ── Multi-source price deviation checking (Issue #price-deviation) ────────────
//
// The single-oracle-per-resolution model above (register_oracle /
// register_token_oracle, resolve_oracle) is appropriate for the boolean
// Identity-feed payer-verification case, but offers no defense against a
// single misbehaving or compromised oracle reporting a wildly incorrect
// PRICE: there's nothing on-chain to compare it against. This section adds
// an optional, separate multi-source registration list per feed type
// specifically for numeric price data, so any one registered source's
// report can be cross-checked against the median of every other
// registered source before being trusted.
//
// **Single-source risk is explicit, not silent.** If only one price
// source is ever registered for a feed type, `get_verified_price` returns
// that source's price unchecked — there is nothing else to compare it
// against, and no amount of code here can manufacture a second opinion
// out of one data point. This is an accepted, documented risk of running a
// single-source price feed (see docs/oracle-attack-economics.md for the
// broader single-source oracle-manipulation cost/benefit model this
// compounds with), not an oversight — registering at least two independent
// price sources is what actually activates this module's protection.
//
// **Degenerate two-source case.** With exactly two sources, "median" is
// their average, and a single outlier deviates from that average by
// exactly the same amount the honest source does (the average sits
// equidistant from both, by construction) — there is no way to tell which
// of two disagreeing sources is lying from two data points alone. Past the
// configured threshold this correctly rejects *both* rather than guessing
// (`AllPriceSourcesRejected`), which is safer than arbitrarily trusting
// one; it does not selectively exclude "the" outlier the way three or more
// sources allows.

/// Default deviation threshold (5%, 500 bps) applied when no governance
/// value has been configured yet.
pub const DEFAULT_MAX_PRICE_DEVIATION_BPS: u32 = 500;

/// Register `oracle` as an additional price source for `feed_type`. A
/// no-op (not an error) if already registered. Unlike
/// `register_oracle`/`register_token_oracle`, this performs no
/// interface-version handshake — price sources are queried dynamically via
/// `try_invoke_contract` and a non-responding/incompatible source is
/// simply excluded from the sample at query time (see `query_price`)
/// rather than rejected at registration.
///
/// Access: Admin only (governance-controlled via the same
/// admin=governance-contract convention used throughout this registry).
pub fn add_price_source(
    env: &Env,
    feed_type: OracleFeedType,
    oracle: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "add_price_source", DEFAULT_RATE_LIMIT_LEDGERS)?;
    let key = DataKey::PriceSources(feed_type);
    let mut sources: soroban_sdk::Vec<Address> = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));
    if !sources.iter().any(|existing| existing == oracle) {
        sources.push_back(oracle.clone());
        env.storage().instance().set(&key, &sources);
    }
    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "price_source_added"),
            feed_type,
        ),
        PriceSourceAdded { feed_type, oracle },
    );
    Ok(())
}

/// Remove `oracle` from `feed_type`'s price source list. A no-op if it
/// wasn't registered.
///
/// Access: Admin only.
pub fn remove_price_source(
    env: &Env,
    feed_type: OracleFeedType,
    oracle: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "remove_price_source", DEFAULT_RATE_LIMIT_LEDGERS)?;
    let key = DataKey::PriceSources(feed_type);
    let sources: soroban_sdk::Vec<Address> = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));
    let mut remaining: soroban_sdk::Vec<Address> = soroban_sdk::Vec::new(env);
    for existing in sources.iter() {
        if existing != oracle {
            remaining.push_back(existing);
        }
    }
    env.storage().instance().set(&key, &remaining);
    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "price_source_removed"),
            feed_type,
        ),
        PriceSourceRemoved { feed_type, oracle },
    );
    Ok(())
}

/// The currently registered price sources for `feed_type`.
pub fn get_price_sources(env: Env, feed_type: OracleFeedType) -> soroban_sdk::Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::PriceSources(feed_type))
        .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
}

/// Update the governance-configurable maximum deviation (basis points) a
/// price source may differ from the cross-source median before being
/// rejected as an outlier. Rejects `0` (would reject every source but an
/// exact median match) and values above `10_000` (100% — meaningless as a
/// deviation cap).
///
/// Access: Admin only.
pub fn set_max_price_deviation_bps(env: &Env, bps: u32) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(
        env,
        "set_max_price_deviation_bps",
        DEFAULT_RATE_LIMIT_LEDGERS,
    )?;
    if bps == 0 || bps > 10_000 {
        return Err(ContractError::InvalidAmount);
    }
    env.storage()
        .instance()
        .set(&DataKey::MaxPriceDeviationBps, &bps);
    Ok(())
}

/// The currently configured maximum price deviation, in basis points.
pub fn get_max_price_deviation_bps(env: Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::MaxPriceDeviationBps)
        .unwrap_or(DEFAULT_MAX_PRICE_DEVIATION_BPS)
}

/// Issue #860: circuit/health gate every price read must pass before it
/// trusts oracle data.
///
/// - Circuit tripped for `feed_type` + `token` → `Err(OracleCircuitOpen)`.
/// - Last recorded health snapshot says the data was stale →
///   `Err(OracleDataStale)`.
///
/// No recorded health (never queried) passes — same fail-open posture the
/// rest of the registry takes for un-configured state. Callers that return
/// `Option` (e.g. `get_twap_price`) map the error to `None`.
pub(crate) fn require_healthy_price_feed(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
) -> Result<(), ContractError> {
    if is_oracle_circuit_tripped(env, feed_type, token) {
        return Err(ContractError::OracleCircuitOpen);
    }
    if let Some(health) = get_oracle_health(env.clone(), feed_type, token.clone()) {
        if health.is_stale {
            return Err(ContractError::OracleDataStale);
        }
    }
    Ok(())
}

/// Query `oracle.get_price(token)`, returning `None` (rather than
/// propagating a panic) if the call fails, traps, or returns a value that
/// doesn't decode as `i128` — a single bad price source degrades to "no
/// opinion" instead of taking down the whole aggregation.
///
/// `pub(crate)` so `invoice::get_price_from_oracle` routes its stats
/// normalization read through this non-panicking path (Issue #860) instead
/// of a bare `invoke_contract`.
pub(crate) fn query_price(env: &Env, oracle: &Address, token: &Address) -> Option<i128> {
    // Turbofish on T/E only (matching iln_governance::invoke_and_check's
    // established pattern) — T's own TryFromVal::Error (ConversionError for
    // i128) is inferred, not spelled out, and E is never actually
    // constructed here since we only match on the outer shape.
    let result = env.try_invoke_contract::<i128, soroban_sdk::Error>(
        oracle,
        &Symbol::new(env, "get_price"),
        vec![env, token.into_val(env)],
    );
    match result {
        Ok(Ok(price)) => Some(price),
        _ => None,
    }
}

/// Sorts a copy of `values` (simple insertion sort — the expected number of
/// price sources is small) and returns the median: the middle element for
/// an odd count, or the average (integer division, floored) of the two
/// middle elements for an even count. Panics only if `values` is empty —
/// every caller below checks that first.
fn median(values: &soroban_sdk::Vec<i128>) -> i128 {
    let len = values.len();
    let mut sorted: soroban_sdk::Vec<i128> = values.clone();
    for i in 1..len {
        let key = sorted.get(i).unwrap();
        let mut j = i;
        while j > 0 && sorted.get(j - 1).unwrap() > key {
            let prev = sorted.get(j - 1).unwrap();
            sorted.set(j, prev);
            j -= 1;
        }
        sorted.set(j, key);
    }
    if len % 2 == 1 {
        sorted.get(len / 2).unwrap()
    } else {
        let a = sorted.get(len / 2 - 1).unwrap();
        let b = sorted.get(len / 2).unwrap();
        (a + b) / 2
    }
}

/// Deviation of `price` from `reference`, in basis points. `reference == 0`
/// is a degenerate case handled explicitly: only `price == 0` is
/// considered non-deviating — any nonzero price against a zero reference
/// is treated as maximally deviant, guaranteed to exceed any valid
/// governance-configured threshold (those are capped at `10_000`).
fn deviation_bps(price: i128, reference: i128) -> u32 {
    if reference == 0 {
        return if price == 0 { 0 } else { u32::MAX };
    }
    let diff = (price - reference).abs();
    let bps = diff.saturating_mul(10_000) / reference.abs();
    bps.clamp(0, u32::MAX as i128) as u32
}

/// Query every registered price source for `feed_type` + `token` and
/// return a cross-validated price, defending against a single misbehaving
/// or compromised source reporting a wildly incorrect value.
///
/// - **Zero sources** (or every registered source failed to respond):
///   `Err(ContractError::NoPriceSource)`.
/// - **Exactly one source**: its price is returned **unchecked** — the
///   documented, accepted single-point-of-failure risk described in this
///   module's header comment, not a bug. There is nothing to cross-check a
///   single source against.
/// - **Two or more sources**: the median of all successfully-queried
///   prices is computed; any individual source deviating from that median
///   by more than `get_max_price_deviation_bps()` is excluded (emitting
///   `PriceOutlierRejected` per exclusion), and the median of the
///   *surviving* sources is returned. If every source is mutually
///   rejected (pathological — no cluster of agreement at all), returns
///   `Err(ContractError::AllPriceSourcesRejected)`.
///
/// Issue #860: before any of the above, the read consults the health-check /
/// circuit-breaker state for `feed_type` + `token` — an open circuit
/// (`Err(ContractError::OracleCircuitOpen)`) or a stale health snapshot
/// (`Err(ContractError::OracleDataStale)`) rejects the price outright
/// instead of trusting data the registry has already observed to be bad.
pub fn get_verified_price(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
) -> Result<i128, ContractError> {
    require_healthy_price_feed(&env, feed_type, &token)?;
    // Issue #816: per-feed opt-in — when TWAP is enabled and enough
    // in-window samples exist, read the windowed average instead of spot.
    // Falls through to the spot path below while samples backfill.
    if is_twap_enabled(&env, feed_type) {
        if let Some(avg) = get_twap_price(&env, feed_type, &token) {
            return Ok(avg);
        }
    }
    let sources = get_price_sources(env.clone(), feed_type);
    let mut prices: soroban_sdk::Vec<i128> = soroban_sdk::Vec::new(&env);
    let mut priced_sources: soroban_sdk::Vec<Address> = soroban_sdk::Vec::new(&env);
    for oracle in sources.iter() {
        if let Some(price) = query_price(&env, &oracle, &token) {
            prices.push_back(price);
            priced_sources.push_back(oracle);
        }
    }

    if prices.is_empty() {
        return Err(ContractError::NoPriceSource);
    }
    if prices.len() == 1 {
        // Single source: nothing to cross-check against. Documented,
        // accepted risk — see this module's header comment.
        return Ok(prices.get(0).unwrap());
    }

    let overall_median = median(&prices);
    let max_deviation = get_max_price_deviation_bps(env.clone());

    let mut survivors: soroban_sdk::Vec<i128> = soroban_sdk::Vec::new(&env);
    for i in 0..prices.len() {
        let price = prices.get(i).unwrap();
        let oracle = priced_sources.get(i).unwrap();
        let dev = deviation_bps(price, overall_median);
        if dev > max_deviation {
            env.events().publish(
                (
                    soroban_sdk::Symbol::new(&env, "price_outlier_rejected"),
                    feed_type,
                ),
                PriceOutlierRejected {
                    feed_type,
                    token: token.clone(),
                    oracle,
                    reported_price: price,
                    median_price: overall_median,
                    deviation_bps: dev,
                },
            );
        } else {
            survivors.push_back(price);
        }
    }

    if survivors.is_empty() {
        return Err(ContractError::AllPriceSourcesRejected);
    }
    Ok(median(&survivors))
}

// ── TWAP opt-in per feed (Issues #815/#816/#817) ─────────────────────────
//
// The accumulator math lives in `crate::twap` (ported from
// `contracts/examples/twap_oracle`). This section owns the production
// wiring: a governance-settable per-feed opt-in flag plus a bounded,
// governance-configurable window. Default is spot (existing behavior) until
// a feed explicitly opts in, so this is not a breaking change.
//
// `fund_invoice`'s payer-verification (`Identity`) path stays boolean spot
// verification — TWAP applies to numeric `Price` reads via
// `get_verified_price`, which branches below. Enabling TWAP on `Identity`
// is stored but has no effect on verification today (documented, not an
// error, so governance can stage configuration in any order).

/// Seconds per ledger on Stellar (~5s), used to convert the ledger-based
/// TWAP window into the timestamp-based window `twap::twap_average` expects.
pub const LEDGER_SECONDS: u64 = 5;

/// Issue #817: minimum TWAP window = 360 ledgers ≈ 30 minutes. Rationale
/// (see `docs/oracle-attack-economics.md` §4–§5 methodology): a sandwich
/// attack manipulates price within a single block/ledger at near-zero
/// on-chain cost, so the window must span *many* ledgers to dilute any one
/// manipulated sample — 30 minutes is the example crate's recommended floor
/// and matches `twap-oracle-recommendations.md`'s minimum.
pub const MIN_TWAP_WINDOW_LEDGERS: u64 = 360;

/// Issue #817: maximum TWAP window = 17_280 ledgers ≈ 24 hours. Rationale:
/// beyond the default `max_oracle_age_ledgers` staleness bound (also
/// 17_280) the average would bake in data the freshness check itself
/// rejects as stale, making staleness worse without adding sandwich
/// resistance. Distinct bound, same order of magnitude, deliberately.
pub const MAX_TWAP_WINDOW_LEDGERS: u64 = 17_280;

/// Default TWAP window = 720 ledgers ≈ 1 hour (the example crate's default
/// `get_price` window), applied until governance sets an explicit value.
pub const DEFAULT_TWAP_WINDOW_LEDGERS: u64 = 720;

/// Whether `feed_type` routes `Price` reads through the TWAP windowed
/// average. Defaults to `false` (raw spot, current behavior).
pub fn is_twap_enabled(env: &Env, feed_type: OracleFeedType) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::TwapEnabled(feed_type))
        .unwrap_or(false)
}

/// The currently configured minimum TWAP observations.
pub fn get_min_twap_observations(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::MinTwapObservations)
        .unwrap_or(2)
}

/// Update the minimum TWAP observations.
///
/// Access: Admin only.
pub fn set_min_twap_observations(env: &Env, min_obs: u32) -> Result<(), ContractError> {
    require_admin(env)?;
    env.storage()
        .instance()
        .set(&DataKey::MinTwapObservations, &min_obs);
    Ok(())
}

/// Enable or disable the TWAP path for `feed_type`.
///
/// Access: Admin only (governance-controlled via the same
/// admin=governance-contract convention used throughout this registry).
pub fn set_twap_enabled(
    env: &Env,
    feed_type: OracleFeedType,
    enabled: bool,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "set_twap_enabled", DEFAULT_RATE_LIMIT_LEDGERS)?;
    env.storage()
        .instance()
        .set(&DataKey::TwapEnabled(feed_type), &enabled);
    env.events().publish(
        (soroban_sdk::Symbol::new(env, "twap_enabled"), feed_type),
        TwapEnabledForFeed { feed_type, enabled },
    );
    Ok(())
}

/// The currently configured TWAP window in ledgers.
pub fn get_twap_window_ledgers(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::TwapWindowLedgers)
        .unwrap_or(DEFAULT_TWAP_WINDOW_LEDGERS)
}

/// Update the TWAP window, rejecting values outside
/// `[MIN_TWAP_WINDOW_LEDGERS, MAX_TWAP_WINDOW_LEDGERS]` with the dedicated
/// `ContractError::InvalidTwapWindow`.
///
/// Access: Admin only.
pub fn set_twap_window_ledgers(env: &Env, window_ledgers: u64) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "set_twap_window", DEFAULT_RATE_LIMIT_LEDGERS)?;
    if window_ledgers < MIN_TWAP_WINDOW_LEDGERS || window_ledgers > MAX_TWAP_WINDOW_LEDGERS {
        return Err(ContractError::InvalidTwapWindow);
    }
    let old_window = get_twap_window_ledgers(env);
    env.storage()
        .instance()
        .set(&DataKey::TwapWindowLedgers, &window_ledgers);
    env.events().publish(
        (soroban_sdk::Symbol::new(env, "twap_window_updated"),),
        TwapWindowUpdated { old_window, new_window: window_ledgers },
    );
    Ok(())
}

/// Record a price observation for `feed_type` + `token` at the current
/// ledger timestamp, for later windowed averaging.
///
/// Access: Admin only (keeper/governance pushes samples; the example
/// crate's `update_price` was likewise admin-gated).
///
/// Deliberately **not rate-limited** (Issue #859 documented exemption):
/// this is a high-frequency keeper operation — the TWAP accumulator only
/// produces a meaningful windowed average if samples land regularly across
/// the window, and clamping it to one call per `DEFAULT_RATE_LIMIT_LEDGERS`
/// (120 ledgers ≈ 10min) would leave the window sparsely sampled and easily
/// skewed by a single manipulated sample. The sampling cadence is the
/// protection here; gating it would weaken, not strengthen, the TWAP path.
pub fn record_twap_sample(
    env: &Env,
    feed_type: OracleFeedType,
    token: Address,
    price: i128,
) -> Result<(), ContractError> {
    require_admin(env)?;
    if price <= 0 {
        return Err(ContractError::InvalidAmount);
    }
    let key = DataKey::TwapSamples(feed_type, token);
    let mut samples: soroban_sdk::Vec<crate::twap::TwapSample> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));
    crate::twap::push_sample(
        &mut samples,
        crate::twap::TwapSample {
            timestamp: env.ledger().timestamp(),
            price,
        },
        crate::twap::MAX_TWAP_SAMPLES,
    );
    env.storage().persistent().set(&key, &samples);
    Ok(())
}

/// Windowed TWAP average for `feed_type` + `token` over the configured
/// window, or `None` when fewer than two in-window samples exist.
///
/// Issue #860: `None` is also returned when the feed's circuit breaker is
/// open or its last recorded health snapshot is stale — a windowed average
/// over data the registry has already observed to be bad is not a price
/// this function should hand back (its `Option` signature predates the
/// health gate, so the error collapses to "no opinion" here; callers that
/// need to distinguish the reasons use `get_verified_price`, which maps
/// them to `OracleCircuitOpen` / `OracleDataStale`).
pub fn get_twap_price(env: &Env, feed_type: OracleFeedType, token: &Address) -> Option<i128> {
    require_healthy_price_feed(env, feed_type, token).ok()?;
    let samples: soroban_sdk::Vec<crate::twap::TwapSample> = env
        .storage()
        .persistent()
        .get(&DataKey::TwapSamples(feed_type, token.clone()))
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));
    let min_obs = get_min_twap_observations(env);
    if samples.len() < min_obs {
        env.events().publish(
            (soroban_sdk::Symbol::new(env, "twap_insufficient_data"), feed_type),
            TwapInsufficientData {
                feed_type,
                token: token.clone(),
                observations: samples.len(),
                min_required: min_obs,
            },
        );
        return None;
    }
    let window_ledgers = get_twap_window_ledgers(env);
    let window_seconds = window_ledgers.saturating_mul(LEDGER_SECONDS);
    let current_time = env.ledger().timestamp();
    let window_start = current_time.saturating_sub(window_seconds);
    crate::twap::twap_average(&samples, window_start, current_time)
}

/// TWAP-aware price read used by numeric `Price` consumers.
///
/// - TWAP disabled (default): behaves exactly as before (spot median with
///   outlier rejection).
/// - TWAP enabled: returns the windowed average when at least two
///   in-window samples exist; otherwise falls back to the spot path so a
///   freshly-enabled feed stays live while keepers backfill samples. The
///   fallback is documented, not silent — callers can distinguish it via
///   `get_twap_price` returning `None`.
pub fn get_twap_aware_price(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
) -> Result<i128, ContractError> {
    if is_twap_enabled(&env, feed_type) {
        // Surface the specific health-gate error first, then treat any
        // remaining `None` as too few observations (no silent spot fallback).
        require_healthy_price_feed(&env, feed_type, &token)?;
        return get_twap_price(&env, feed_type, &token)
            .ok_or(ContractError::InsufficientTwapObservations);
    }
    get_verified_price(env, feed_type, token)
}
