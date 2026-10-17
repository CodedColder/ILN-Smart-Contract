//! Off-chain payer-verification oracle interface.
//!
//! Issue #91 — defines the cross-contract trait and helpers the ILN contract
//! uses to query payer verification status from a trusted oracle contract.
//!
//! The oracle is optional. When no oracle address is registered in
//! `Config.price_oracle`, all payer-verification checks pass (fail-open).

use soroban_sdk::{contractclient, contracttype, Address, Env};

/// Interface version expected by `invoice_liquidity` when registering an
/// oracle. Bump together with implementing contracts on breaking changes.
pub const ORACLE_INTERFACE_VERSION: u32 = 1;

/// Verification record returned by the oracle.
#[contracttype]
#[derive(Clone, Debug)]
pub struct VerificationResult {
    /// Whether the payer's off-chain identity/creditworthiness is verified.
    pub verified: bool,
    /// Unix epoch seconds when the oracle last updated this entry.
    pub timestamp: u64,
}

/// Cross-contract client trait for a payer-verification oracle.
///
/// Any contract registered as the ILN payer oracle must expose these
/// entry-points with exactly these signatures, including
/// [`interface_version`](OracleInterface::interface_version).
///
/// # Authorization model (Issue #1046)
///
/// `get_verification` is a read and needs no authorization. `update_verification`
/// *writes* verification state and is the oracle's **feeder** (operator) entry
/// point: it is the single write path that can flip a payer from unverified to
/// verified — which, when `fund_invoice` is called with
/// `require_oracle_verification = true`, is precisely what unlocks funding.
///
/// An oracle implementation **MUST** authorize this entry point. Concretely it
/// must:
///
/// 1. persist a designated feeder address (set once by the deployer / operator
///    during deployment), and
/// 2. require that address's signature (`feeder.require_auth()`) before
///    mutating any verification state, and
/// 3. reject the call when no feeder has been configured.
///
/// An unrestricted `update_verification` lets anyone mint funding eligibility
/// for any payer, so it must not ship. See
/// `docs/oracle-integration.md` § Security Checklist and
/// `docs/access-control.md` (Oracle Feeder role). The ILN contract never calls
/// this method directly, so the authorization is enforced by the oracle contract
/// the ILN registry points at, not by the ILN contract.
#[contractclient(name = "OracleClient")]
pub trait OracleInterface {
    /// Returns [`ORACLE_INTERFACE_VERSION`].
    fn interface_version(env: Env) -> u32;

    /// Returns the verification record for `payer`.
    fn get_verification(env: Env, payer: Address) -> VerificationResult;

    /// Update the verification status for `payer`. **Feeder-authorized.**
    ///
    /// Implementations MUST require the configured feeder's authorization
    /// (`feeder.require_auth()`) and MUST reject the call when no feeder has
    /// been configured. The ILN contract never calls this method directly — the
    /// authorization is enforced inside the oracle contract.
    fn update_verification(env: Env, payer: Address, verified: bool);
}

/// Default staleness threshold: 7 days in seconds.
pub const ORACLE_STALENESS_THRESHOLD_SECS: u64 = 7 * 24 * 60 * 60;

/// Query the payer oracle (if one is configured) and return whether `payer`
/// is verified with fresh data.
///
/// Semantics:
/// - Returns `true` (permissive) if no oracle address is in `Config.price_oracle`.
/// - Returns `false` if the oracle reports the payer as unverified.
/// - Returns `false` if the oracle's timestamp is older than
///   `ORACLE_STALENESS_THRESHOLD_SECS`.
/// - Returns `true` when verified and the data is fresh.
///
/// **Dead code (Issue #860 enumeration):** no caller inside or outside this
/// crate resolves payer verification through this helper — `fund_invoice`
/// routes payer checks through `oracle_registry::resolve_oracle_for_verification`
/// (which consults circuit/health state) instead. Kept as the legacy
/// boolean-check API; note that unlike every live path, this function does
/// *not* consult the health gate, which is precisely why it must not be
/// reconnected to a price-dependent flow without adding that check.
pub fn check_payer_verified(env: &Env, payer: &Address) -> bool {
    let config = match crate::storage::get_config(env) {
        Some(c) => c,
        None => return true,
    };
    let oracle_addr = match config.price_oracle {
        Some(a) => a,
        None => return true,
    };
    let client = OracleClient::new(env, &oracle_addr);
    let result = client.get_verification(payer);
    if !result.verified {
        return false;
    }
    env.ledger().timestamp().saturating_sub(result.timestamp) <= ORACLE_STALENESS_THRESHOLD_SECS
}
