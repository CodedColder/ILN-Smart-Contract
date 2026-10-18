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
use crate::constants::{
    DEFAULT_RATE_LIMIT_LEDGERS, MAX_IDENTITY_ORACLES_PER_FEED, MAX_PRICE_SOURCES_PER_FEED,
};
use crate::errors::ContractError;
use crate::events::{
    IdentityOracleAdded, IdentityOracleRemoved, OracleCircuitRecovered, OracleCircuitReset,
    OracleCircuitTripped, OracleDataStaleRejected, OracleFeederDisagreement, OracleHealthRecorded,
    OracleRegistered, OracleUnregistered, PriceOutlierRejected, PriceSourceAdded,
    PriceSourceRemoved, TwapEnabledForFeed, TwapInsufficientData, TwapWindowUpdated,
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

/// Issue #1053 — maximum number of *consecutive* duplicate oracle updates
/// (the same oracle re-serving an identical `data_timestamp`, i.e. no
/// advancement) that oracle-gated funding will accept before rejecting with
/// `OracleDuplicateUpdate`.
///
/// Bounds duplicate update frequency: an oracle that has stopped advancing
/// would otherwise keep producing non-stale responses (a re-served timestamp
/// stays within the freshness window) and keep resetting the consecutive
/// stale streak, so the circuit breaker never trips and funding stays open on
/// data that is no longer live. Mirrors [`MAX_CONSECUTIVE_STALE_QUERIES`].
pub const MAX_CONSECUTIVE_DUPLICATE_UPDATES: u32 = 3;

/// Issue #1048 — number of consecutive *healthy* (fresh, timestamp-advancing)
/// observations the oracle that tripped the circuit breaker must serve before
/// the breaker auto-recovers (half-open → closed). Mirrors
/// [`MAX_CONSECUTIVE_STALE_QUERIES`]: recovery takes at least as many healthy
/// observations as the trip took stale ones. A single fresh query is never
/// enough (see [`maybe_recover_oracle_circuit`]).
pub const CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS: u32 = 3;

/// Issue #1048 — minimum number of ledgers that must elapse between the first
/// healthy observation of a recovery streak and the auto-close. Together with
/// [`CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS`] this makes recovery a *sustained*
/// property (a proven-stable oracle over a real time window) rather than a
/// single lucky poll, so a flapping oracle can't recover within one tick.
pub const CIRCUIT_RECOVERY_MIN_LEDGERS: u64 = 120;

/// Issue #1048: half-open recovery bookkeeping for a tripped circuit breaker.
/// Records which oracle tripped the breaker (only *that* oracle may auto-recover
/// it) and the progress of the current healthy-observation streak.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CircuitRecoveryState {
    /// The oracle whose staleness tripped the breaker. A *different* (even
    /// healthy) oracle cannot auto-recover this breaker — see
    /// [`maybe_recover_oracle_circuit`].
    pub tripped_oracle: Address,
    /// Consecutive healthy observations served by `tripped_oracle` since the
    /// streak began. Reset to 0 by any unhealthy observation.
    pub consecutive_healthy: u32,
    /// Ledger of the first healthy observation of the current streak
    /// (`0` = no streak in progress).
    pub first_healthy_ledger: u64,
}

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
    /// Issue #1053: number of consecutive queries where the *same* oracle
    /// re-served an identical `last_data_timestamp` — a duplicate update with
    /// no advancement. Resets to 0 the moment the timestamp advances or
    /// resolution falls back to a different oracle. Bounding this is what
    /// stops a "frozen" oracle from standing in for live verification.
    pub consecutive_duplicate_count: u32,
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

/// Whether oracle data timestamped `data_timestamp` (a ledger sequence) is
/// stale relative to `current_ledger`, given `max_age_ledgers`.
///
/// Staleness is `age >= max_age_ledgers`, where `age = current_ledger -
/// data_timestamp`; `max_age_ledgers == 0` disables the check entirely (the
/// governance escape hatch).
///
/// Issue #1050 — a `data_timestamp` *ahead* of `current_ledger` is treated as
/// stale, not as age 0. Plain saturating subtraction maps every future
/// timestamp onto the freshest possible reading, so an oracle (or a buggy one)
/// reporting an arbitrarily large timestamp would defeat the freshness bound
/// on every subsequent ledger. Rejecting the future region makes the bound hold
/// across the full ledger range, including the boundary where the data
/// timestamp first overtakes the current ledger sequence.
///
/// This is the single source of truth for the ledger-based freshness rule; both
/// `fund_invoice`'s gate and `record_oracle_health` call it so the funding
/// decision and the recorded health snapshot can never disagree.
pub fn is_oracle_data_stale(
    current_ledger: u32,
    data_timestamp: u32,
    max_age_ledgers: u64,
) -> bool {
    if max_age_ledgers == 0 {
        return false;
    }
    if data_timestamp > current_ledger {
        return true;
    }
    (current_ledger as u64).saturating_sub(data_timestamp as u64) >= max_age_ledgers
}

/// Whether `data_timestamp` *regresses* relative to the most recently recorded
/// health snapshot for the same `oracle` on this `feed_type` + `token`.
///
/// Issue #1052 — an oracle's reported data timestamp must be monotonically
/// non-decreasing. `record_oracle_health` persists the last timestamp seen per
/// (feed type, token); a later response from the *same* oracle carrying a
/// **smaller** timestamp is a regression — replayed or rolled-back data — and
/// the funding gate rejects it with `OracleTimestampRegression`. Together with
/// [`is_oracle_data_stale`] (which rejects *future*-dated data), an accepted
/// response must therefore lie in `[last_recorded, current_ledger]`.
///
/// Equal timestamps are allowed (non-decreasing, not strictly increasing): a
/// re-read of the same data is not a regression.
///
/// A regression is only meaningful against the *same* oracle's timeline, so a
/// snapshot recorded for a different oracle address is ignored (resolution may
/// have fallen back to another oracle). Returns `false` when no snapshot exists
/// yet — there is no baseline to regress from.
///
/// Note: the funding gate records health only on success, and a rejected call
/// rolls its writes back, so a regression observed through funding never lowers
/// the baseline. A regression observed through the never-erroring
/// `check_oracle_health` is recorded as-is for monitoring.
pub fn is_oracle_data_timestamp_regression(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
    oracle: &Address,
    data_timestamp: u32,
) -> bool {
    let key = DataKey::OracleHealth(feed_type, token.clone());
    match env
        .storage()
        .persistent()
        .get::<DataKey, OracleHealthStatus>(&key)
    {
        Some(prev) => prev.oracle == *oracle && data_timestamp < prev.last_data_timestamp,
        None => false,
    }
}

/// Whether `data_timestamp` is a *duplicate* oracle update that pushes the
/// consecutive duplicate streak past [`MAX_CONSECUTIVE_DUPLICATE_UPDATES`] —
/// i.e. the same oracle has already re-served this identical timestamp enough
/// times in a row that the funding gate should reject the next one.
///
/// Issue #1053 — [`is_oracle_data_timestamp_regression`] forbids a timestamp
/// going *backwards* but deliberately allows an *equal* one (non-decreasing).
/// Nothing there stops an oracle from standing still: an oracle that stops
/// advancing and re-serves the same value would keep producing non-stale
/// responses (within the freshness window) and keep resetting the
/// consecutive-stale streak, so the circuit breaker never trips and funding
/// stays open on data that is no longer live. This predicate bounds that.
///
/// Meaningful only against the *same* oracle (resolution may fail over to a
/// different oracle, whose own timeline starts clean) — a snapshot recorded
/// for another address is ignored. Returns `false` when no snapshot exists.
///
/// Same rollback property as the regression check: a rejected `fund_invoice`
/// rolls its health write back, so a rejected duplicate never advances the
/// streak — the gate stays shut until the oracle actually advances its
/// timestamp (or governance changes the resolved oracle). A duplicate
/// observed through the never-erroring `check_oracle_health` is recorded
/// as-is for monitoring.
pub fn is_oracle_data_duplicate_update(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
    oracle: &Address,
    data_timestamp: u32,
) -> bool {
    let key = DataKey::OracleHealth(feed_type, token.clone());
    match env
        .storage()
        .persistent()
        .get::<DataKey, OracleHealthStatus>(&key)
    {
        Some(prev) => {
            prev.oracle == *oracle
                && data_timestamp == prev.last_data_timestamp
                && prev.consecutive_duplicate_count >= MAX_CONSECUTIVE_DUPLICATE_UPDATES
        }
        None => false,
    }
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
    // Note: `age` is the saturating distance (0 for future-dated data), while
    // `is_stale` treats the future region as stale — see `is_oracle_data_stale`.
    let is_stale = is_oracle_data_stale(current_ledger, data_timestamp, max_age_ledgers);

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

    // Issue #1053: consecutive duplicate (identical-timestamp) update streak
    // for the same oracle — bounds duplicate update frequency. A duplicate is
    // an *unchanged* `data_timestamp` from the same oracle; the first read
    // (no snapshot) and any advance/fallback reset the streak to 0.
    let consecutive_duplicate_count = match &previous {
        Some(prev) if prev.oracle == *oracle && data_timestamp == prev.last_data_timestamp => {
            prev.consecutive_duplicate_count.saturating_add(1)
        }
        _ => 0,
    };

    let status = OracleHealthStatus {
        oracle: oracle.clone(),
        last_checked_ledger: current_ledger,
        last_data_timestamp: data_timestamp,
        last_data_age_ledgers: age,
        is_stale,
        consecutive_stale_count,
        consecutive_duplicate_count,
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
            consecutive_duplicate_count,
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
        // Issue #1048: start half-open recovery bookkeeping, pinning the
        // breaker to the oracle that just tripped it so that only *that*
        // oracle's later sustained health can auto-recover it.
        env.storage().persistent().set(
            &DataKey::OracleCircuitRecovery(feed_type, token.clone()),
            &CircuitRecoveryState {
                tripped_oracle: oracle.clone(),
                consecutive_healthy: 0,
                first_healthy_ledger: 0,
            },
        );
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

    // Issue #1048: if the breaker is currently tripped, let a sustained run of
    // healthy observations from the tripping oracle auto-recover it. No-op
    // (and no storage cost) when the breaker is closed.
    maybe_recover_oracle_circuit(
        env,
        feed_type,
        token,
        oracle,
        is_stale,
        data_timestamp,
        &previous,
        current_ledger,
    );
}

/// Issue #1048 — half-open auto-recovery for a tripped circuit breaker.
///
/// Called from [`record_oracle_health`] after each observation. When the
/// breaker for `feed_type` + `token` is tripped, this advances a recovery
/// streak whenever the *tripping* oracle serves a **healthy** response —
/// defined as fresh (`!is_stale`) **and** timestamp-advancing since that
/// oracle's previous observation (so a frozen/replayed value never counts).
/// Any unhealthy observation resets the streak to 0. Once the streak reaches
/// [`CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS`] and spans at least
/// [`CIRCUIT_RECOVERY_MIN_LEDGERS`] ledgers, the breaker is cleared and
/// `oracle_circuit_recovered` is emitted.
///
/// Safety properties (preserving the anti-flapping design):
///   * A **single** fresh query can never recover — the streak and the ledger
///     window both have to be satisfied.
///   * Only the oracle that *caused* the trip can recover it; a different
///     (even healthy) oracle observed in the meantime does not clear this
///     breaker, so the previously-tripped oracle isn't silently re-trusted.
///   * A missing recovery record (e.g. a breaker tripped before this feature
///     existed) fails safe: no auto-recovery, the admin `reset_oracle_circuit`
///     is still required.
///
/// Deliberately does **not** run from `fund_invoice`'s gated path: while the
/// breaker is open that path rejects before querying the oracle, so recovery
/// is driven by the permissionless, never-erroring `check_oracle_health`
/// (keepers/monitors), whose writes commit.
#[allow(clippy::too_many_arguments)]
fn maybe_recover_oracle_circuit(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
    oracle: &Address,
    is_stale: bool,
    data_timestamp: u32,
    previous: &Option<OracleHealthStatus>,
    current_ledger: u32,
) {
    if !is_oracle_circuit_tripped(env, feed_type, token.clone()) {
        return;
    }
    let recovery_key = DataKey::OracleCircuitRecovery(feed_type, token.clone());
    let mut state: CircuitRecoveryState = match env.storage().persistent().get(&recovery_key) {
        Some(s) => s,
        // Fail safe: no record of who tripped → no auto-recovery.
        None => return,
    };
    // Only the oracle that tripped the breaker can recover it.
    if state.tripped_oracle != *oracle {
        return;
    }

    // Healthy = fresh AND the oracle actually advanced its timestamp since its
    // own previous observation (a re-served value is not evidence of recovery).
    let advanced = match previous {
        Some(prev) if prev.oracle == *oracle => data_timestamp > prev.last_data_timestamp,
        _ => true,
    };
    let healthy = !is_stale && advanced;

    if !healthy {
        if state.consecutive_healthy != 0 || state.first_healthy_ledger != 0 {
            state.consecutive_healthy = 0;
            state.first_healthy_ledger = 0;
            env.storage().persistent().set(&recovery_key, &state);
        }
        return;
    }

    if state.consecutive_healthy == 0 {
        state.first_healthy_ledger = current_ledger as u64;
    }
    state.consecutive_healthy = state.consecutive_healthy.saturating_add(1);
    let elapsed = (current_ledger as u64).saturating_sub(state.first_healthy_ledger);

    if state.consecutive_healthy >= CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS
        && elapsed >= CIRCUIT_RECOVERY_MIN_LEDGERS
    {
        env.storage().persistent().remove(&recovery_key);
        env.storage()
            .persistent()
            .remove(&DataKey::OracleCircuitTripped(feed_type, token.clone()));
        env.events().publish(
            (
                soroban_sdk::Symbol::new(env, "oracle_circuit_recovered"),
                feed_type,
            ),
            OracleCircuitRecovered {
                feed_type,
                token: token.clone(),
                healthy_observations: state.consecutive_healthy,
                elapsed_ledgers: elapsed,
            },
        );
        return;
    }

    env.storage().persistent().set(&recovery_key, &state);
}

/// Whether the oracle circuit breaker for `feed_type` + `token` is
/// currently tripped. Cleared either by the governance-gated
/// `reset_oracle_circuit` fast path or by the Issue #1048 half-open
/// auto-recovery (a sustained run of healthy observations from the tripping
/// oracle) — never by a single fresh query, per the anti-flapping requirement.
pub fn is_oracle_circuit_tripped(env: &Env, feed_type: OracleFeedType, token: &Address) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::OracleCircuitTripped(feed_type, token.clone()))
        .unwrap_or(false)
}

/// Issue #1048: the current half-open recovery progress for a tripped circuit
/// breaker at `feed_type` + `token`, or `None` when nothing is recovering
/// (breaker closed, or an already-tripped breaker with no recovery record).
/// Read-only view for operations/monitoring so a stuck recovery is observable.
pub fn get_circuit_recovery_state(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
) -> Option<CircuitRecoveryState> {
    env.storage()
        .persistent()
        .get(&DataKey::OracleCircuitRecovery(feed_type, token))
}

/// Governance-gated reset of a tripped oracle circuit breaker. Requires an
/// explicit call and is the fast path; since Issue #1048 a breaker also
/// auto-recovers after the tripping oracle demonstrates a sustained run of
/// healthy observations (see [`maybe_recover_oracle_circuit`]). This clears
/// both the tripped flag and any in-progress recovery bookkeeping, so a
/// flapping oracle can't quietly resume being trusted without either this
/// affirmative call or a proven recovery window.
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
    // Issue #1048: drop any half-open recovery progress too, so a later
    // re-trip starts from a clean streak.
    env.storage()
        .persistent()
        .remove(&DataKey::OracleCircuitRecovery(feed_type, token.clone()));
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
/// Issue #1064: the per-feed source list is capped at
/// `MAX_PRICE_SOURCES_PER_FEED` — a genuinely new source is rejected with
/// `TooManyPriceSources` once the feed is full. This bounds the number of
/// `PriceOutlierRejected` events `get_verified_price` can emit in a single
/// invocation (one per deviating source), which was previously unbounded
/// because the list itself was.
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
        // Issue #1064: bound the per-feed source list. Only a genuinely new
        // source can push the list past the cap — re-registering an existing
        // one stays a no-op so idempotency is preserved.
        if sources.len() >= MAX_PRICE_SOURCES_PER_FEED {
            return Err(ContractError::TooManyPriceSources);
        }
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

// ── Issue #1047: multi-oracle Identity quorum ─────────────────────────────
//
// Before this, `fund_invoice`'s payer-verification gate resolved a *single*
// `Identity` oracle through the priority chain and trusted its boolean
// outright. A single compromised or mistaken feeder could therefore mint
// funding eligibility on its own, and there was no place for a second feeder
// to be consulted — or for disagreement between feeders to be observed at all.
//
// This adds an opt-in, governance-controlled set of Identity oracles per feed
// type. When the set is non-empty, `fund_invoice` queries *all* of them and
// requires at least `get_identity_quorum()` of them to agree the payer is
// verified (with fresh data) before funding proceeds; if they disagree — fewer
// than the configured quorum agree, or too few respond fresh — the call fails
// closed as `PayerUnverified` and an `OracleFeederDisagreement` event is
// emitted. An empty set preserves the prior single-oracle behavior exactly.
//
// The `OracleInterface` contract is deliberately unchanged: each feeder is an
// independent deployment of the same `get_payer_data` interface, so no ABI or
// interface-version bump is required.

/// Default number of registered Identity oracles that must agree before
/// oracle-gated funding proceeds, when governance has not set an explicit
/// quorum via `set_identity_quorum`.
///
/// `1` keeps the multi-oracle list a strict superset of the prior single-oracle
/// behavior: an operator who registers feeders but never widens the threshold
/// still only needs one agreement. Operators relying on the list for
/// disagreement safety should call `set_identity_quorum` with the agreement
/// level they actually require (e.g. a majority of the registered feeders).
pub const DEFAULT_IDENTITY_QUORUM: u32 = 1;

/// Add `oracle` to `feed_type`'s Identity-oracle quorum list, consulted by
/// `fund_invoice`'s oracle-gated path together with the other registered
/// Identity oracles.
///
/// Idempotent: re-adding an already-registered oracle is a no-op (so the
/// per-feed cap can't be reached by re-registering the same address). The
/// list is bounded by [`MAX_IDENTITY_ORACLES_PER_FEED`]; a genuinely new oracle
/// once the list is full is rejected with `TooManyIdentityOracles`.
///
/// Unlike `register_oracle`, this performs no interface-version handshake —
/// feeders are queried dynamically via `try_invoke_contract` at funding time
/// and a non-responding/incompatible feeder simply fails to contribute an
/// agreement (it can never *force* verification).
///
/// Access: Admin only (governance-controlled via the same
/// admin=governance-contract convention used throughout this registry).
pub fn add_identity_oracle(
    env: &Env,
    feed_type: OracleFeedType,
    oracle: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "add_identity_oracle", DEFAULT_RATE_LIMIT_LEDGERS)?;
    let key = DataKey::IdentityOracles(feed_type);
    let mut oracles: soroban_sdk::Vec<Address> = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));
    if !oracles.iter().any(|existing| existing == oracle) {
        // Issue #1047: bound the per-feed Identity-oracle list. Only a
        // genuinely new oracle can push the list past the cap — re-adding an
        // existing one stays a no-op so idempotency is preserved.
        if oracles.len() >= MAX_IDENTITY_ORACLES_PER_FEED {
            return Err(ContractError::TooManyIdentityOracles);
        }
        oracles.push_back(oracle.clone());
        env.storage().instance().set(&key, &oracles);
    }
    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "identity_oracle_added"),
            feed_type,
        ),
        IdentityOracleAdded { feed_type, oracle },
    );
    Ok(())
}

/// Remove `oracle` from `feed_type`'s Identity-oracle quorum list. A no-op if
/// it wasn't registered.
///
/// Access: Admin only.
pub fn remove_identity_oracle(
    env: &Env,
    feed_type: OracleFeedType,
    oracle: Address,
) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "remove_identity_oracle", DEFAULT_RATE_LIMIT_LEDGERS)?;
    let key = DataKey::IdentityOracles(feed_type);
    let oracles: soroban_sdk::Vec<Address> = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));
    let mut remaining: soroban_sdk::Vec<Address> = soroban_sdk::Vec::new(env);
    for existing in oracles.iter() {
        if existing != oracle {
            remaining.push_back(existing);
        }
    }
    env.storage().instance().set(&key, &remaining);
    env.events().publish(
        (
            soroban_sdk::Symbol::new(env, "identity_oracle_removed"),
            feed_type,
        ),
        IdentityOracleRemoved { feed_type, oracle },
    );
    Ok(())
}

/// The currently registered Identity oracles for `feed_type` (empty if none).
pub fn get_identity_oracles(env: Env, feed_type: OracleFeedType) -> soroban_sdk::Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::IdentityOracles(feed_type))
        .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
}

/// Set the number of registered Identity oracles that must agree the payer is
/// verified before oracle-gated funding proceeds (the `N` in `N`-of-`M`).
///
/// Rejects `0` (a threshold no agreement can ever reach meaningfully) and any
/// value above [`MAX_IDENTITY_ORACLES_PER_FEED`] (more agreement than the list
/// can ever hold) with `InvalidIdentityQuorum`.
///
/// Access: Admin only.
pub fn set_identity_quorum(env: &Env, quorum: u32) -> Result<(), ContractError> {
    require_admin(env)?;
    check_rate_limit(env, "set_identity_quorum", DEFAULT_RATE_LIMIT_LEDGERS)?;
    if quorum == 0 || quorum > MAX_IDENTITY_ORACLES_PER_FEED {
        return Err(ContractError::InvalidIdentityQuorum);
    }
    env.storage()
        .instance()
        .set(&DataKey::IdentityQuorum, &quorum);
    Ok(())
}

/// The currently configured Identity verification quorum. Defaults to
/// [`DEFAULT_IDENTITY_QUORUM`] until governance sets one.
pub fn get_identity_quorum(env: Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::IdentityQuorum)
        .unwrap_or(DEFAULT_IDENTITY_QUORUM)
}

/// Outcome of [`check_identity_quorum`], used by `fund_invoice`'s
/// oracle-gated payer-verification path.
#[derive(Debug, PartialEq, Eq)]
pub enum IdentityQuorumOutcome {
    /// No Identity oracle is registered in the quorum list — the caller must
    /// fall back to the pre-#1047 single-oracle resolution chain, preserving
    /// existing behavior.
    NotConfigured,
    /// At least `get_identity_quorum()` registered oracles agreed the payer is
    /// verified (with fresh data). Funding may proceed.
    Verified,
    /// The list is non-empty but fewer than the configured quorum agreed (the
    /// feeders disagreed, or too few responded fresh + verified). Funding fails
    /// closed.
    Disagreement,
}

/// Query every registered Identity oracle for `feed_type` and require at least
/// `get_identity_quorum()` of them to agree `payer` is verified before funding
/// proceeds.
///
/// Issue #1047 — the safety property is *disagreement fails closed*: an oracle
/// that reverts, returns unverified, or returns stale data simply does not
/// contribute an agreement, so it can never force verification through. Only a
/// response that is both `is_verified` and fresh (per the shared
/// [`is_oracle_data_stale`] rule, so this gate and the recorded health snapshot
/// use the same notion of freshness) counts toward the quorum. If fewer than
/// the configured threshold agree, `Disagreement` is returned and an
/// `OracleFeederDisagreement` event is emitted; the caller maps this to
/// `PayerUnverified`.
///
/// Returns [`IdentityQuorumOutcome::NotConfigured`] when no oracle is
/// registered, so the caller runs the unchanged single-oracle path.
///
/// Note: this path deliberately does not write the single-oracle
/// `OracleHealth` snapshot (that key models one oracle per feed + token and is
/// owned by the single-oracle path); disagreement is surfaced through the
/// dedicated event instead.
pub fn check_identity_quorum(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
    payer: &Address,
    max_age_ledgers: u64,
) -> IdentityQuorumOutcome {
    let oracles = get_identity_oracles(env.clone(), feed_type);
    if oracles.is_empty() {
        return IdentityQuorumOutcome::NotConfigured;
    }

    let required = get_identity_quorum(env.clone());
    let current_ledger = env.ledger().sequence();
    let mut verified_count: u32 = 0;

    for oracle in oracles.iter() {
        // Issue #1055: the per-feeder predicate lives in one shared helper so
        // the gate below and the `check_identity_quorum_health` diagnostic can
        // never disagree about *why* a feeder did or didn't count.
        if classify_identity_feeder(env, &oracle, payer, current_ledger, max_age_ledgers)
            == FeederHealth::Verified
        {
            verified_count += 1;
        }
    }

    if verified_count >= required {
        IdentityQuorumOutcome::Verified
    } else {
        env.events().publish(
            (
                soroban_sdk::Symbol::new(env, "oracle_feeder_disagreement"),
                feed_type,
            ),
            OracleFeederDisagreement {
                feed_type,
                token: token.clone(),
                verified_count,
                required_quorum: required,
                total_oracles: oracles.len(),
            },
        );
        IdentityQuorumOutcome::Disagreement
    }
}

/// Issue #1049: precise outcome of the Identity payer-verification gate.
///
/// This is the single source of truth for "would oracle-gated funding accept
/// this payer?" — computed by [`evaluate_identity_verification`] and consumed
/// two ways:
///
///   * `fund_invoice` maps it onto the exact `ContractError` it already
///     returned (so funding behavior is unchanged), and
///   * the permissionless, never-erroring `check_oracle_verification`
///     entrypoint returns it directly to off-chain monitors, so the *reason* a
///     gate would reject is observable even though `fund_invoice`'s own
///     `Err` return rolls its events back.
///
/// A `#[contracttype]` enum so the diagnostic entrypoint can return it
/// on-chain (mirrors `OracleResolution` / `IdentityQuorumOutcome`, which are
/// internal-only and therefore plain Rust enums).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum IdentityVerificationOutcome {
    /// The gate passes: the payer is verified with fresh data (single oracle or
    /// quorum), or nothing is configured — the legacy fail-open path.
    Verified,
    /// Data was stale → `oracle_stale_data_rejected` emitted (Issue #1049).
    /// `fund_invoice` maps this to `ContractError::OracleDataStale`.
    Stale,
    /// Registered Identity feeders disagreed (fewer than the quorum agreed) →
    /// `oracle_feeder_disagreement` emitted (Issue #1047). `fund_invoice` maps
    /// this to `ContractError::PayerUnverified`.
    QuorumDisagreement,
    /// The oracle's data timestamp regressed (Issue #1052).
    TimestampRegression,
    /// The oracle re-served an identical timestamp past the duplicate cap
    /// (Issue #1053).
    DuplicateUpdate,
    /// Every candidate oracle is circuit-tripped.
    CircuitOpen,
    /// The resolved oracle responded `is_verified = false`.
    Unverified,
}

/// Issue #1049: evaluate the Identity payer-verification gate for `token` +
/// `payer` and report the precise outcome, performing the same side effects the
/// funding path does (monotonicity/duplicate checks, `record_oracle_health`) and
/// emitting the **failure events** `oracle_stale_data_rejected` and
/// `oracle_feeder_disagreement`.
///
/// This is the shared implementation behind both `fund_invoice`'s
/// `require_oracle_verification` block and the permissionless
/// `check_oracle_verification` diagnostic. Splitting it out is what makes the
/// failure events *observable*: `fund_invoice` returns `Err` on rejection, and
/// Soroban discards every event of a call that returns `Err`, so the failures it
/// detects would otherwise never reach a ledger. The diagnostic calls this same
/// helper but returns `Ok`, so its events — and its `OracleHealth` write — commit.
///
/// Order of checks is preserved exactly from the funding path: quorum first
/// (a registered list short-circuits the single-oracle chain), then circuit
/// status, then monotonicity, then duplicate, then record health, then
/// freshness, then the verification boolean. This keeps the error precedence
/// (`OracleCircuitOpen` before `OracleTimestampRegression` before
/// `OracleDuplicateUpdate` before `OracleDataStale` before `PayerUnverified`)
/// identical for `fund_invoice`.
///
/// Never returns `Err`: a feeder/oracle that reverts in the quorum path
/// degrades to "no agreement" via `try_invoke_contract` (as in
/// [`check_identity_quorum`]); the single-oracle path's `invoke_contract` is
/// left to propagate exactly as `fund_invoice` already allowed.
pub fn evaluate_identity_verification(
    env: &Env,
    feed_type: OracleFeedType,
    token: &Address,
    payer: &Address,
    max_age_ledgers: u64,
) -> IdentityVerificationOutcome {
    // Multi-oracle quorum path (Issue #1047): a non-empty registered list is
    // authoritative and short-circuits the single-oracle chain below.
    match check_identity_quorum(env, feed_type, token, payer, max_age_ledgers) {
        IdentityQuorumOutcome::Disagreement => return IdentityVerificationOutcome::QuorumDisagreement,
        IdentityQuorumOutcome::Verified => return IdentityVerificationOutcome::Verified,
        IdentityQuorumOutcome::NotConfigured => {}
    }

    let resolution = resolve_oracle_for_verification(env, feed_type, token);
    if resolution == OracleResolution::CircuitOpen {
        return IdentityVerificationOutcome::CircuitOpen;
    }
    let oracle_addr = match resolution {
        OracleResolution::Available(addr) => addr,
        // Unconfigured: legacy fail-open — no oracle to consult.
        _ => return IdentityVerificationOutcome::Verified,
    };

    let response: OracleVerificationResponse = env.invoke_contract(
        &oracle_addr,
        &Symbol::new(env, "get_payer_data"),
        vec![env, payer.clone().into_val(env)],
    );

    // Issue #1052: monotonicity — a data timestamp must never go *backwards*
    // for the same oracle. Read the previous snapshot *before*
    // `record_oracle_health` overwrites it below.
    if is_oracle_data_timestamp_regression(env, feed_type, token, &oracle_addr, response.timestamp) {
        return IdentityVerificationOutcome::TimestampRegression;
    }

    // Issue #1053: bound duplicate update frequency (a frozen oracle that keeps
    // re-serving the same value). Same ordering rationale as above.
    if is_oracle_data_duplicate_update(env, feed_type, token, &oracle_addr, response.timestamp) {
        return IdentityVerificationOutcome::DuplicateUpdate;
    }

    // Issue #532: record health (staleness) regardless of outcome.
    record_oracle_health(
        env,
        feed_type,
        token,
        &oracle_addr,
        response.timestamp,
        max_age_ledgers,
    );

    let current_ledger = env.ledger().sequence();

    // Issue #93 / #1050: reject stale data via the shared freshness rule.
    if is_oracle_data_stale(current_ledger, response.timestamp, max_age_ledgers) {
        let age_ledgers = (current_ledger as u64).saturating_sub(response.timestamp as u64);
        env.events().publish(
            (
                soroban_sdk::Symbol::new(env, "oracle_stale_data_rejected"),
                feed_type,
            ),
            OracleDataStaleRejected {
                feed_type,
                token: token.clone(),
                oracle: oracle_addr,
                data_timestamp: response.timestamp,
                age_ledgers,
                max_age_ledgers,
            },
        );
        return IdentityVerificationOutcome::Stale;
    }

    // Issue #92: reject unverified payers.
    if !response.is_verified {
        return IdentityVerificationOutcome::Unverified;
    }

    IdentityVerificationOutcome::Verified
}

/// Issue #1049: the permissionless, **never-erroring** diagnostic counterpart
/// to `fund_invoice`'s Identity gate. Evaluates the gate for `token` + `payer`
/// at the current max-oracle-age and returns the [`IdentityVerificationOutcome`]
/// — but, crucially, its writes and its failure events commit (it returns `Ok`),
/// so off-chain monitors/keepers can observe *why* oracle-gated funding would be
/// rejected (stale data, feeder disagreement, a regressed/duplicate timestamp,
/// an open circuit, or an unverified payer) even though a rejected
/// `fund_invoice` leaves no on-chain trace.
///
/// Mirrors [`check_oracle_health`]: a read-only observer entrypoint with no
/// authorization and no successful-path side effect beyond the health snapshot.
pub fn check_oracle_verification(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
    payer: Address,
) -> IdentityVerificationOutcome {
    let max_age = crate::storage::get_config(&env)
        .map(|c| c.max_oracle_age_ledgers)
        .unwrap_or(crate::DEFAULT_MAX_ORACLE_AGE_LEDGERS);
    evaluate_identity_verification(&env, feed_type, &token, &payer, max_age)
}

// ── Issue #1055: exposing oracle health & quorum ────────────────────────────
//
// Before: the answer to "is oracle gating for this feed + token healthy?" was
// spread across five separate read entrypoints — `get_oracle_health`,
// `is_oracle_circuit_tripped`, `get_circuit_recovery_state`,
// `get_identity_oracles`, `get_identity_quorum` — so an off-chain `/health`
// probe (the game-day exercise's action item) had to fan out several calls and
// know which five to ask for. Worse, the *live* health of a multi-oracle
// Identity quorum had no reader at all: `check_identity_quorum` deliberately
// never writes `OracleHealth` (that key models a single oracle per feed +
// token), so in quorum mode `get_oracle_health` reports `None` forever and a
// silently degraded quorum was indistinguishable from an unqueried one.
//
// After: two additive, permissionless, never-erroring views —
//   * `get_oracle_status` — one cheap call (no cross-contract calls, no
//     writes) returning the whole recorded health + configuration state, and
//   * `check_identity_quorum_health` — the missing live per-feeder health +
//     quorum outcome, sharing `classify_identity_feeder` with the funding gate
//     so the two can never disagree.
//
// Neither changes any existing entrypoint, storage key, or event, so funding
// behaviour and every existing integration are untouched.

/// Issue #1055: per-feeder health classification observed by
/// [`check_identity_quorum_health`].
///
/// Mirrors exactly the predicate the funding gate applies — `check_identity_quorum`
/// counts a feeder only when it classifies as [`FeederHealth::Verified`] — so
/// the diagnostic can never disagree with the gate about which feeders agreed.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeederHealth {
    /// Fresh data **and** `is_verified = true` — counts toward the quorum.
    Verified,
    /// Fresh data, `is_verified = false` — responded, but disagrees.
    Unverified,
    /// Responded, but the data is stale (or future-dated) — cannot agree.
    Stale,
    /// Reverted, trapped, or returned an undecodable payload — "no opinion".
    Unreachable,
}

/// Issue #1055: live health of the registered Identity quorum for a feed +
/// token — one entry per registered feeder, plus the resulting quorum verdict.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct IdentityQuorumHealth {
    pub feed_type: OracleFeedType,
    pub token: Address,
    /// Registered feeders, in registration order. Empty means the multi-oracle
    /// quorum is **not in use** and the single-oracle resolution chain governs
    /// (mirroring [`IdentityQuorumOutcome::NotConfigured`]).
    pub feeders: soroban_sdk::Vec<Address>,
    /// Per-feeder observation, index-aligned with `feeders`.
    pub observations: soroban_sdk::Vec<FeederHealth>,
    /// Feeders that agreed (classified [`FeederHealth::Verified`]).
    pub verified_count: u32,
    /// Configured threshold — the `N` of `N`-of-`M`.
    pub required_quorum: u32,
    /// `verified_count >= required_quorum`. Always `false` when `feeders` is
    /// empty (no quorum is in use), so a caller can treat this as "the quorum
    /// is active *and* currently satisfied".
    pub quorum_met: bool,
}

/// Issue #1055: one-call, permissionless snapshot of a feed + token's oracle
/// configuration **and** its recorded health state.
///
/// Health and configuration were previously spread across five separate read
/// entrypoints (`get_oracle_health`, `is_oracle_circuit_tripped`,
/// `get_circuit_recovery_state`, `get_identity_oracles`, `get_identity_quorum`),
/// so an off-chain `/health` probe had to know and fan out all five to answer
/// "is this feed's oracle gating healthy?".
///
/// This performs **no cross-contract calls and no writes** — every field is a
/// local read of already-recorded state — so it is cheap enough for a
/// high-frequency health probe and safe to call at any time, including while
/// the contract is paused. Use [`check_identity_quorum_health`] when you need
/// the *live* per-feeder health, which necessarily queries the feeders.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleStatusView {
    pub feed_type: OracleFeedType,
    pub token: Address,
    /// The oracle the circuit-agnostic priority chain currently resolves to:
    /// per-token override → feed-type default → legacy `price_oracle`
    /// (`Identity` only). `None` = nothing registered.
    pub resolved_oracle: Option<Address>,
    /// Last recorded single-oracle health snapshot, or `None` if this feed +
    /// token has never been queried.
    ///
    /// Caveat: the multi-oracle Identity quorum path deliberately does not
    /// write this key (it models one oracle per feed + token), so while a
    /// quorum list is registered this stays `None` no matter how often
    /// verification runs. That blind spot is exactly what
    /// [`check_identity_quorum_health`] fills.
    pub health: Option<OracleHealthStatus>,
    /// Whether the circuit breaker is currently open for this feed + token.
    pub circuit_tripped: bool,
    /// In-progress half-open recovery of an open breaker (Issue #1048), or
    /// `None` when nothing is recovering.
    pub recovery: Option<CircuitRecoveryState>,
    /// Registered Identity quorum feeders, in registration order. Empty for
    /// non-`Identity` feeds and when the multi-oracle quorum is not in use (in
    /// which case the single-oracle chain above governs).
    pub identity_oracles: soroban_sdk::Vec<Address>,
    /// Configured Identity quorum threshold — the `N` of `N`-of-`M`. A single
    /// protocol-wide setting, reported for every feed, so only meaningful
    /// alongside `identity_oracles` when that list is non-empty. Note
    /// `remove_identity_oracle` does not re-validate the threshold, so removing
    /// feeders can leave a quorum too few feeders remain to reach — comparing
    /// this against `identity_oracles.len()` is how that state is caught.
    pub identity_quorum: u32,
    /// Registered price sources for this feed (cross-source deviation
    /// checking). Empty for non-`Price` feeds.
    pub price_sources: soroban_sdk::Vec<Address>,
}

/// Issue #1055: read the whole recorded oracle configuration + health state for
/// `feed_type` + `token` in a single call. Permissionless; performs no
/// cross-contract calls and writes nothing.
pub fn get_oracle_status(env: Env, feed_type: OracleFeedType, token: Address) -> OracleStatusView {
    OracleStatusView {
        feed_type,
        token: token.clone(),
        resolved_oracle: resolve_oracle(&env, feed_type, &token),
        health: env
            .storage()
            .persistent()
            .get(&DataKey::OracleHealth(feed_type, token.clone())),
        circuit_tripped: is_oracle_circuit_tripped(&env, feed_type, &token),
        recovery: env
            .storage()
            .persistent()
            .get(&DataKey::OracleCircuitRecovery(feed_type, token.clone())),
        identity_oracles: get_identity_oracles(env.clone(), feed_type),
        identity_quorum: get_identity_quorum(env.clone()),
        price_sources: get_price_sources(env, feed_type),
    }
}

/// Issue #1055: the single per-feeder predicate shared by the funding gate
/// ([`check_identity_quorum`]) and the [`check_identity_quorum_health`]
/// diagnostic, so the two can never disagree.
///
/// A feeder that reverts/traps/returns an undecodable payload degrades to
/// "no opinion" via the non-panicking `try_invoke_contract` (mirroring
/// [`query_price`](crate::oracle_registry)) rather than aborting the whole
/// call — but it still cannot contribute an agreement.
fn classify_identity_feeder(
    env: &Env,
    oracle: &Address,
    payer: &Address,
    current_ledger: u32,
    max_age_ledgers: u64,
) -> FeederHealth {
    let result = env.try_invoke_contract::<OracleVerificationResponse, soroban_sdk::Error>(
        oracle,
        &Symbol::new(env, "get_payer_data"),
        vec![env, payer.clone().into_val(env)],
    );
    match result {
        // Issue #1050: the same shared freshness rule the gate and the
        // recorded health snapshot use, so "fresh" means one thing everywhere.
        Ok(Ok(response)) => {
            if is_oracle_data_stale(current_ledger, response.timestamp, max_age_ledgers) {
                FeederHealth::Stale
            } else if response.is_verified {
                FeederHealth::Verified
            } else {
                FeederHealth::Unverified
            }
        }
        _ => FeederHealth::Unreachable,
    }
}

/// Issue #1055: live per-feeder health for the Identity quorum on `feed_type` +
/// `token`, plus the resulting quorum verdict.
///
/// This is the missing reader for quorum-mode health: the funding gate
/// ([`check_identity_quorum`]) never writes the single-oracle `OracleHealth`
/// key, so before this the *only* way to learn that one of several feeders had
/// gone stale, unverified, or unreachable was to observe funding failing closed
/// (which rolls its events back). The classification is produced by the same
/// [`classify_identity_feeder`] helper the gate uses, so `verified_count` /
/// `quorum_met` here always match what the gate decided, and `quorum_met` in
/// turn tracks `IdentityVerificationOutcome::Verified` from
/// [`check_oracle_verification`] (when the list is non-empty).
///
/// Permissionless and never-erroring: a feeder that reverts is reported as
/// [`FeederHealth::Unreachable`] rather than propagating an error. Like
/// [`check_oracle_health`] it writes nothing and emits nothing — the fan-out is
/// bounded by [`MAX_IDENTITY_ORACLES_PER_FEED`], so its cost is bounded even
/// with a full feeder list.
pub fn check_identity_quorum_health(
    env: Env,
    feed_type: OracleFeedType,
    token: Address,
    payer: Address,
) -> IdentityQuorumHealth {
    let feeders = get_identity_oracles(env.clone(), feed_type);
    let active = !feeders.is_empty();
    let required_quorum = get_identity_quorum(env.clone());
    let current_ledger = env.ledger().sequence();
    let max_age = crate::storage::get_config(&env)
        .map(|c| c.max_oracle_age_ledgers)
        .unwrap_or(crate::DEFAULT_MAX_ORACLE_AGE_LEDGERS);

    let mut observations = soroban_sdk::Vec::new(&env);
    let mut verified_count: u32 = 0;
    for feeder in feeders.iter() {
        let health = classify_identity_feeder(&env, &feeder, &payer, current_ledger, max_age);
        if health == FeederHealth::Verified {
            verified_count += 1;
        }
        observations.push_back(health);
    }

    IdentityQuorumHealth {
        feed_type,
        token,
        feeders,
        observations,
        verified_count,
        required_quorum,
        quorum_met: active && verified_count >= required_quorum,
    }
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
