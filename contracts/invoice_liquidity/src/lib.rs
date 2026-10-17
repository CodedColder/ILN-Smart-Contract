#![no_std]
// Soroban's contractimpl/contractargs macros generate client functions that
// mirror the contract's public interface — these may exceed the 7-argument
// threshold when the source function itself has many arguments.
#![allow(clippy::too_many_arguments)]

#[cfg(test)]
extern crate std;

pub mod access;
pub mod config;
pub mod errors;
pub mod events;
pub mod invoice;
pub mod multisig;
pub mod nft;
pub mod rate_logic;
pub mod storage;
pub mod top_payers;
pub mod twap_accumulator;
use access::*;
use access::{check_rate_limit, lock_reentrancy, unlock_reentrancy};
pub mod constants;
use constants::{
    ADMIN_CHANGE_COOLDOWN_LEDGERS, DEFAULT_RATE_LIMIT_LEDGERS, ECONOMIC_PARAM_COOLDOWN_LEDGERS,
    QUEUE_DELAY_LEDGERS, UPGRADE_COOLDOWN_LEDGERS,
};
pub mod oracle_interface;
pub mod oracle_registry;
pub mod twap;
use insurance_pool::InsurancePoolInterfaceClient;
use oracle_registry::OracleFeedType;
use twap_accumulator::{record_observation, get_twap, TWAPError};

pub use crate::invoice::{
    AppealRecord, Invoice, InvoiceParams, InvoiceStatus, LpFundRequest, ReferralCode,
    ReputationProfile, ReputationScore, TopPayerEntry,
};
pub use crate::nft::InvoiceNftMetadata;
pub use crate::storage::DataKey;
pub use config::{Config, ConfigError};
pub use errors::ContractError;
use soroban_sdk::{
    contract, contractimpl, token::Client as TokenClient, vec, Address, BytesN, Env, IntoVal,
    Symbol, Vec,
};

use crate::storage::get_admin;
use events::{
    AdminChanged, AppealResolved, ContractInitialized, ContractPaused, ContractUnpaused,
    ContractUpgraded, DefaultAppealed, DisputeResolved, DisputeUpheldPayerRefund,
    DistributionContractUpdated, FundQueueResolutionAttempted, FundQueueResolved, FundRequested,
    InsuranceClaimAttempted, InvoiceCancelled, InvoiceDefaulted, InvoiceDisputed, InvoiceExpired,
    InvoiceFunded, InvoicePaid, InvoicePartiallyPaid, InvoiceSubmitted, InvoiceTokenChanged,
    InvoiceTransferred, InvoiceUpdated, LPPositionTransferred, ParameterUpdated,
    PriceOracleUpdated, SignerRotationCancelled, SignerRotationFinalized,
    SignerRotationScheduled, TokenAdded, TokenRemoved,
};
use invoice::{
    add_invoice_to_lp, add_invoice_to_submitter, add_volume, get_appeal, get_contract_stats,
    get_dispute, get_fund_queue, get_fund_queue_opened_at, get_invoice_funders, get_lp_invoices,
    get_lp_score, get_max_invoice_amount, get_min_payer_reputation, get_payer_score,
    get_pre_default_payer_score, get_queue_resolution, get_reputation, get_submitter_invoices,
    get_token_volume, get_token_volume_cap, increment_invoices_defaulted, increment_invoices_paid,
    increment_invoices_submitted, increment_total_funded, increment_total_invoices,
    increment_total_paid, invoice_exists, is_paused, load_invoice, next_invoice_id,
    remove_invoice_from_lp, remove_invoice_from_submitter, save_appeal, save_dispute,
    save_fund_queue, save_invoice, save_invoice_funders, save_pre_default_payer_score,
    save_queue_resolution, set_lp_score, set_max_invoice_amount, set_min_payer_reputation,
    set_paused, set_payer_score, set_reputation, set_token_volume_cap, try_load_invoice,
    try_set_fund_queue_opened_at, ContractStats, DisputeRecord, ProtocolStatus, StorageKey,
};
use invoice::get_last_pause_timestamp;
// 30-day window in seconds for a payer to file an appeal after a default.
const APPEAL_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;

// ----------------------------------------------------------------
// CONSTANTS
// ----------------------------------------------------------------

/// Minimum invoice duration: 24 hours (in seconds)
const MIN_INVOICE_DURATION: u64 = 24 * 60 * 60;

/// Maximum invoice duration: 365 days (in seconds)
const MAX_INVOICE_DURATION: u64 = 365 * 24 * 60 * 60;

/// Default oracle freshness window: ~24 hours at one ledger per 5 seconds.
/// Governance can override this per-contract via set_max_oracle_age().
pub const DEFAULT_MAX_ORACLE_AGE_LEDGERS: u64 = 17_280;

// ----------------------------------------------------------------
// ORACLE TYPES (Issue #93)
// ----------------------------------------------------------------

use soroban_sdk::contracttype;

/// Response returned by the oracle's get_payer_data() entry point.
/// Combines identity verification with a freshness timestamp so the
/// contract can reject stale data without a second round-trip.
#[contracttype]
#[derive(Clone, Debug)]
pub struct OracleVerificationResponse {
    /// Whether the payer has passed oracle identity/creditworthiness checks.
    pub is_verified: bool,
    /// Ledger sequence number at which this data was last updated by the oracle.
    /// fund_invoice() rejects responses where current_ledger - timestamp ≥ max_oracle_age_ledgers.
    pub timestamp: u32,
}

// ----------------------------------------------------------------
// CONTRACT
// ----------------------------------------------------------------

#[contract]
pub struct InvoiceLiquidityContract;

#[allow(clippy::too_many_arguments)]
#[contractimpl]
impl InvoiceLiquidityContract {
    // ------------------------------------------------------------
    // initialize (multi-token aware)
    // ------------------------------------------------------------
    /// Access: Anyone
    pub fn initialize(
        env: Env,
        admin: Address,
        usdc_token: Address,
        eurc_token: Address,
        xlm_token: Address,
    ) -> Result<(), ContractError> {
        if env
            .storage()
            .instance()
            .has(&crate::storage::DataKey::InvoiceCount)
        {
            return Err(ContractError::AlreadyInitialized);
        }

        env.storage()
            .instance()
            .set(&crate::storage::DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&crate::storage::DataKey::FeeRate, &0_u32);
        env.storage()
            .instance()
            .set(&crate::storage::DataKey::MaxDiscountRate, &5000_u32);

        if !env.storage().instance().has(&StorageKey::NextInvoiceId) {
            env.storage()
                .instance()
                .set(&StorageKey::NextInvoiceId, &1_u64);
        }

        // Initialize config with token addresses
        let initial_config = crate::config::Config {
            high_rep_threshold: 70,
            bonus_bps: 100,
            min_discount_rate_bps: 100,
            decay_rate_bps: 50,
            decay_period_ledgers: 10000,
            dispute_timeout_ledgers: 10000,
            xlm_sac_address: xlm_token.clone(),
            usdc_sac_address: usdc_token.clone(),
            eurc_sac_address: eurc_token.clone(),
            price_oracle: None,
            max_oracle_age_ledgers: DEFAULT_MAX_ORACLE_AGE_LEDGERS,
        };
        crate::storage::set_config(&env, &initial_config);

        // approve first token (USDC: 6 decimals)
        // approve initial tokens
        env.storage().persistent().set(
            &crate::storage::DataKey::ApprovedToken(usdc_token.clone()),
            &true,
        );

        env.storage().persistent().set(
            &crate::storage::DataKey::ApprovedToken(eurc_token.clone()),
            &true,
        );
        env.storage().persistent().set(
            &crate::storage::DataKey::TokenDecimals(usdc_token.clone()),
            &6_u32,
        );

        // approve native XLM SAC (7 decimals)
        env.storage().persistent().set(
            &crate::storage::DataKey::ApprovedToken(xlm_token.clone()),
            &true,
        );
        env.storage().persistent().set(
            &crate::storage::DataKey::TokenDecimals(xlm_token.clone()),
            &7_u32,
        );

        let mut list: Vec<Address> = Vec::new(&env);
        list.push_back(usdc_token);
        list.push_back(xlm_token);
        list.push_back(eurc_token);

        env.storage()
            .persistent()
            .set(&crate::storage::DataKey::TokenList, &list);

        env.events().publish(
            (Symbol::new(&env, "initialized"), admin.clone()),
            ContractInitialized {
                admin,
                usdc_token: list.get(0).unwrap(),
                xlm_token: list.get(1).unwrap(),
                eurc_token: list.get(2).unwrap(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(())
    }

    // ------------------------------------------------------------
    // Version view
    // ------------------------------------------------------------
    /// Access: Anyone
    pub fn get_version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, crate::constants::CONTRACT_VERSION)
    }

    // ------------------------------------------------------------
    /// Access: Admin only
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "set_admin", ADMIN_CHANGE_COOLDOWN_LEDGERS)?;
        record_admin_action(&env, "set_admin");
        let old_admin: Address = env.storage().instance().get(&StorageKey::Admin).unwrap();
        env.storage().instance().set(&StorageKey::Admin, &new_admin);
        env.events().publish(
            (Symbol::new(&env, "admin_changed"),),
            AdminChanged {
                old_admin,
                new_admin,
                timestamp: env.ledger().timestamp(),
            },
        );
        Ok(())
    }

    /// Access: Admin only
    pub fn update_fee_rate(env: Env, rate: u32) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "update_fee_rate", ECONOMIC_PARAM_COOLDOWN_LEDGERS)?;
        record_admin_action(&env, "update_fee_rate");

        let old_rate: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::FeeRate)
            .unwrap_or(0);
        env.storage().instance().set(&StorageKey::FeeRate, &rate);
        let updated_by = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        let pn = Symbol::new(&env, "protocol_fee_rate_bps");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn.clone(),
                updated_by.clone(),
            ),
            ParameterUpdated {
                param_name: pn,
                old_value: old_rate as i128,
                new_value: rate as i128,
                updated_by,
            },
        );
        Ok(())
    }

    /// Access: Admin only
    pub fn update_max_discount(env: Env, rate: u32) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "update_max_discount", ECONOMIC_PARAM_COOLDOWN_LEDGERS)?;
        record_admin_action(&env, "update_max_discount");

        let old_rate: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MaxDiscountRate)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StorageKey::MaxDiscountRate, &rate);
        let updated_by = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        let pn = Symbol::new(&env, "max_discount_rate_bps");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn.clone(),
                updated_by.clone(),
            ),
            ParameterUpdated {
                param_name: pn,
                old_value: old_rate as i128,
                new_value: rate as i128,
                updated_by,
            },
        );
        Ok(())
    }

    /// Update reputation decay parameters. Admin or governance only.
    /// Access: Admin only
    pub fn update_decay_params(
        env: Env,
        rate_bps: u32,
        period_ledgers: u64,
    ) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "update_decay_params", ECONOMIC_PARAM_COOLDOWN_LEDGERS)?;
        record_admin_action(&env, "update_decay_params");
        let admin = get_admin(&env).ok_or(ContractError::Unauthorized)?;

        let mut config = crate::storage::get_config(&env).ok_or(ContractError::Unauthorized)?;
        let old_rate = config.decay_rate_bps;
        let old_period = config.decay_period_ledgers;

        config.decay_rate_bps = rate_bps;
        config.decay_period_ledgers = period_ledgers;
        crate::storage::set_config(&env, &config);

        let pn_rate = Symbol::new(&env, "decay_rate_bps");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn_rate.clone(),
                admin.clone(),
            ),
            ParameterUpdated {
                param_name: pn_rate,
                old_value: old_rate as i128,
                new_value: rate_bps as i128,
                updated_by: admin.clone(),
            },
        );

        let pn_period = Symbol::new(&env, "decay_period_ledgers");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn_period.clone(),
                admin.clone(),
            ),
            ParameterUpdated {
                param_name: pn_period,
                old_value: old_period as i128,
                new_value: period_ledgers as i128,
                updated_by: admin,
            },
        );

        Ok(())
    }

    /// Access: Admin only
    pub fn set_distribution_contract(
        env: Env,
        distribution_contract: Address,
    ) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(
            &env,
            "set_distribution_contract",
            DEFAULT_RATE_LIMIT_LEDGERS,
        )?;
        record_admin_action(&env, "set_distribution_contract");

        let old_distribution_contract: Option<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::DistributionContract);
        env.storage()
            .instance()
            .set(&StorageKey::DistributionContract, &distribution_contract);

        let updated_by = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        env.events().publish(
            (
                Symbol::new(&env, "distribution_contract_updated"),
                updated_by.clone(),
            ),
            DistributionContractUpdated {
                old_distribution_contract,
                new_distribution_contract: distribution_contract,
                updated_by,
            },
        );
        Ok(())
    }

    /// Access: Admin only
    pub fn set_price_oracle(env: Env, oracle: Address) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "set_price_oracle", DEFAULT_RATE_LIMIT_LEDGERS)?;
        record_admin_action(&env, "set_price_oracle");
        let admin = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        let old_oracle = crate::storage::get_config(&env).and_then(|c| c.price_oracle);
        crate::config::set_price_oracle(&env, &admin, oracle.clone())
            .map_err(|_| ContractError::Unauthorized)?;

        env.events().publish(
            (Symbol::new(&env, "price_oracle_updated"), admin.clone()),
            PriceOracleUpdated {
                old_oracle,
                new_oracle: oracle,
                updated_by: admin,
            },
        );
        Ok(())
    }

    /// Access: Anyone
    pub fn get_price_oracle(env: Env) -> Option<Address> {
        crate::storage::get_config(&env).and_then(|config| config.price_oracle)
    }

    /// Update the maximum oracle data age in ledgers. Admin / governance only.
    ///
    /// Setting this to 0 disables the freshness check entirely (not recommended
    /// for production — stale data is as dangerous as no oracle).
    /// Access: Admin only
    pub fn set_max_oracle_age(env: Env, max_age_ledgers: u64) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "set_max_oracle_age", DEFAULT_RATE_LIMIT_LEDGERS)?;
        record_admin_action(&env, "set_max_oracle_age");
        let admin = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        let old_max_age = crate::storage::get_config(&env)
            .map(|c| c.max_oracle_age_ledgers)
            .unwrap_or(DEFAULT_MAX_ORACLE_AGE_LEDGERS);
        crate::config::set_max_oracle_age(&env, &admin, max_age_ledgers)
            .map_err(|_| ContractError::Unauthorized)?;

        let pn = Symbol::new(&env, "max_oracle_age_ledgers");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn.clone(),
                admin.clone(),
            ),
            ParameterUpdated {
                param_name: pn,
                old_value: old_max_age as i128,
                new_value: max_age_ledgers as i128,
                updated_by: admin,
            },
        );
        Ok(())
    }

    /// Return the configured maximum oracle data age in ledgers.
    /// Access: Anyone
    pub fn get_max_oracle_age(env: Env) -> u64 {
        crate::storage::get_config(&env)
            .map(|c| c.max_oracle_age_ledgers)
            .unwrap_or(DEFAULT_MAX_ORACLE_AGE_LEDGERS)
    }

    // ── Issue #532: governance-controlled oracle registry ─────────

    /// Register (or update) the default oracle for `feed_type`, applying to
    /// every token without a more specific per-token override.
    /// Access: Admin only (governance-controlled via cross-contract proposal
    /// execution, same pattern as `update_fee_rate` / `add_token`).
    pub fn register_oracle(
        env: Env,
        feed_type: OracleFeedType,
        oracle: Address,
    ) -> Result<(), ContractError> {
        oracle_registry::register_oracle(&env, feed_type, oracle)
    }

    /// Remove the default oracle for `feed_type`.
    /// Access: Admin only.
    pub fn remove_oracle(env: Env, feed_type: OracleFeedType) -> Result<(), ContractError> {
        oracle_registry::remove_oracle(&env, feed_type)
    }

    /// Register (or update) a per-token override oracle for `feed_type`.
    /// Access: Admin only.
    pub fn register_token_oracle(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
        oracle: Address,
    ) -> Result<(), ContractError> {
        oracle_registry::register_token_oracle(&env, feed_type, token, oracle)
    }

    /// Remove a per-token override oracle for `feed_type`.
    /// Access: Admin only.
    pub fn remove_token_oracle(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
    ) -> Result<(), ContractError> {
        oracle_registry::remove_token_oracle(&env, feed_type, token)
    }

    /// Resolve the oracle address that would be queried for `feed_type` +
    /// `token` (per-token override, then feed-type default, then — for
    /// `Identity` only — the legacy `price_oracle` config field).
    /// Access: Anyone
    pub fn get_oracle_for_token(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
    ) -> Option<Address> {
        oracle_registry::get_oracle_for_token(env, feed_type, token)
    }

    /// Return the last recorded health snapshot for `feed_type` + `token`,
    /// or `None` if that oracle has never been queried.
    /// Access: Anyone
    pub fn get_oracle_health(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
    ) -> Option<oracle_registry::OracleHealthStatus> {
        oracle_registry::get_oracle_health(env, feed_type, token)
    }

    /// Actively query the oracle resolved for `feed_type` + `token` (using
    /// `payer`'s record) and record + return its current health status.
    /// Unlike `fund_invoice`'s inline check, this never errors on stale or
    /// unverified data — it only reports the observation — making it safe
    /// for off-chain monitors/keepers to poll independent of funding activity.
    /// Access: Anyone
    pub fn check_oracle_health(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
        payer: Address,
    ) -> Option<oracle_registry::OracleHealthStatus> {
        oracle_registry::check_oracle_health(env, feed_type, token, payer)
    }

    /// Whether the oracle circuit breaker for `feed_type` + `token` is
    /// currently tripped (from `MAX_CONSECUTIVE_STALE_QUERIES` consecutive
    /// stale responses) and therefore excluded from `fund_invoice`'s
    /// oracle-gated resolution until governance calls
    /// `reset_oracle_circuit`.
    /// Access: Anyone
    pub fn is_oracle_circuit_tripped(env: Env, feed_type: OracleFeedType, token: Address) -> bool {
        oracle_registry::is_oracle_circuit_tripped(&env, feed_type, &token)
    }

    /// Reset a tripped oracle circuit breaker for `feed_type` + `token`.
    /// There is no automatic recovery on a single fresh query — this
    /// explicit, governance-gated call is required, so a flapping oracle
    /// can't quietly resume being trusted by the funding path.
    /// Access: Admin only (governance-controlled via cross-contract proposal
    /// execution, same pattern as `register_oracle` / `remove_oracle`).
    pub fn reset_oracle_circuit(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
    ) -> Result<(), ContractError> {
        oracle_registry::reset_oracle_circuit(&env, feed_type, token)
    }

    // ── Issue #price-deviation: multi-source price deviation checking ──

    /// Register `oracle` as an additional price source for `feed_type`.
    /// Distinct from `register_oracle`/`register_token_oracle` (the
    /// single-oracle model for boolean payer verification): multiple price
    /// sources can be registered per feed type so `get_verified_price` can
    /// cross-check them against each other.
    /// Access: Admin only (governance-controlled via cross-contract proposal
    /// execution, same pattern as `register_oracle`).
    pub fn add_price_source(
        env: Env,
        feed_type: OracleFeedType,
        oracle: Address,
    ) -> Result<(), ContractError> {
        oracle_registry::add_price_source(&env, feed_type, oracle)
    }

    /// Remove `oracle` from `feed_type`'s price source list.
    /// Access: Admin only.
    pub fn remove_price_source(
        env: Env,
        feed_type: OracleFeedType,
        oracle: Address,
    ) -> Result<(), ContractError> {
        oracle_registry::remove_price_source(&env, feed_type, oracle)
    }

    /// The currently registered price sources for `feed_type`.
    /// Access: Anyone
    pub fn get_price_sources(env: Env, feed_type: OracleFeedType) -> soroban_sdk::Vec<Address> {
        oracle_registry::get_price_sources(env, feed_type)
    }

    /// Update the governance-configurable maximum deviation (basis points)
    /// a price source may differ from the cross-source median before being
    /// rejected as an outlier. Default is
    /// [`oracle_registry::DEFAULT_MAX_PRICE_DEVIATION_BPS`] (5%) until set.
    /// Access: Admin only.
    pub fn set_max_price_deviation_bps(env: Env, bps: u32) -> Result<(), ContractError> {
        oracle_registry::set_max_price_deviation_bps(&env, bps)
    }

    /// The currently configured maximum price deviation, in basis points.
    /// Access: Anyone
    pub fn get_max_price_deviation_bps(env: Env) -> u32 {
        oracle_registry::get_max_price_deviation_bps(env)
    }

    /// Query every registered price source for `feed_type` + `token` and
    /// return a cross-validated price. With zero sources, errors; with
    /// exactly one, returns it unchecked (documented single-source risk —
    /// see `oracle_registry`'s module docs); with two or more, rejects any
    /// source deviating from the cross-source median beyond
    /// `get_max_price_deviation_bps()` and returns the median of the
    /// survivors.
    ///
    /// Issue #816: when TWAP is enabled for `feed_type` (see
    /// `set_twap_enabled`) and enough in-window samples exist, returns the
    /// windowed TWAP average instead of the spot median. Disabled by
    /// default, preserving existing behavior.
    /// Access: Anyone
    pub fn get_verified_price(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
    ) -> Result<i128, ContractError> {
        oracle_registry::get_verified_price(env, feed_type, token)
    }

    // ── Issues #815/#816/#817: TWAP accumulator + per-feed opt-in ──

    /// Whether `feed_type` routes `Price` reads through the TWAP windowed
    /// average (`true`) or raw spot (`false`, default).
    /// Access: Anyone
    pub fn is_twap_enabled(env: Env, feed_type: OracleFeedType) -> bool {
        oracle_registry::is_twap_enabled(&env, feed_type)
    }

    /// Enable or disable the TWAP path for `feed_type`. Disabled by default
    /// so existing feeds behave exactly as before until explicitly opted in.
    /// Access: Admin only.
    pub fn set_twap_enabled(
        env: Env,
        feed_type: OracleFeedType,
        enabled: bool,
    ) -> Result<(), ContractError> {
        oracle_registry::set_twap_enabled(&env, feed_type, enabled)
    }

    /// The currently configured TWAP window in ledgers (default 720 ≈ 1h).
    /// Access: Anyone
    pub fn get_twap_window(env: Env) -> u64 {
        oracle_registry::get_twap_window_ledgers(&env)
    }

    /// Update the TWAP window, rejecting values outside
    /// `[MIN_TWAP_WINDOW_LEDGERS, MAX_TWAP_WINDOW_LEDGERS]` with
    /// `ContractError::InvalidTwapWindow` (Issue #817).
    /// Access: Admin only.
    pub fn set_twap_window(env: Env, window_ledgers: u64) -> Result<(), ContractError> {
        oracle_registry::set_twap_window_ledgers(&env, window_ledgers)
    }

    /// Record a price observation for `feed_type` + `token` at the current
    /// ledger timestamp.
    /// Access: Admin only.
    pub fn record_twap_sample(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
        price: i128,
    ) -> Result<(), ContractError> {
        oracle_registry::record_twap_sample(&env, feed_type, token, price)
    }

    /// Windowed TWAP average for `feed_type` + `token`, or `None` when
    /// fewer than two in-window samples exist.
    /// Access: Anyone
    pub fn get_twap_price(
        env: Env,
        feed_type: OracleFeedType,
        token: Address,
    ) -> Option<i128> {
        oracle_registry::get_twap_price(&env, feed_type, &token)
    }

    // ── Issue #529: insurance pool integration ────────────────────

    /// Configure the deployed insurance pool contract address consulted by
    /// `claim_default` to compensate enrolled LPs on a confirmed default.
    /// Access: Admin only
    pub fn set_insurance_pool(env: Env, pool: Address) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "set_insurance_pool", DEFAULT_RATE_LIMIT_LEDGERS)?;
        record_admin_action(&env, "set_insurance_pool");
        crate::storage::set_insurance_pool(&env, &pool);
        Ok(())
    }

    /// Return the configured insurance pool contract address, if any.
    /// Access: Anyone
    pub fn get_insurance_pool(env: Env) -> Option<Address> {
        crate::storage::get_insurance_pool(&env)
    }

    /// Access: Admin only
    pub fn add_token(env: Env, token: Address, decimals: u32) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "add_token", DEFAULT_RATE_LIMIT_LEDGERS)?;
        record_admin_action(&env, "add_token");

        let token_client = token_client(&env, &token);
        let contract_address = env.current_contract_address();
        let test_amount: i128 = 1_000_000;
        let admin_address: Address = env
            .storage()
            .instance()
            .get(&crate::storage::DataKey::Admin)
            .unwrap();
        let before_balance = token_client.balance(&contract_address);

        token_client.transfer(&admin_address, &contract_address, &test_amount);

        let after_balance = token_client.balance(&contract_address);
        let received = after_balance.checked_sub(before_balance).unwrap_or(0);
        if received != test_amount {
            if received > 0 {
                token_client.transfer(&contract_address, &admin_address, &received);
            }
            return Err(ContractError::FeeOnTransferToken);
        }

        // Return the exact test amount to the admin account after verification.
        token_client.transfer(&contract_address, &admin_address, &test_amount);

        env.storage().persistent().set(
            &crate::storage::DataKey::ApprovedToken(token.clone()),
            &true,
        );

        // Store the decimal precision for this token so amount comparisons and
        // minimum-amount checks can be scaled correctly (Issue #23).
        env.storage().persistent().set(
            &crate::storage::DataKey::TokenDecimals(token.clone()),
            &decimals,
        );

        let mut list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&crate::storage::DataKey::TokenList)
            .unwrap_or(Vec::new(&env));
        if !list.contains(&token) {
            list.push_back(token.clone());
            env.storage()
                .persistent()
                .set(&crate::storage::DataKey::TokenList, &list);
        }

        env.events().publish(
            (Symbol::new(&env, "token_added"), token.clone()),
            TokenAdded { token, decimals },
        );
        Ok(())
    }

    /// Access: Admin only
    pub fn remove_token(env: Env, token: Address) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "remove_token", DEFAULT_RATE_LIMIT_LEDGERS)?;
        record_admin_action(&env, "remove_token");

        env.storage()
            .persistent()
            .set(&StorageKey::ApprovedToken(token.clone()), &false);

        // Keep the allowlist Vec in sync with the ApprovedToken flag.
        let list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&crate::storage::DataKey::TokenList)
            .unwrap_or(Vec::new(&env));
        let mut pruned: Vec<Address> = Vec::new(&env);
        for t in list.iter() {
            if t != token {
                pruned.push_back(t);
            }
        }
        env.storage()
            .persistent()
            .set(&crate::storage::DataKey::TokenList, &pruned);

        env.events().publish(
            (Symbol::new(&env, "token_removed"), token.clone()),
            TokenRemoved { token },
        );
        Ok(())
    }

    /// Return the registered decimal precision for a token.
    ///
    /// Returns `None` when the token has never been registered via
    /// `add_token` or `initialize`. For the two bootstrap tokens (USDC at 6
    /// decimals and XLM at 7 decimals) these are set automatically during
    /// `initialize`.
    ///
    /// Access: Anyone
    pub fn get_token_decimals(env: Env, token: Address) -> Option<u32> {
        env.storage()
            .persistent()
            .get(&crate::storage::DataKey::TokenDecimals(token))
    }

    // ------------------------------------------------------------
    // pause / unpause (emergency controls)
    // ------------------------------------------------------------
    /// Access: Admin only
    pub fn pause(env: Env) -> Result<(), ContractError> {
        require_admin(&env)?;
        record_admin_action(&env, "pause");

        set_paused(&env, true);
        env.events().publish(
            (Symbol::new(&env, "paused"),),
            ContractPaused {
                timestamp: env.ledger().timestamp(),
            },
        );
        Ok(())
    }

    /// Access: Admin only
    pub fn unpause(env: Env) -> Result<(), ContractError> {
        require_admin(&env)?;
        record_admin_action(&env, "unpause");

        set_paused(&env, false);
        env.events().publish(
            (Symbol::new(&env, "unpaused"),),
            ContractUnpaused {
                timestamp: env.ledger().timestamp(),
            },
        );
        Ok(())
    }

    // ------------------------------------------------------------
    // Multi-sig admin (Issue #124 / #641)
    // ------------------------------------------------------------
    // Threshold-gated alternative to the single-admin pause/unpause/
    // remove_token/update_fee_rate/update_max_discount calls above: once
    // bootstrapped, critical admin actions can instead be routed through
    // propose_*/sign_proposal/execute_proposal, requiring `threshold`
    // distinct signers to agree. The existing single-admin entry points
    // above are unaffected — this is an additive authorization path.

    /// Bootstrap the multisig admin signer set and approval threshold.
    /// Access: Admin only (one-time; see `multisig::initialize`)
    pub fn initialize_multisig_admin(
        env: Env,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), ContractError> {
        multisig::initialize(&env, signers, threshold)
    }

    /// Returns the configured multisig admin signer set / threshold, if bootstrapped.
    /// Access: Anyone
    pub fn get_multisig_admin(env: Env) -> Option<multisig::MultisigAdmin> {
        env.storage().instance().get(&DataKey::MultisigAdmin)
    }

    /// Returns a stored multisig proposal by id, if any.
    /// Access: Anyone
    pub fn get_multisig_proposal(env: Env, proposal_id: u64) -> Option<multisig::MultisigProposal> {
        env.storage()
            .persistent()
            .get(&DataKey::MultisigProposal(proposal_id))
    }

    /// Propose pausing the contract via the multisig flow.
    /// Access: configured multisig signer only
    pub fn propose_pause(env: Env, proposer: Address) -> Result<u64, ContractError> {
        multisig::propose(&env, &proposer, multisig::AdminAction::Pause)
    }

    /// Propose unpausing the contract via the multisig flow.
    /// Access: configured multisig signer only
    pub fn propose_unpause(env: Env, proposer: Address) -> Result<u64, ContractError> {
        multisig::propose(&env, &proposer, multisig::AdminAction::Unpause)
    }

    /// Propose removing a token from the approved-token allowlist via the multisig flow.
    /// Access: configured multisig signer only
    pub fn propose_remove_token(
        env: Env,
        proposer: Address,
        token: Address,
    ) -> Result<u64, ContractError> {
        multisig::propose(&env, &proposer, multisig::AdminAction::RemoveToken(token))
    }

    /// Propose a new protocol fee rate via the multisig flow.
    /// Access: configured multisig signer only
    pub fn propose_set_fee_rate(env: Env, proposer: Address, rate: u32) -> Result<u64, ContractError> {
        multisig::propose(&env, &proposer, multisig::AdminAction::SetFeeRate(rate))
    }

    /// Propose a new maximum discount rate via the multisig flow.
    /// Access: configured multisig signer only
    pub fn propose_set_max_discount(
        env: Env,
        proposer: Address,
        rate: u32,
    ) -> Result<u64, ContractError> {
        multisig::propose(&env, &proposer, multisig::AdminAction::SetMaxDiscount(rate))
    }

    /// Propose replacing the multisig signer set / threshold via the multisig flow.
    /// Access: configured multisig signer only
    pub fn propose_update_multisig(
        env: Env,
        proposer: Address,
        new_signers: Vec<Address>,
        new_threshold: u32,
    ) -> Result<u64, ContractError> {
        multisig::propose(
            &env,
            &proposer,
            multisig::AdminAction::UpdateMultisig(new_signers, new_threshold),
        )
    }

    /// Propose rotating `old_signer` out for `new_signer` via the multisig
    /// flow. Executing this proposal schedules the swap behind a timelock
    /// (see `finalize_signer_rotation`) rather than applying it
    /// immediately (Issue #640).
    /// Access: configured multisig signer only
    pub fn propose_rotate_signer(
        env: Env,
        proposer: Address,
        old_signer: Address,
        new_signer: Address,
    ) -> Result<u64, ContractError> {
        multisig::propose(
            &env,
            &proposer,
            multisig::AdminAction::RotateSigner(old_signer, new_signer),
        )
    }

    /// Returns the currently scheduled signer rotation, if any (Issue #640).
    /// Access: Anyone
    pub fn get_pending_signer_rotation(env: Env) -> Option<multisig::PendingRotation> {
        env.storage().instance().get(&DataKey::PendingSignerRotation)
    }

    /// Finalize a scheduled signer rotation once its timelock has elapsed,
    /// swapping a compromised or departing signer's key in the multisig
    /// signer set without a contract upgrade (Issue #640).
    /// Access: configured multisig signer only
    pub fn finalize_signer_rotation(env: Env, caller: Address) -> Result<(), ContractError> {
        let rotation = multisig::finalize_rotation(&env, &caller)?;
        env.events().publish(
            (Symbol::new(&env, "signer_rotation_finalized"),),
            SignerRotationFinalized {
                old_signer: rotation.old_signer,
                new_signer: rotation.new_signer,
            },
        );
        Ok(())
    }

    /// Cancel a pending signer rotation before it takes effect — the
    /// reaction mechanism the timelock exists to enable if a scheduled
    /// rotation looks malicious or mistaken (Issue #640).
    /// Access: configured multisig signer only
    pub fn cancel_signer_rotation(env: Env, caller: Address) -> Result<(), ContractError> {
        let rotation = multisig::cancel_rotation(&env, &caller)?;
        env.events().publish(
            (Symbol::new(&env, "signer_rotation_cancelled"),),
            SignerRotationCancelled {
                old_signer: rotation.old_signer,
                new_signer: rotation.new_signer,
            },
        );
        Ok(())
    }

    /// Approve a pending multisig proposal.
    /// Access: configured multisig signer only
    pub fn sign_proposal(env: Env, signer: Address, proposal_id: u64) -> Result<(), ContractError> {
        multisig::sign(&env, &signer, proposal_id)
    }

    /// Execute a multisig proposal once its approval threshold has been
    /// met, applying the action it authorized.
    /// Access: configured multisig signer only
    pub fn execute_proposal(env: Env, caller: Address, proposal_id: u64) -> Result<(), ContractError> {
        let action = multisig::execute(&env, &caller, proposal_id)?;
        match action {
            multisig::AdminAction::Pause => {
                set_paused(&env, true);
                env.events().publish(
                    (Symbol::new(&env, "paused"),),
                    ContractPaused {
                        timestamp: env.ledger().timestamp(),
                    },
                );
            }
            multisig::AdminAction::Unpause => {
                set_paused(&env, false);
                env.events().publish(
                    (Symbol::new(&env, "unpaused"),),
                    ContractUnpaused {
                        timestamp: env.ledger().timestamp(),
                    },
                );
            }
            multisig::AdminAction::RemoveToken(token) => {
                env.storage()
                    .persistent()
                    .set(&StorageKey::ApprovedToken(token.clone()), &false);
                let list: Vec<Address> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TokenList)
                    .unwrap_or(Vec::new(&env));
                let mut pruned: Vec<Address> = Vec::new(&env);
                for t in list.iter() {
                    if t != token {
                        pruned.push_back(t);
                    }
                }
                env.storage().persistent().set(&DataKey::TokenList, &pruned);
                env.events().publish(
                    (Symbol::new(&env, "token_removed"), token.clone()),
                    TokenRemoved { token },
                );
            }
            multisig::AdminAction::SetFeeRate(rate) => {
                let old_rate: u32 = env.storage().instance().get(&StorageKey::FeeRate).unwrap_or(0);
                env.storage().instance().set(&StorageKey::FeeRate, &rate);
                let pn = Symbol::new(&env, "protocol_fee_rate_bps");
                env.events().publish(
                    (Symbol::new(&env, "parameter_updated"), pn.clone(), caller.clone()),
                    ParameterUpdated {
                        param_name: pn,
                        old_value: old_rate as i128,
                        new_value: rate as i128,
                        updated_by: caller.clone(),
                    },
                );
            }
            multisig::AdminAction::SetMaxDiscount(rate) => {
                let old_rate: u32 = env
                    .storage()
                    .instance()
                    .get(&StorageKey::MaxDiscountRate)
                    .unwrap_or(0);
                env.storage().instance().set(&StorageKey::MaxDiscountRate, &rate);
                let pn = Symbol::new(&env, "max_discount_rate_bps");
                env.events().publish(
                    (Symbol::new(&env, "parameter_updated"), pn.clone(), caller.clone()),
                    ParameterUpdated {
                        param_name: pn,
                        old_value: old_rate as i128,
                        new_value: rate as i128,
                        updated_by: caller.clone(),
                    },
                );
            }
            multisig::AdminAction::UpdateMultisig(new_signers, new_threshold) => {
                if !multisig::is_valid_config(&new_signers, new_threshold) {
                    return Err(ContractError::InvalidMultisigConfig);
                }
                let updated = multisig::MultisigAdmin {
                    signers: new_signers,
                    threshold: new_threshold,
                };
                env.storage().instance().set(&DataKey::MultisigAdmin, &updated);
            }
            multisig::AdminAction::RotateSigner(old_signer, new_signer) => {
                let rotation =
                    multisig::schedule_rotation(&env, old_signer.clone(), new_signer.clone())?;
                env.events().publish(
                    (Symbol::new(&env, "signer_rotation_scheduled"),),
                    SignerRotationScheduled {
                        old_signer,
                        new_signer,
                        effective_at: rotation.effective_at,
                    },
                );
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------
    // upgrade (Issue #48, #539)
    // ------------------------------------------------------------
    /// Upgrade the contract to a new WASM hash.
    ///
    /// Only the admin can trigger an upgrade. The function performs the
    /// actual on-chain WASM replacement via the Soroban deployer API,
    /// records the upgrade in storage, and emits an event for audit.
    ///
    /// # Arguments
    /// - `env`: The Soroban environment
    /// - `new_wasm_hash`: The hash of the new WASM binary to upgrade to (32 bytes)
    ///
    /// # Returns
    /// - `Ok(())` if the upgrade succeeded
    /// - `Err(ContractError)` if called by non-admin
    ///
    /// Access: Admin only
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "upgrade", UPGRADE_COOLDOWN_LEDGERS)?;
        record_admin_action(&env, "upgrade");

        let admin = get_admin(&env).ok_or(ContractError::Unauthorized)?;

        // Issue #539: Actually perform the upgrade via the Soroban deployer.
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());

        env.events().publish(
            (Symbol::new(&env, "upgraded"), admin.clone()),
            ContractUpgraded {
                admin,
                new_wasm_hash,
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(())
    }

    /// Return up to `limit` most recent executed admin actions, newest
    /// first, so admin activity can be reviewed on-chain (e.g. by SCF
    /// reviewers or the community) without replaying the full event log
    /// (Issue #645). Capped at `ADMIN_ACTION_LOG_CAPACITY` entries.
    /// Access: Anyone
    pub fn get_recent_admin_actions(env: Env, limit: u32) -> Vec<AdminActionRecord> {
        access::get_recent_admin_actions(&env, limit)
    }

    // Issue #539: Return the current on-chain storage schema version.
    pub fn get_storage_version(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&crate::storage::DataKey::StorageVersion)
            .unwrap_or(1)
    }

    // Issue #539: Migrate storage from an older schema version to the current
    // version. Can only be called by admin. This allows incremental storage
    // layout changes to be applied atomically after an upgrade.
    pub fn migrate(env: Env) -> Result<u32, ContractError> {
        require_admin(&env)?;
        record_admin_action(&env, "migrate");

        let current: u32 = env
            .storage()
            .instance()
            .get(&crate::storage::DataKey::StorageVersion)
            .unwrap_or(1);

        if current >= crate::constants::CURRENT_STORAGE_VERSION {
            return Ok(current);
        }

        // Add migration steps here as the storage schema evolves.
        // Example for v1 → v2:
        // if current < 2 {
        //     // migrate from v1 to v2
        // }

        env.storage().instance().set(
            &crate::storage::DataKey::StorageVersion,
            &crate::constants::CURRENT_STORAGE_VERSION,
        );

        env.events().publish(
            (Symbol::new(&env, "migrated"),),
            (
                current,
                crate::constants::CURRENT_STORAGE_VERSION,
                env.ledger().timestamp(),
            ),
        );

        Ok(crate::constants::CURRENT_STORAGE_VERSION)
    }

    /// Update the fee tier configuration. Admin or governance only.
    ///
    /// Fee tiers must be sorted by `min_amount` in ascending order. Each tier
    /// specifies a minimum invoice amount (inclusive) and a fee rate in basis
    /// points. The effective fee for an invoice is the fee rate of the first
    /// tier whose `min_amount` is <= the invoice amount.
    ///
    /// An empty list disables tiered fees and falls back to the flat FeeRate.
    /// Access: Admin only
    pub fn update_fee_tiers(env: Env, tiers: Vec<(i128, u32)>) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "update_fee_tiers", ECONOMIC_PARAM_COOLDOWN_LEDGERS)?;
        record_admin_action(&env, "update_fee_tiers");
        let admin = get_admin(&env).ok_or(ContractError::Unauthorized)?;

        env.storage().instance().set(&StorageKey::FeeTiers, &tiers);

        env.events().publish(
            (Symbol::new(&env, "fee_tiers_updated"), admin.clone()),
            tiers.len(),
        );

        Ok(())
    }

    /// Return the configured fee tiers.
    /// Access: Anyone
    pub fn get_fee_tiers(env: Env) -> Vec<(i128, u32)> {
        env.storage()
            .instance()
            .get(&StorageKey::FeeTiers)
            .unwrap_or(Vec::new(&env))
    }

    /// Compute the effective fee rate (in basis points) for a given invoice amount.
    /// Falls back to the flat FeeRate when no tiers are configured.
    fn effective_fee_rate(env: &Env, invoice_amount: i128) -> u32 {
        let tiers: Vec<(i128, u32)> = env
            .storage()
            .instance()
            .get(&StorageKey::FeeTiers)
            .unwrap_or(Vec::new(env));

        if !tiers.is_empty() {
            // Tiers are stored sorted ascending by min_amount.
            // Walk backwards to find the last tier whose min_amount <= invoice_amount.
            let mut best_rate: u32 = 0;
            for i in 0..tiers.len() {
                let (min_amount, rate) = tiers.get(i).unwrap();
                if invoice_amount >= min_amount {
                    best_rate = rate;
                }
            }
            best_rate
        } else {
            env.storage()
                .instance()
                .get(&StorageKey::FeeRate)
                .unwrap_or(0)
        }
    }

    // ------------------------------------------------------------
    // get_contract_stats (read-only view)
    // ------------------------------------------------------------
    /// Access: Anyone
    pub fn get_contract_stats(env: Env) -> ContractStats {
        get_contract_stats(&env)
    }

    // ------------------------------------------------------------
    // get_protocol_status (read-only view — Issue #775)
    // ------------------------------------------------------------
    /// Operationally-relevant protocol state in one call, for public
    /// incident transparency ("is it paused, and why"). Safe to expose
    /// publicly; the indexer mirrors this at `GET /protocol-status`.
    ///
    /// Access: Anyone
    pub fn get_protocol_status(env: Env) -> ProtocolStatus {
        let admin =
            get_admin(&env).unwrap_or_else(|| env.current_contract_address());

        let multisig: Option<multisig::MultisigAdmin> =
            env.storage().instance().get(&DataKey::MultisigAdmin);
        let (multisig_configured, multisig_threshold, multisig_signer_count) = match multisig {
            Some(m) => (true, m.threshold, m.signers.len()),
            None => (false, 0u32, 0u32),
        };

        // Count tripped oracle circuit breakers across every feed type and
        // every allowlisted token. `is_oracle_circuit_tripped` is a plain
        // storage read (sticky flag), so this stays a cheap, non-erroring view.
        let tokens: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::TokenList)
            .unwrap_or_else(|| Vec::new(&env));
        let feed_types = [
            OracleFeedType::Price,
            OracleFeedType::Identity,
            OracleFeedType::Credit,
        ];
        let mut oracle_circuits_tripped: u32 = 0;
        for feed_type in feed_types.iter() {
            for token in tokens.iter() {
                if oracle_registry::is_oracle_circuit_tripped(&env, *feed_type, &token) {
                    oracle_circuits_tripped += 1;
                }
            }
        }

        ProtocolStatus {
            paused: is_paused(&env),
            last_pause_timestamp: get_last_pause_timestamp(&env),
            admin,
            multisig_configured,
            multisig_threshold,
            multisig_signer_count,
            oracle_circuit_tripped: oracle_circuits_tripped > 0,
            oracle_circuits_tripped,
        }
    }

    // ------------------------------------------------------------
    // list_invoices_by_submitter (Paginated)
    // ------------------------------------------------------------
    /// Access: Anyone
    pub fn list_invoices_by_submitter(
        env: Env,
        submitter: Address,
        page: u32,
        page_size: u32,
    ) -> Vec<Invoice> {
        let page_size = page_size.min(50);
        let invoice_ids = get_submitter_invoices(&env, &submitter);
        let total_invoices = invoice_ids.len();

        let start = page.saturating_mul(page_size);
        if start >= total_invoices {
            return Vec::new(&env);
        }

        let end = start.saturating_add(page_size).min(total_invoices);
        let mut result = Vec::new(&env);

        for i in start..end {
            if let Some(id) = invoice_ids.get(i) {
                result.push_back(load_invoice(&env, id));
            }
        }

        result
    }

    // ------------------------------------------------------------
    // list_invoices_by_lp (Paginated)
    // ------------------------------------------------------------
    /// Access: Anyone
    pub fn list_invoices_by_lp(env: Env, lp: Address, page: u32, page_size: u32) -> Vec<Invoice> {
        let page_size = page_size.min(50);
        let invoice_ids = get_lp_invoices(&env, &lp);
        let total_invoices = invoice_ids.len();

        let start = page.saturating_mul(page_size);
        if start >= total_invoices {
            return Vec::new(&env);
        }

        let end = start.saturating_add(page_size).min(total_invoices);
        let mut result = Vec::new(&env);

        for i in start..end {
            if let Some(id) = invoice_ids.get(i) {
                result.push_back(load_invoice(&env, id));
            }
        }

        result
    }

    // ------------------------------------------------------------
    // submit_invoice (NOW TOKEN-AWARE)
    // ------------------------------------------------------------
    /// Access: Submitter only
    pub fn submit_invoice(
        env: Env,
        freelancer: Address,
        payer: Address,
        amount: i128,
        due_date: u64,
        discount_rate: u32,
        token: Address,
        referral_code: ReferralCode,
    ) -> Result<u64, ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        require_submitter(&env, &freelancer)?;

        if freelancer == payer {
            return Err(ContractError::SelfInvoice);
        }

        if discount_rate == 0 || discount_rate > crate::constants::MAX_DISCOUNT_RATE {
            return Err(ContractError::InvalidDiscountRate);
        }

        // Issue #655: staged mainnet rollout — reject invoices above the
        // governance-configured per-invoice cap. 0 = uncapped (default).
        let max_invoice_amount = get_max_invoice_amount(&env);
        if max_invoice_amount > 0 && amount > max_invoice_amount {
            return Err(ContractError::MaxInvoiceAmountExceeded);
        }

        validate_invoice_terms(&env, amount, due_date, discount_rate)?;

        // token validation
        if !is_approved_token(&env, &token) {
            return Err(ContractError::Unauthorized);
        }

        // Re-validate amount using token-aware decimal precision now that we
        // know the token is on the allowlist (and therefore has decimals stored).
        validate_invoice_terms_with_token(&env, amount, due_date, discount_rate, &token)?;

        let id = next_invoice_id(&env)?;

        // Capture the freelancer's reputation score at submission time
        let submitter_reputation = get_payer_score(&env, &freelancer);

        let invoice = Invoice {
            id,
            freelancer: freelancer.clone(),
            payer,
            token,
            amount,
            due_date: due_date.try_into().unwrap(),
            discount_rate,
            status: InvoiceStatus::Pending,
            funder: None,
            funded_at: None,
            amount_funded: 0,
            amount_paid: 0,
            referral_code: referral_code.clone(),
            submitter_reputation,
        };

        save_invoice(&env, &invoice);

        // Update submitter index
        add_invoice_to_submitter(&env, &freelancer, id);

        // Increment total invoices counter
        increment_total_invoices(&env);

        // Increment detailed reputation invoices_submitted count
        increment_invoices_submitted(&env, &freelancer);

        env.events().publish(
            (
                Symbol::new(&env, "submitted"),
                invoice.id,
                invoice.freelancer.clone(),
                invoice.payer.clone(),
            ),
            InvoiceSubmitted {
                invoice_id: invoice.id,
                freelancer: invoice.freelancer.clone(),
                payer: invoice.payer.clone(),
                token: invoice.token.clone(),
                amount: invoice.amount,
                due_date: u64::from(invoice.due_date),
                discount_rate: invoice.discount_rate,
                referral_code: referral_code.clone(),
                status: invoice.status.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        // Track referral count if provided
        if let ReferralCode::Present(code) = &referral_code {
            let key = crate::storage::DataKey::ReferralCount(code.clone());
            let current: u64 = env.storage().persistent().get(&key).unwrap_or(0);
            env.storage().persistent().set(&key, &(current + 1));
        }

        Ok(id)
    }

    // ------------------------------------------------------------
    // update_invoice
    // ------------------------------------------------------------
    /// Access: Submitter only
    pub fn update_invoice(
        env: Env,
        freelancer: Address,
        invoice_id: u64,
        amount: i128,
        due_date: u64,
        discount_rate: u32,
    ) -> Result<(), ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);
        require_submitter_by_id(&env, &freelancer, invoice_id)?;

        if invoice.status == InvoiceStatus::Pending
            && env.ledger().timestamp() >= u64::from(invoice.due_date)
        {
            invoice.status = InvoiceStatus::Expired;
            save_invoice(&env, &invoice);
            return Err(ContractError::InvoiceExpired);
        }

        match invoice.status {
            InvoiceStatus::Pending => {}
            InvoiceStatus::PartiallyFunded | InvoiceStatus::Funded => {
                return Err(ContractError::AlreadyFunded)
            }
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        validate_invoice_terms(&env, amount, due_date, discount_rate)?;

        // Issue #489: re-validate with token-aware minimum so updating
        // cannot push the amount below the token-specific floor (e.g. XLM).
        validate_invoice_terms_with_token(&env, amount, due_date, discount_rate, &invoice.token)?;

        invoice.amount = amount;
        invoice.due_date = due_date.try_into().unwrap();
        invoice.discount_rate = discount_rate;

        save_invoice(&env, &invoice);

        env.events().publish(
            (
                Symbol::new(&env, "updated"),
                invoice.id,
                invoice.freelancer.clone(),
                invoice.payer.clone(),
            ),
            InvoiceUpdated {
                invoice_id: invoice.id,
                freelancer: invoice.freelancer.clone(),
                payer: invoice.payer.clone(),
                token: invoice.token.clone(),
                amount: invoice.amount,
                due_date: u64::from(invoice.due_date),
                discount_rate: invoice.discount_rate,
                status: invoice.status.clone(),
            },
        );

        Ok(())
    }

    // ------------------------------------------------------------
    // convert_invoice_token
    // ------------------------------------------------------------
    /// Access: Submitter only
    pub fn convert_invoice_token(
        env: Env,
        freelancer: Address,
        invoice_id: u64,
        new_token: Address,
    ) -> Result<(), ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);
        require_submitter_by_id(&env, &freelancer, invoice_id)?;

        // Only allowed in Pending state
        if invoice.status != InvoiceStatus::Pending {
            match invoice.status {
                InvoiceStatus::PartiallyFunded | InvoiceStatus::Funded => {
                    return Err(ContractError::AlreadyFunded)
                }
                InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
                _ => return Err(ContractError::Unauthorized), // Generic unauthorized for other states
            }
        }

        // Check if invoice is expired (mirroring update_invoice logic)
        if env.ledger().timestamp() >= u64::from(invoice.due_date) {
            invoice.status = InvoiceStatus::Expired;
            save_invoice(&env, &invoice);
            return Err(ContractError::InvoiceExpired);
        }

        // New token must be in the allowlist
        if !is_approved_token(&env, &new_token) {
            return Err(ContractError::Unauthorized);
        }

        let old_token = invoice.token.clone();
        invoice.token = new_token.clone();

        save_invoice(&env, &invoice);

        env.events().publish(
            (Symbol::new(&env, "token_changed"), invoice_id),
            InvoiceTokenChanged {
                invoice_id,
                old_token,
                new_token,
            },
        );

        Ok(())
    }

    // ------------------------------------------------------------
    // submit_invoices_batch
    // ------------------------------------------------------------
    /// Access: Submitter only
    pub fn submit_invoices_batch(
        env: Env,
        invoices: Vec<InvoiceParams>,
    ) -> Result<Vec<u64>, ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if invoices.len() > 10 {
            return Err(ContractError::BatchTooLarge);
        }

        let mut authenticated_freelancers: Vec<Address> = Vec::new(&env);
        let mut ids = Vec::new(&env);
        for params in invoices.iter() {
            if !authenticated_freelancers.contains(&params.freelancer) {
                require_submitter(&env, &params.freelancer)?;
                authenticated_freelancers.push_back(params.freelancer.clone());
            }

            validate_invoice_terms(&env, params.amount, params.due_date, params.discount_rate)?;

            if !is_approved_token(&env, &params.token) {
                return Err(ContractError::Unauthorized);
            }

            // Re-validate with token-aware decimal precision.
            validate_invoice_terms_with_token(
                &env,
                params.amount,
                params.due_date,
                params.discount_rate,
                &params.token,
            )?;

            let id = next_invoice_id(&env)?;

            // Capture the freelancer's reputation score at submission time
            let submitter_reputation = get_payer_score(&env, &params.freelancer);

            let invoice = Invoice {
                id,
                freelancer: params.freelancer.clone(),
                payer: params.payer,
                token: params.token,
                amount: params.amount,
                due_date: params.due_date.try_into().unwrap(),
                discount_rate: params.discount_rate,
                status: InvoiceStatus::Pending,
                funder: None,
                funded_at: None,
                amount_funded: 0,
                amount_paid: 0,
                referral_code: params.referral_code.clone(),
                submitter_reputation,
            };

            save_invoice(&env, &invoice);

            // Update submitter index
            add_invoice_to_submitter(&env, &params.freelancer, id);

            // Increment total invoices counter
            increment_total_invoices(&env);

            // Increment detailed reputation invoices_submitted count
            // (mirrors the same call in submit_invoice so batch submission
            // does not unfairly penalise high-volume freelancers).
            increment_invoices_submitted(&env, &params.freelancer);

            env.events().publish(
                (
                    Symbol::new(&env, "submitted"),
                    invoice.id,
                    invoice.freelancer.clone(),
                    invoice.payer.clone(),
                ),
                InvoiceSubmitted {
                    invoice_id: invoice.id,
                    freelancer: invoice.freelancer.clone(),
                    payer: invoice.payer.clone(),
                    token: invoice.token.clone(),
                    amount: invoice.amount,
                    due_date: u64::from(invoice.due_date),
                    discount_rate: invoice.discount_rate,
                    referral_code: params.referral_code.clone(),
                    status: invoice.status.clone(),
                    timestamp: env.ledger().timestamp(),
                },
            );

            // Track referral count if provided
            if let ReferralCode::Present(code) = &params.referral_code {
                let key = crate::storage::DataKey::ReferralCount(code.clone());
                let current: u64 = env.storage().persistent().get(&key).unwrap_or(0);
                env.storage().persistent().set(&key, &(current + 1));
            }

            ids.push_back(id);
        }

        Ok(ids)
    }

    /// Access: Anyone
    pub fn get_referral_stats(env: Env, code: BytesN<32>) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::ReferralCount(code))
            .unwrap_or(0)
    }

    // ================================================================
    // Issue #34: LP Priority Queue
    //
    // Design:
    //  1. Any LP calls `join_fund_queue(lp, invoice_id)` to register intent.
    //     Their current LP reputation score is snapshotted.
    //  2. Anyone can call `resolve_fund_queue(invoice_id)` to lock in the
    //     highest-score LP as the approved funder.
    //  3. `fund_invoice` checks: if a QueueResolution exists for this invoice,
    //     only the approved LP may fund it.
    //  If no LP ever joins the queue the existing first-come-first-served
    //  behaviour is preserved unchanged.
    // ================================================================

    /// Register an LP's intent to fund an invoice.
    /// The LP's current reputation score is snapshotted for ordering.
    /// Queue is kept sorted by score (descending) for O(1) resolution.
    /// Access: LP only
    pub fn join_fund_queue(env: Env, lp: Address, invoice_id: u64) -> Result<(), ContractError> {
        require_lp(&env, &lp)?;

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        // Queue resolution already happened — too late to join.
        if get_queue_resolution(&env, invoice_id).is_some() {
            return Err(ContractError::NotApprovedFunder);
        }

        let invoice = load_invoice(&env, invoice_id);
        match invoice.status {
            InvoiceStatus::Pending | InvoiceStatus::PartiallyFunded => {}
            InvoiceStatus::Funded => return Err(ContractError::AlreadyFunded),
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        let mut queue = get_fund_queue(&env, invoice_id);

        // Prevent duplicate entries.
        for i in 0..queue.len() {
            if queue.get(i).unwrap().lp == lp {
                return Err(ContractError::AlreadyInQueue);
            }
        }

        let score = get_lp_score(&env, &lp);
        let new_request = LpFundRequest {
            lp: lp.clone(),
            score,
        };

        // Insert in sorted position (descending score).
        // This maintains the invariant: queue[0] always has the highest score.
        let mut insert_pos = queue.len();
        for i in 0..queue.len() {
            if queue.get(i).unwrap().score < score {
                insert_pos = i;
                break;
            }
        }

        queue.insert(insert_pos, new_request);
        save_fund_queue(&env, invoice_id, &queue);

        // MEV mitigation (Issue #MEV-1): record the ledger when the first LP
        // joins so that `resolve_fund_queue` can enforce a minimum maturity
        // delay before locking in the winner.
        try_set_fund_queue_opened_at(&env, invoice_id);

        env.events().publish(
            (Symbol::new(&env, "fund_requested"), invoice_id, lp.clone()),
            FundRequested {
                invoice_id,
                lp,
                score,
            },
        );

        Ok(())
    }

    /// Select the highest-reputation LP from the queue as the approved funder.
    /// Returns the winning LP address.
    /// Can be called by anyone once at least one LP has joined the queue.
    ///
    /// **Optimization Note**: The queue is maintained in sorted order (descending score)
    /// by `join_fund_queue`, so this operation is O(1) — just returning the first element.
    /// Access: Anyone
    pub fn resolve_fund_queue(env: Env, invoice_id: u64) -> Result<Address, ContractError> {
        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        // Already resolved.
        if let Some(approved) = get_queue_resolution(&env, invoice_id) {
            return Ok(approved);
        }

        let queue = get_fund_queue(&env, invoice_id);
        if queue.is_empty() {
            return Err(ContractError::NotFunded); // no one in queue
        }

        // MEV mitigation (Issue #MEV-1): enforce a minimum maturity delay so
        // that all LPs have a fair window to join before the winner is locked.
        // The delay is measured in ledger sequences (not timestamps) because
        // ledger sequence is monotonically increasing and cannot be manipulated.
        if let Some(opened_at) = get_fund_queue_opened_at(&env, invoice_id) {
            let current = env.ledger().sequence();
            if current < opened_at.saturating_add(QUEUE_DELAY_LEDGERS) {
                // Emit an attempt event so off-chain monitors can detect MEV
                // probing even on rejected calls.
                env.events().publish(
                    (Symbol::new(&env, "queue_resolve_attempt"), invoice_id),
                    FundQueueResolutionAttempted {
                        invoice_id,
                        caller_ledger: opened_at,
                        attempted_at_ledger: current,
                        success: false,
                    },
                );
                return Err(ContractError::QueueNotMature);
            }
        }

        // Queue is sorted by score (descending), so the highest score is at
        // index 0 — but join_fund_queue's insertion (`score < new_score`,
        // strictly less-than) breaks ties by join order, meaning the LP who
        // joins first among equal-reputation competitors always wins
        // deterministically. That's the gameable ordering the threat model
        // (docs/threat-model.md, "Add Randomness to Queue Tie-Breaking")
        // flagged as an open recommendation (Issue #708): a would-be LP with
        // knowledge of another LP's equal reputation gains nothing from
        // reputation, only from being first to submit.
        //
        // All entries tied for the top score form a contiguous prefix
        // (queue[0..tied_count]), since the queue is sorted by score alone.
        // When there's more than one, pick uniformly among them using
        // Soroban's network-seeded PRNG instead of always taking index 0.
        let best_score = queue.get(0).unwrap().score;
        let mut tied_count: u32 = 1;
        while tied_count < queue.len() && queue.get(tied_count).unwrap().score == best_score {
            tied_count += 1;
        }
        let winner_index: u32 = if tied_count > 1 {
            // GenRange is only implemented for u64 in this soroban-sdk
            // version — generate as u64, then narrow (safe: tied_count is a
            // small queue length, well within u32 range).
            env.prng().gen_range::<u64>(0..u64::from(tied_count)) as u32
        } else {
            0
        };
        let best_entry = queue.get(winner_index).unwrap();
        let best_lp = best_entry.lp.clone();

        save_queue_resolution(&env, invoice_id, &best_lp);

        // Emit resolution attempt event (successful).
        let current_ledger = env.ledger().sequence();
        env.events().publish(
            (Symbol::new(&env, "queue_resolve_attempt"), invoice_id),
            FundQueueResolutionAttempted {
                invoice_id,
                caller_ledger: get_fund_queue_opened_at(&env, invoice_id).unwrap_or(0),
                attempted_at_ledger: current_ledger,
                success: true,
            },
        );

        env.events().publish(
            (
                Symbol::new(&env, "fund_queue_resolved"),
                invoice_id,
                best_lp.clone(),
            ),
            FundQueueResolved {
                invoice_id,
                approved_lp: best_lp.clone(),
                score: best_score,
            },
        );

        Ok(best_lp)
    }

    // ------------------------------------------------------------
    // fund_invoice (USES invoice.token) — now queue-aware
    // ------------------------------------------------------------
    /// Access: LP only
    ///
    /// `require_oracle_verification` — when `true`, the oracle stored in
    /// contract config is queried for the payer's verification status.
    /// If the oracle returns `false` (unverified), the call returns
    /// `ContractError::PayerUnverified`. When `false`, the oracle is not
    /// consulted and the existing behaviour is preserved.
    pub fn fund_invoice(
        env: Env,
        funder: Address,
        invoice_id: u64,
        fund_amount: i128,
        require_oracle_verification: bool,
    ) -> Result<(), ContractError> {
        lock_reentrancy(&env)?;

        if is_paused(&env) {
            unlock_reentrancy(&env);
            return Err(ContractError::ContractPaused);
        }

        require_lp(&env, &funder)?;

        // Issue #71: load the invoice once instead of `invoice_exists` + `load_invoice`
        // (which read the same persistent key twice on the hottest path).
        let mut invoice =
            try_load_invoice(&env, invoice_id).ok_or(ContractError::InvoiceNotFound)?;

        // ── Issue #34: priority queue check ──────────────────────
        // If a queue has been resolved, only the approved LP may fund.
        if let Some(approved) = get_queue_resolution(&env, invoice_id) {
            if approved != funder {
                return Err(ContractError::NotApprovedFunder);
            }
        }

        // Issue #19: the invoice token must still be on the governance allowlist.
        if !is_approved_token(&env, &invoice.token) {
            return Err(ContractError::Unauthorized);
        }

        // Issue #28: reject funding when the payer's reputation is below the
        // configured minimum threshold (default 0 allows everyone).
        let min_payer_reputation = get_min_payer_reputation(&env);
        if min_payer_reputation > 0 && get_payer_score(&env, &invoice.payer) < min_payer_reputation
        {
            return Err(ContractError::PayerReputationTooLow);
        }

        // Issues #92 + #93 + #532 + circuit-breaker: optional oracle
        // verification with a data-freshness guard. When
        // require_oracle_verification is true, the oracle registry is
        // queried for the Identity feed, resolved per the invoice's token
        // (per-token override, then feed-type default, then the legacy
        // price_oracle config field) — skipping any level that's
        // circuit-tripped from repeated staleness. If nothing was ever
        // registered, the flag is a no-op (existing fail-open behavior). If
        // everything registered is circuit-tripped, funding is rejected
        // rather than silently proceeding as if no oracle existed.
        if require_oracle_verification {
            let resolution = oracle_registry::resolve_oracle_for_verification(
                &env,
                OracleFeedType::Identity,
                &invoice.token,
            );
            if resolution == oracle_registry::OracleResolution::CircuitOpen {
                return Err(ContractError::OracleCircuitOpen);
            }
            if let oracle_registry::OracleResolution::Available(oracle_addr) = resolution {
                let response: OracleVerificationResponse = env.invoke_contract(
                    &oracle_addr,
                    &Symbol::new(&env, "get_payer_data"),
                    vec![&env, invoice.payer.clone().into_val(&env)],
                );

                // Issue #93: reject stale oracle data.
                // Staleness = current_ledger_sequence - oracle.timestamp >= max_oracle_age_ledgers.
                // If max_oracle_age_ledgers == 0 the check is disabled (governance escape hatch).
                let max_age = crate::storage::get_config(&env)
                    .map(|c| c.max_oracle_age_ledgers)
                    .unwrap_or(DEFAULT_MAX_ORACLE_AGE_LEDGERS);

                // Issue #532: record health (staleness) for this oracle
                // regardless of outcome, so monitoring sees every query.
                oracle_registry::record_oracle_health(
                    &env,
                    OracleFeedType::Identity,
                    &invoice.token,
                    &oracle_addr,
                    response.timestamp,
                    max_age,
                );

                if max_age > 0 {
                    let current_ledger = env.ledger().sequence() as u64;
                    let age = current_ledger.saturating_sub(response.timestamp as u64);
                    if age >= max_age {
                        return Err(ContractError::OracleDataStale);
                    }
                }

                // Issue #92: reject unverified payers.
                if !response.is_verified {
                    return Err(ContractError::PayerUnverified);
                }
            }
        }

        if invoice.status == InvoiceStatus::Pending
            && env.ledger().timestamp() > u64::from(invoice.due_date)
        {
            invoice.status = InvoiceStatus::Expired;
            save_invoice(&env, &invoice);
            env.events().publish(
                (Symbol::new(&env, "expired"), invoice.id),
                InvoiceExpired {
                    invoice_id: invoice.id,
                    freelancer: invoice.freelancer.clone(),
                    status: invoice.status.clone(),
                },
            );
            return Err(ContractError::InvoiceExpired);
        }

        match invoice.status {
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Funded => return Err(ContractError::AlreadyFunded),
            InvoiceStatus::Pending | InvoiceStatus::PartiallyFunded => {} // all good
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        let prospective_funded = invoice
            .amount_funded
            .checked_add(fund_amount)
            .ok_or(ContractError::ArithmeticOverflow)?;
        if prospective_funded > invoice.amount {
            return Err(ContractError::OverfundingRejected);
        }

        // Issue #655: staged mainnet rollout — reject funding that would
        // push this token's cumulative funded volume past the
        // governance-configured cap (0 = uncapped). Checked against the
        // same `TokenVolume` counter `add_volume` maintains below, so
        // raising the cap via governance takes effect immediately without
        // any separate migration.
        let token_volume_cap = get_token_volume_cap(&env, &invoice.token);
        if token_volume_cap > 0 {
            let prospective_volume = get_token_volume(&env, &invoice.token)
                .checked_add(fund_amount)
                .ok_or(ContractError::ArithmeticOverflow)?;
            if prospective_volume > token_volume_cap {
                return Err(ContractError::GlobalVolumeCapExceeded);
            }
        }

        // --- Execute transfer ---
        let token = token_client(&env, &invoice.token);
        let contract_address = env.current_contract_address();

        // Handle token precision if needed
        let normalized_fund_amount = if is_xlm_token(&env, &invoice.token) {
            normalize_xlm_amount(fund_amount)
        } else if is_eurc_token(&env, &invoice.token) {
            normalize_eurc_amount(fund_amount)
        } else {
            normalize_usdc_amount(fund_amount)
        };

        let fund_discount = normalized_fund_amount
            .checked_mul(discount_rate_as_i128(invoice.discount_rate))
            .unwrap_or(0)
            / 10_000;
        let cost = normalized_fund_amount.saturating_sub(fund_discount);

        token.transfer(&funder, &contract_address, &cost);

        // --- Update contributor list ---
        let mut funders = get_invoice_funders(&env, invoice_id);
        let mut found = false;
        for i in 0..funders.len() {
            let (addr, amt) = funders.get(i).unwrap();
            if addr == funder {
                funders.set(i, (addr, amt.saturating_add(fund_amount)));
                found = true;
                break;
            }
        }
        if !found {
            funders.push_back((funder.clone(), fund_amount));
        }
        save_invoice_funders(&env, invoice_id, &funders);

        // --- Update invoice state ---
        invoice.amount_funded = prospective_funded;

        if invoice.amount_funded == invoice.amount {
            // Fully funded — pay out to freelancer
            let discount_amount = invoice
                .amount
                .checked_mul(discount_rate_as_i128(invoice.discount_rate))
                .unwrap_or(0)
                / 10_000;
            let freelancer_payout = invoice.amount.saturating_sub(discount_amount);

            token.transfer(&contract_address, &invoice.freelancer, &freelancer_payout);

            invoice.status = InvoiceStatus::Funded;
            invoice.funded_at = Some(env.ledger().timestamp().try_into().unwrap());
            invoice.funder = Some(funder.clone());

            // Boost LP score on successful funding
            let current_lp_score = get_lp_score(&env, &funder);
            set_lp_score(&env, &funder, current_lp_score.saturating_add(1));
        } else {
            invoice.status = InvoiceStatus::PartiallyFunded;
        }

        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        // Update LP index
        add_invoice_to_lp(&env, &funder, invoice_id);

        // Increment total funded counter if fully funded
        if invoice.status == InvoiceStatus::Funded {
            increment_total_funded(&env);
        }

        add_volume(&env, &invoice.token, fund_amount);

        notify_distribution_funding(&env, &funder, fund_amount);

        let now = env.ledger().timestamp();

        let seconds_to_due = if u64::from(invoice.due_date) > now {
            u64::from(invoice.due_date) - now
        } else {
            0
        };

        let days_to_due = seconds_to_due / (24 * 60 * 60);

        let effective_yield_bps = ((invoice.discount_rate as u64 * days_to_due) / 365) as u32;

        env.events().publish(
            (Symbol::new(&env, "funded"), invoice.id, funder.clone()),
            InvoiceFunded {
                invoice_id: invoice.id,
                funder: funder.clone(),
                freelancer: invoice.freelancer.clone(),
                payer: invoice.payer.clone(),
                token: invoice.token.clone(),
                fund_amount,
                amount_funded: invoice.amount_funded,
                invoice_amount: invoice.amount,
                due_date: u64::from(invoice.due_date),
                discount_rate: invoice.discount_rate,
                funded_at: invoice.funded_at.map(|ts| ts.into()),
                status: invoice.status.clone(),

                // NEW
                lp: funder.clone(),
                effective_yield_bps,
                timestamp: now,
            },
        );

        unlock_reentrancy(&env);
        Ok(())
    }

    // ------------------------------------------------------------
    // transfer_invoice
    // ------------------------------------------------------------
    /// Access: Submitter only
    pub fn transfer_invoice(
        env: Env,
        invoice_id: u64,
        new_freelancer: Address,
    ) -> Result<(), ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        require_submitter_by_id(&env, &invoice.freelancer, invoice_id)?;

        match invoice.status {
            InvoiceStatus::Pending => {}
            InvoiceStatus::PartiallyFunded | InvoiceStatus::Funded => {
                return Err(ContractError::AlreadyFunded)
            }
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        let old_freelancer = invoice.freelancer.clone();
        invoice.freelancer = new_freelancer.clone();

        save_invoice(&env, &invoice);

        // Update submitter index
        remove_invoice_from_submitter(&env, &old_freelancer, invoice_id);
        add_invoice_to_submitter(&env, &new_freelancer, invoice_id);

        env.events().publish(
            (Symbol::new(&env, "transferred"), invoice_id),
            InvoiceTransferred {
                invoice_id,
                old_freelancer,
                new_freelancer,
                status: invoice.status.clone(),
            },
        );

        Ok(())
    }

    // ------------------------------------------------------------
    // transfer_lp_position
    /// Access: Current LP only
    pub fn transfer_lp_position(
        env: Env,
        invoice_id: u64,
        new_lp: Address,
    ) -> Result<(), ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);
        match invoice.status {
            InvoiceStatus::Funded => {}
            InvoiceStatus::Pending | InvoiceStatus::PartiallyFunded => {
                return Err(ContractError::NotFunded)
            }
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        let current_lp = invoice.funder.clone().ok_or(ContractError::Unauthorized)?;

        current_lp.require_auth();

        if current_lp == new_lp {
            return Err(ContractError::Unauthorized);
        }

        let mut funders = get_invoice_funders(&env, invoice_id);
        for i in 0..funders.len() {
            let (addr, amt) = funders.get(i).unwrap();
            if addr == current_lp {
                funders.set(i, (new_lp.clone(), amt));
            }
        }
        save_invoice_funders(&env, invoice_id, &funders);

        invoice.funder = Some(new_lp.clone());
        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        remove_invoice_from_lp(&env, &current_lp, invoice_id);
        add_invoice_to_lp(&env, &new_lp, invoice_id);

        env.events().publish(
            (Symbol::new(&env, "lp_position_transferred"), invoice_id),
            LPPositionTransferred {
                invoice_id,
                old_lp: current_lp,
                new_lp,
                status: invoice.status.clone(),
            },
        );

        Ok(())
    }

    // ------------------------------------------------------------
    // cancel_invoice
    // ------------------------------------------------------------
    /// Access: Submitter only
    pub fn cancel_invoice(env: Env, invoice_id: u64) -> Result<(), ContractError> {
        lock_reentrancy(&env)?;

        if is_paused(&env) {
            unlock_reentrancy(&env);
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            unlock_reentrancy(&env);
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        require_submitter_by_id(&env, &invoice.freelancer, invoice_id)?;

        match invoice.status {
            InvoiceStatus::Pending => {}
            InvoiceStatus::PartiallyFunded => {
                let funders = get_invoice_funders(&env, invoice_id);
                // CEI: update state before external token transfers
                invoice.status = InvoiceStatus::Cancelled;
                save_invoice(&env, &invoice);
                let token = token_client(&env, &invoice.token);
                let contract_address = env.current_contract_address();
                for i in 0..funders.len() {
                    let (funder_addr, fund_amt) = funders.get(i).unwrap();
                    let fund_discount = fund_amt
                        .checked_mul(discount_rate_as_i128(invoice.discount_rate))
                        .unwrap_or(0)
                        / 10_000;
                    let refund = fund_amt.saturating_sub(fund_discount);
                    token.transfer(&contract_address, &funder_addr, &refund);
                }
            }
            InvoiceStatus::Funded => return Err(ContractError::AlreadyFunded),
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        invoice.status = InvoiceStatus::Cancelled;

        save_invoice(&env, &invoice);

        env.events().publish(
            (Symbol::new(&env, "cancelled"), invoice_id),
            InvoiceCancelled {
                invoice_id,
                freelancer: invoice.freelancer.clone(),
                status: invoice.status.clone(),
            },
        );

        unlock_reentrancy(&env);
        Ok(())
    }

    // ------------------------------------------------------------
    // expire_invoice
    // ------------------------------------------------------------
    /// Access: Anyone
    pub fn expire_invoice(env: Env, invoice_id: u64) -> Result<(), ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        if env.ledger().timestamp() <= u64::from(invoice.due_date) {
            return Err(ContractError::NotYetDefaulted);
        }

        match invoice.status {
            InvoiceStatus::Pending => {
                invoice.status = InvoiceStatus::Expired;
                save_invoice(&env, &invoice);
                env.events().publish(
                    (Symbol::new(&env, "expired"), invoice.id),
                    InvoiceExpired {
                        invoice_id: invoice.id,
                        freelancer: invoice.freelancer.clone(),
                        status: invoice.status.clone(),
                    },
                );
                Ok(())
            }
            InvoiceStatus::PartiallyFunded | InvoiceStatus::Funded => {
                Err(ContractError::AlreadyFunded)
            }
            InvoiceStatus::Paid => Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => Err(ContractError::AlreadyCancelled),
        }
    }

    // ------------------------------------------------------------
    // mark_paid (USES invoice.token)
    // ------------------------------------------------------------
    /// Access: Payer only
    pub fn mark_paid(env: Env, invoice_id: u64, amount: i128) -> Result<(), ContractError> {
        lock_reentrancy(&env)?;

        if is_paused(&env) {
            unlock_reentrancy(&env);
            return Err(ContractError::ContractPaused);
        }

        if amount <= 0 {
            unlock_reentrancy(&env);
            return Err(ContractError::InvalidAmount);
        }

        // Issue #71: single load instead of `invoice_exists` + `load_invoice`.
        let mut invoice =
            try_load_invoice(&env, invoice_id).ok_or(ContractError::InvoiceNotFound)?;

        require_payer_by_id(&env, invoice_id)?;

        match invoice.status {
            InvoiceStatus::Pending | InvoiceStatus::PartiallyFunded => {
                return Err(ContractError::NotFunded)
            }
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Funded => {}
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        let remaining = invoice.amount.saturating_sub(invoice.amount_paid);
        if amount > remaining {
            return Err(ContractError::OverpaymentRejected);
        }

        let funders = get_invoice_funders(&env, invoice_id);
        if funders.is_empty() {
            return Err(ContractError::NotFunded);
        }

        let token = token_client(&env, &invoice.token);
        let contract_address = env.current_contract_address();

        // Handle token precision if needed
        let normalized_amount = if is_xlm_token(&env, &invoice.token) {
            normalize_xlm_amount(amount)
        } else if is_eurc_token(&env, &invoice.token) {
            normalize_eurc_amount(amount)
        } else {
            normalize_usdc_amount(amount)
        };

        // CEI: state update before external call
        invoice.amount_paid = invoice
            .amount_paid
            .checked_add(amount)
            .ok_or(ContractError::ArithmeticOverflow)?;

        // Payer sends partial/full amount to the contract
        token.transfer(&invoice.payer, &contract_address, &normalized_amount);

        // If not fully paid, save and emit partial event
        if invoice.amount_paid < invoice.amount {
            save_invoice(&env, &invoice);
            crate::nft::sync_nft_state(&env, invoice_id)?;
            env.events().publish(
                (
                    Symbol::new(&env, "partially_paid"),
                    invoice.id,
                    invoice.payer.clone(),
                ),
                InvoicePartiallyPaid {
                    invoice_id: invoice.id,
                    payer: invoice.payer.clone(),
                    amount_paid_now: amount,
                    total_amount_paid: invoice.amount_paid,
                    remaining_amount: invoice.amount.saturating_sub(invoice.amount_paid),
                },
            );
            unlock_reentrancy(&env);
            return Ok(());
        }

        // --- FULL PAYMENT LOGIC ---
        // Calculate protocol fee using tiered fee rate if configured
        let fee_rate = Self::effective_fee_rate(&env, invoice.amount);
        let protocol_fee = invoice.amount.checked_mul(fee_rate as i128).unwrap_or(0) / 10_000;

        if protocol_fee > 0 {
            let admin: Address = env
                .storage()
                .instance()
                .get(&crate::storage::DataKey::Admin)
                .unwrap();
            token.transfer(&contract_address, &admin, &protocol_fee);
        }

        let distribute_amount = invoice.amount.saturating_sub(protocol_fee);

        // Legacy compatibility: use first LP for event emission
        let primary_lp = funders.get(0).unwrap().0.clone();

        // Total amount funded by primary LP
        let primary_lp_funded = funders.get(0).unwrap().1;

        // LP payout after settlement distribution. A genuine multiplication
        // overflow here must surface as an error, not silently collapse to a
        // corrupting zero payout (Issue #619).
        let primary_lp_payout = distribute_amount
            .checked_mul(primary_lp_funded)
            .ok_or(ContractError::ArithmeticOverflow)?
            / invoice.amount;

        // LP earnings. Protocol-fee deduction plus integer-division
        // truncation can make primary_lp_payout fall slightly below
        // primary_lp_funded when distribute_amount is close to
        // invoice.amount — use checked_sub (never a bare `-`) so this can
        // never panic in debug builds or silently wrap to an enormous value
        // in release builds. The LP simply earns zero on that edge, never a
        // negative or wrapped amount (Issue #619).
        let lp_earned = primary_lp_payout
            .checked_sub(primary_lp_funded)
            .unwrap_or(0);

        // CEI: update state before external token transfers
        // CEI: update state before external token transfers
        invoice.status = InvoiceStatus::Paid;

        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        // Distribute proportionally to funders
        for i in 0..funders.len() {
            let (funder_addr, fund_amt) = funders.get(i).unwrap();
            let funder_share =
                distribute_amount.checked_mul(fund_amt).unwrap_or(0) / invoice.amount;
            if funder_share > 0 {
                token.transfer(&contract_address, &funder_addr, &funder_share);
            }
        }

        // Increment total paid counter
        increment_total_paid(&env);

        let paid_on_time = env.ledger().timestamp() <= u64::from(invoice.due_date);
        notify_distribution_settlement(&env, &invoice.freelancer, &invoice.payer, paid_on_time);

        // --- Update payer reputation ---
        let current_score = get_payer_score(&env, &invoice.payer);
        set_payer_score(&env, &invoice.payer, current_score.saturating_add(1));

        // Increment detailed reputation invoices_paid count for both payer and freelancer
        increment_invoices_paid(&env, &invoice.payer);
        increment_invoices_paid(&env, &invoice.freelancer);

        env.events().publish(
            (
                Symbol::new(&env, "paid"),
                invoice.id,
                invoice.payer.clone(),
                primary_lp.clone(),
            ),
            InvoicePaid {
                invoice_id: invoice.id,
                payer: invoice.payer.clone(),
                lp: primary_lp,
                freelancer: invoice.freelancer.clone(),
                token: invoice.token.clone(),
                amount_paid: invoice.amount,
                lp_earned,
                lp_payout: primary_lp_payout,
                settlement_timestamp: env.ledger().timestamp(),
                paid_on_time,
                status: invoice.status.clone(),
            },
        );

        unlock_reentrancy(&env);
        Ok(())
    }

    // ----------------------------------------------------------------
    // claim_yield
    // ----------------------------------------------------------------
    /// Access: LP only
    pub fn claim_yield(env: Env, invoice_id: u64) -> Result<i128, ContractError> {
        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let invoice = load_invoice(&env, invoice_id);

        // Only the funder can query their own yield
        if let Some(ref funder) = invoice.funder {
            require_lp_by_id(&env, funder, invoice_id)?;
        } else {
            return Err(ContractError::NothingToClaim);
        }

        match invoice.status {
            InvoiceStatus::Pending | InvoiceStatus::PartiallyFunded | InvoiceStatus::Funded => {
                Ok(0)
            }
            InvoiceStatus::Defaulted => Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => Err(ContractError::AlreadyCancelled),
            InvoiceStatus::Paid => {
                let yield_amount = invoice
                    .amount
                    .checked_mul(discount_rate_as_i128(invoice.discount_rate))
                    .unwrap_or(0)
                    / 10_000;
                Ok(yield_amount)
            }
        }
    }

    // ----------------------------------------------------------------
    // claim_default
    // ----------------------------------------------------------------
    /// Access: LP only
    pub fn claim_default(env: Env, funder: Address, invoice_id: u64) -> Result<(), ContractError> {
        lock_reentrancy(&env)?;

        if is_paused(&env) {
            unlock_reentrancy(&env);
            return Err(ContractError::ContractPaused);
        }

        require_lp(&env, &funder)?;

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        let funders = get_invoice_funders(&env, invoice_id);
        let mut is_funder = false;
        for i in 0..funders.len() {
            if funders.get(i).unwrap().0 == funder {
                is_funder = true;
                break;
            }
        }

        if !is_funder {
            return Err(ContractError::Unauthorized);
        }

        let now = env.ledger().timestamp();
        if now < u64::from(invoice.due_date) {
            return Err(ContractError::NotYetDefaulted);
        }

        match invoice.status {
            InvoiceStatus::Funded => {}
            InvoiceStatus::Pending | InvoiceStatus::PartiallyFunded => {
                return Err(ContractError::NotFunded)
            }
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Disputed => return Err(ContractError::InvoiceDisputed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
        }

        let token = token_client(&env, &invoice.token);
        let contract_address = env.current_contract_address();

        // CEI: update state before external token transfers
        invoice.status = InvoiceStatus::Defaulted;
        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        let mut total_refunded: i128 = 0;

        for i in 0..funders.len() {
            let (funder_addr, fund_amt) = funders.get(i).unwrap();
            let fund_discount = fund_amt
                .checked_mul(discount_rate_as_i128(invoice.discount_rate))
                .unwrap_or(0)
                / 10_000;
            let refund = fund_amt.saturating_sub(fund_discount);
            token.transfer(&contract_address, &funder_addr, &refund);
            total_refunded = total_refunded.saturating_add(refund);
        }

        // Issue #529: on top of the principal refund above, an LP enrolled in
        // the insurance pool gets an additional payout for this default. The
        // pool itself credits the LP directly (it transfers tokens from its
        // own balance), so this contract only needs to trigger the claim and
        // report the outcome - it never holds or forwards the payout.
        //
        // Handled via try_* so a paused/empty/unreachable pool degrades
        // gracefully: claim_default still completes (refund + status update
        // already happened above, in the same atomic invocation) rather than
        // reverting the whole default over an optional insurance top-up.
        if let Some(pool_addr) = crate::storage::get_insurance_pool(&env) {
            let pool_client = InsurancePoolInterfaceClient::new(&env, &pool_addr);
            let enrolled = matches!(pool_client.try_is_enrolled(&funder), Ok(Ok(true)));
            if enrolled {
                let (compensated, payout) = match pool_client.try_claim(&invoice_id, &funder) {
                    Ok(Ok(payout)) => (true, payout),
                    _ => (false, 0),
                };
                env.events().publish(
                    (
                        Symbol::new(&env, "insurance_claim_attempted"),
                        invoice.id,
                        funder.clone(),
                    ),
                    InsuranceClaimAttempted {
                        invoice_id: invoice.id,
                        lp: funder.clone(),
                        compensated,
                        payout,
                    },
                );
            }
        }

        // --- Update payer reputation ---
        // Snapshot the score BEFORE applying the penalty so appeal_default()
        // can restore it exactly if the appeal is upheld.
        let current_score = get_payer_score(&env, &invoice.payer);
        save_pre_default_payer_score(&env, invoice_id, current_score);

        if current_score > 5 {
            set_payer_score(&env, &invoice.payer, current_score - 5);
        } else {
            set_payer_score(&env, &invoice.payer, 0);
        }

        // Increment detailed reputation invoices_defaulted count for the payer
        increment_invoices_defaulted(&env, &invoice.payer);

        env.events().publish(
            (Symbol::new(&env, "defaulted"), invoice.id, funder.clone()),
            InvoiceDefaulted {
                invoice_id: invoice.id,
                funder,
                freelancer: invoice.freelancer.clone(),
                payer: invoice.payer.clone(),
                token: invoice.token.clone(),
                amount: invoice.amount,
                due_date: u64::from(invoice.due_date),
                defaulted_at: now,
                discount_amount: total_refunded,
                status: invoice.status.clone(),
            },
        );

        unlock_reentrancy(&env);
        Ok(())
    }

    // ================================================================
    // Issue #36: appeal_default — payer contests an unfair default
    //
    // Flow:
    //   1. Payer calls `appeal_default(invoice_id, evidence_hash)`.
    //   2. Invoice transitions to `Appealed` status.
    //   3. Admin/governance calls `resolve_appeal(invoice_id, upheld)`.
    //      - upheld=true  → default reversed, score restored.
    //      - upheld=false → invoice remains Defaulted.
    // ================================================================

    /// File an appeal against an unfair default marking.
    ///
    /// * `invoice_id`    – the defaulted invoice
    /// * `evidence_hash` – SHA-256 hash of off-chain evidence provided by the payer
    /// Access: Payer only
    pub fn appeal_default(
        env: Env,
        invoice_id: u64,
        evidence_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        // Only the payer may appeal.
        require_payer_by_id(&env, invoice_id)?;

        // Check AlreadyAppealed BEFORE status check: after the first appeal the
        // status is `Appealed` (not `Defaulted`), so the status guard would fire
        // with the wrong error code if checked first.
        if get_appeal(&env, invoice_id).is_some() {
            return Err(ContractError::AlreadyAppealed);
        }

        // Invoice must be in Defaulted state.
        if invoice.status != InvoiceStatus::Defaulted {
            return Err(ContractError::NotDefaulted);
        }

        let now = env.ledger().timestamp();

        // Appeal must be filed within the appeal window after default.
        // A default can only occur after due_date, so we measure from due_date.
        if now > u64::from(invoice.due_date).saturating_add(APPEAL_WINDOW_SECONDS) {
            return Err(ContractError::AppealWindowClosed);
        }

        // Use the pre-default score snapshot saved by claim_default().
        // Fall back to the current score if somehow missing (shouldn't happen).
        let pre_default_score = get_pre_default_payer_score(&env, invoice_id)
            .unwrap_or_else(|| get_payer_score(&env, &invoice.payer));

        save_appeal(
            &env,
            invoice_id,
            &AppealRecord {
                evidence_hash: evidence_hash.clone(),
                appealed_at: now.try_into().unwrap(),
                pre_default_score,
            },
        );

        invoice.status = InvoiceStatus::Appealed;
        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        env.events().publish(
            (
                Symbol::new(&env, "default_appealed"),
                invoice_id,
                invoice.payer.clone(),
            ),
            DefaultAppealed {
                invoice_id,
                payer: invoice.payer.clone(),
                evidence_hash,
                appealed_at: now,
            },
        );

        Ok(())
    }

    /// Resolve a pending appeal (admin / governance only).
    ///
    /// * `upheld=true`  → reverse the default, restore pre-default score, status → Defaulted (reversed).
    ///   In practice the status transitions back to Defaulted with score restored so the LP
    ///   can still collect principal they were already refunded. The key effect is reputation repair.
    /// * `upheld=false` → reject the appeal; invoice remains Defaulted (status reverts from Appealed).
    /// Access: Admin only
    pub fn resolve_appeal(env: Env, invoice_id: u64, upheld: bool) -> Result<(), ContractError> {
        require_admin(&env)?;
        record_admin_action(&env, "resolve_appeal");

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        if invoice.status != InvoiceStatus::Appealed {
            return Err(ContractError::NotDefaulted);
        }

        let appeal = get_appeal(&env, invoice_id).ok_or(ContractError::InvoiceNotFound)?;

        let now = env.ledger().timestamp();

        if upheld {
            // Restore the payer's reputation to what it was before the default.
            set_payer_score(&env, &invoice.payer, appeal.pre_default_score);

            // Decrement invoices_defaulted count since the default was reversed
            let mut profile = get_reputation(&env, &invoice.payer);
            profile.invoices_defaulted = profile.invoices_defaulted.saturating_sub(1);
            set_reputation(&env, &profile);

            // Status moves back to Defaulted — the LP still received their refund,
            // but the reputational penalty on the payer is reversed.
            invoice.status = InvoiceStatus::Defaulted;
        } else {
            // Appeal rejected; mark as Defaulted again (was temporarily Appealed).
            invoice.status = InvoiceStatus::Defaulted;
        }

        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        env.events().publish(
            (
                Symbol::new(&env, "appeal_resolved"),
                invoice_id,
                invoice.payer.clone(),
            ),
            AppealResolved {
                invoice_id,
                payer: invoice.payer.clone(),
                upheld,
                resolved_at: now,
            },
        );

        Ok(())
    }

    // ================================================================
    // Dispute Mechanism — payer raised disputes before settlement
    // ================================================================

    /// Dispute an invoice before settlement.
    ///
    /// * `invoice_id`  – the invoice to dispute
    /// * `reason_hash` – SHA-256 hash of off-chain dispute evidence
    /// Access: Payer only
    pub fn dispute_invoice(
        env: Env,
        invoice_id: u64,
        reason_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        if is_paused(&env) {
            return Err(ContractError::ContractPaused);
        }

        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        // Only the payer may dispute.
        require_payer_by_id(&env, invoice_id)?;

        // Check if already disputed.
        if get_dispute(&env, invoice_id).is_some() {
            return Err(ContractError::AlreadyDisputed);
        }

        // Only Pending, PartiallyFunded or Funded invoices can be disputed (before settlement).
        match invoice.status {
            InvoiceStatus::Pending | InvoiceStatus::PartiallyFunded | InvoiceStatus::Funded => {}
            InvoiceStatus::Paid => return Err(ContractError::AlreadyPaid),
            InvoiceStatus::Defaulted => return Err(ContractError::InvoiceDefaulted),
            InvoiceStatus::Appealed => return Err(ContractError::InvoiceAppealed),
            InvoiceStatus::Expired => return Err(ContractError::InvoiceExpired),
            InvoiceStatus::Cancelled => return Err(ContractError::AlreadyCancelled),
            InvoiceStatus::Disputed => return Err(ContractError::AlreadyDisputed),
        }

        let now_ts = env.ledger().timestamp();
        let now_ledger = env.ledger().sequence();

        save_dispute(
            &env,
            invoice_id,
            &DisputeRecord {
                reason_hash: reason_hash.clone(),
                disputed_at: now_ledger,
            },
        );

        invoice.status = InvoiceStatus::Disputed;
        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        env.events().publish(
            (
                Symbol::new(&env, "disputed"),
                invoice_id,
                invoice.payer.clone(),
            ),
            InvoiceDisputed {
                invoice_id,
                payer: invoice.payer.clone(),
                reason_hash,
                disputed_at: now_ts,
            },
        );

        Ok(())
    }

    /// Resolve a dispute (admin / governance only).
    ///
    /// * `resolution_hash` – Optional hash of resolution details
    /// * `resolution`      – Ruling: 1 = Upheld (Payer right), 2 = Rejected (Freelancer right)
    /// Access: Admin only
    pub fn resolve_dispute(
        env: Env,
        invoice_id: u64,
        resolution_hash: BytesN<32>,
        resolution: u32,
    ) -> Result<(), ContractError> {
        lock_reentrancy(&env)?;

        require_admin(&env)?;
        record_admin_action(&env, "resolve_dispute");

        if !invoice_exists(&env, invoice_id) {
            unlock_reentrancy(&env);
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        if invoice.status != InvoiceStatus::Disputed {
            return Err(ContractError::NotDisputed);
        }

        match resolution {
            1 => {
                // Upheld: Payer is right.
                // CEI: update state before external token transfers
                invoice.status = InvoiceStatus::Cancelled;
                save_invoice(&env, &invoice);

                let token = token_client(&env, &invoice.token);
                let contract_address = env.current_contract_address();

                // Refund LPs if it was funded.
                let funders = get_invoice_funders(&env, invoice_id);
                if !funders.is_empty() {
                    for i in 0..funders.len() {
                        let (funder_addr, fund_amt) = funders.get(i).unwrap();
                        let fund_discount = fund_amt
                            .checked_mul(discount_rate_as_i128(invoice.discount_rate))
                            .unwrap_or(0)
                            / 10_000;
                        let refund = fund_amt.saturating_sub(fund_discount);
                        token.transfer(&contract_address, &funder_addr, &refund);
                    }
                }

                // Refund payer if a partial payment was made.
                if invoice.amount_paid > 0 {
                    let refund_amount = invoice.amount_paid;
                    token.transfer(&contract_address, &invoice.payer, &refund_amount);

                    env.events().publish(
                        (
                            Symbol::new(&env, "dispute_upheld_payer_refund"),
                            invoice_id,
                            invoice.payer.clone(),
                        ),
                        DisputeUpheldPayerRefund {
                            invoice_id,
                            payer: invoice.payer.clone(),
                            amount: refund_amount,
                        },
                    );
                }
            }
            2 => {
                // Rejected: Freelancer is right.
                // Restore status based on funding level.
                if invoice.amount_funded == invoice.amount {
                    invoice.status = InvoiceStatus::Funded;
                } else if invoice.amount_funded > 0 {
                    invoice.status = InvoiceStatus::PartiallyFunded;
                } else {
                    invoice.status = InvoiceStatus::Pending;
                }
            }
            _ => return Err(ContractError::Unauthorized), // Invalid resolution
        }

        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        env.events().publish(
            (
                Symbol::new(&env, "dispute_resolved"),
                invoice_id,
                resolution_hash.clone(),
            ),
            DisputeResolved {
                invoice_id,
                resolution_hash,
                resolution,
                resolved_at: env.ledger().timestamp(),
            },
        );

        unlock_reentrancy(&env);
        Ok(())
    }

    /// Auto-resolve a dispute after the timeout has passed.
    ///
    /// * `invoice_id` – the invoice to auto-resolve
    /// Access: Anyone
    pub fn auto_resolve_dispute(env: Env, invoice_id: u64) -> Result<(), ContractError> {
        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }

        let mut invoice = load_invoice(&env, invoice_id);

        if invoice.status != InvoiceStatus::Disputed {
            return Err(ContractError::NotDisputed);
        }

        let dispute = get_dispute(&env, invoice_id).ok_or(ContractError::InvoiceNotFound)?;
        let config = crate::storage::get_config(&env).ok_or(ContractError::Unauthorized)?;

        let now_ledger = env.ledger().sequence();

        if u64::from(now_ledger)
            < u64::from(dispute.disputed_at).saturating_add(config.dispute_timeout_ledgers)
        {
            return Err(ContractError::Unauthorized); // Or a more specific error like TimeoutNotReached
        }

        // Auto-resolve: Default to Rejected (Freelancer right) to prevent DOS.
        if invoice.amount_funded == invoice.amount {
            invoice.status = InvoiceStatus::Funded;
        } else if invoice.amount_funded > 0 {
            invoice.status = InvoiceStatus::PartiallyFunded;
        } else {
            invoice.status = InvoiceStatus::Pending;
        }

        save_invoice(&env, &invoice);
        crate::nft::sync_nft_state(&env, invoice_id)?;

        let empty_hash = BytesN::from_array(&env, &[0u8; 32]);
        env.events().publish(
            (
                Symbol::new(&env, "dispute_resolved"),
                invoice_id,
                empty_hash.clone(),
            ),
            DisputeResolved {
                invoice_id,
                resolution_hash: empty_hash,
                resolution: 2, // Rejected
                resolved_at: env.ledger().timestamp(),
            },
        );

        Ok(())
    }

    // ================================================================
    // Contract Configuration
    // ================================================================

    #[allow(clippy::too_many_arguments)]
    pub fn update_config(
        env: Env,
        caller: Address,
        high_rep_threshold: u32,
        bonus_bps: u32,
        min_discount_rate_bps: u32,
        decay_rate_bps: u32,
        decay_period_ledgers: u64,
        dispute_timeout_ledgers: u64,
        xlm_sac_address: Address,
        usdc_sac_address: Address,
        eurc_sac_address: Address,
    ) -> Result<(), ContractError> {
        crate::config::update_config(
            &env,
            &caller,
            high_rep_threshold,
            bonus_bps,
            min_discount_rate_bps,
            decay_rate_bps,
            decay_period_ledgers,
            dispute_timeout_ledgers,
            xlm_sac_address,
            usdc_sac_address,
            eurc_sac_address,
        )
        .map_err(|_| ContractError::Unauthorized)
    }

    pub fn get_config(env: Env) -> Result<Config, ContractError> {
        crate::storage::get_config(&env).ok_or(ContractError::Unauthorized)
    }
    // payer_score
    // ----------------------------------------------------------------
    /// Access: Anyone
    pub fn payer_score(env: Env, payer: Address) -> u32 {
        get_payer_score(&env, &payer)
    }

    // ----------------------------------------------------------------
    // lp_score  (Issue #34)
    // ----------------------------------------------------------------
    /// Access: Anyone
    pub fn lp_score(env: Env, lp: Address) -> u32 {
        get_lp_score(&env, &lp)
    }

    // ----------------------------------------------------------------
    // get_top_payers (Issue #77)
    // ----------------------------------------------------------------
    /// Return up to `limit` payers with the highest reputation scores.
    /// Reads from the maintained top-payers heap — no full-list sort required.
    /// Access: Anyone
    pub fn get_top_payers(env: Env, limit: u32) -> Vec<TopPayerEntry> {
        crate::top_payers::get_top_payers(&env, limit)
    }

    // ----------------------------------------------------------------
    // get_reputation (Issue #26)
    // ----------------------------------------------------------------
    /// Read an address's detailed reputation profile. Unknown addresses return
    /// a zeroed profile rather than panicking.
    /// Access: Anyone
    pub fn get_reputation(env: Env, address: Address) -> ReputationProfile {
        get_reputation(&env, &address)
    }

    // ----------------------------------------------------------------
    // min_payer_reputation config (Issue #28)
    // ----------------------------------------------------------------
    /// Current minimum payer reputation required to fund an invoice (0 = off).
    /// Access: Anyone
    pub fn min_payer_reputation(env: Env) -> u32 {
        get_min_payer_reputation(&env)
    }

    /// Update the minimum payer reputation threshold.
    /// Access: Admin only
    pub fn set_min_payer_reputation(env: Env, value: u32) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(
            &env,
            "set_min_payer_reputation",
            ECONOMIC_PARAM_COOLDOWN_LEDGERS,
        )?;
        record_admin_action(&env, "set_min_payer_reputation");
        let updated_by = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        let old_value = get_min_payer_reputation(&env);
        set_min_payer_reputation(&env, value);
        let pn = Symbol::new(&env, "min_payer_reputation");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn.clone(),
                updated_by.clone(),
            ),
            ParameterUpdated {
                param_name: pn,
                old_value: old_value as i128,
                new_value: value as i128,
                updated_by,
            },
        );
        Ok(())
    }

    // ----------------------------------------------------------------
    // Issue #655: staged mainnet rollout caps
    // ----------------------------------------------------------------
    /// Current per-invoice amount cap (0 = uncapped).
    /// Access: Anyone
    pub fn max_invoice_amount(env: Env) -> i128 {
        get_max_invoice_amount(&env)
    }

    /// Update the maximum single-invoice amount. Intended to be raised over
    /// time via governance as a staged mainnet rollout progresses.
    /// Access: Admin only (routed through governance once `admin` is set to
    /// the governance contract's address — see `iln_governance`).
    pub fn set_max_invoice_amount(env: Env, value: i128) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "set_max_invoice_amount", ECONOMIC_PARAM_COOLDOWN_LEDGERS)?;
        let updated_by = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        let old_value = get_max_invoice_amount(&env);
        set_max_invoice_amount(&env, value);
        let pn = Symbol::new(&env, "max_invoice_amount");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn.clone(),
                updated_by.clone(),
            ),
            ParameterUpdated {
                param_name: pn,
                old_value,
                new_value: value,
                updated_by,
            },
        );
        Ok(())
    }

    /// Current cumulative funded-volume cap for `token` (0 = uncapped).
    /// Access: Anyone
    pub fn token_volume_cap(env: Env, token: Address) -> i128 {
        get_token_volume_cap(&env, &token)
    }

    /// Update the cumulative funded-volume cap for `token`. Intended to be
    /// raised over time via governance as a staged mainnet rollout
    /// progresses — checked against the same cumulative counter used for
    /// `get_contract_stats` reporting.
    /// Access: Admin only (routed through governance once `admin` is set to
    /// the governance contract's address — see `iln_governance`).
    pub fn set_token_volume_cap(
        env: Env,
        token: Address,
        value: i128,
    ) -> Result<(), ContractError> {
        require_admin(&env)?;
        check_rate_limit(&env, "set_token_volume_cap", ECONOMIC_PARAM_COOLDOWN_LEDGERS)?;
        let updated_by = get_admin(&env).ok_or(ContractError::Unauthorized)?;
        let old_value = get_token_volume_cap(&env, &token);
        set_token_volume_cap(&env, &token, value);
        // Note: the target token is not carried in the event topics (kept at
        // 3, matching every other parameter_updated event in this contract)
        // — it is visible in the `set_token_volume_cap` call's own arguments
        // on-chain, and callers needing it per-event should track via
        // `token_volume_cap(token)` reads rather than topic filtering.
        let pn = Symbol::new(&env, "token_volume_cap");
        env.events().publish(
            (
                Symbol::new(&env, "parameter_updated"),
                pn.clone(),
                updated_by.clone(),
            ),
            ParameterUpdated {
                param_name: pn,
                old_value,
                new_value: value,
                updated_by,
            },
        );
        Ok(())
    }

    // ----------------------------------------------------------------
    // suggested_discount_rate
    // ----------------------------------------------------------------
    /// Access: Anyone
    pub fn suggested_discount_rate(env: Env, payer: Address) -> u32 {
        let score = get_payer_score(&env, &payer);
        let capped = score.min(100);
        let rate = 500 + (100 - capped) * 5;
        rate.max(50)
    }

    /// Returns the invoice with the given `invoice_id`.
    ///
    /// This is a read-only view method that returns the full `Invoice`
    /// struct, including submitter, payer, LP, token, amount, discount rate,
    /// due date, status, and funding state.
    ///
    /// # Errors
    ///
    /// Returns `ContractError::InvoiceNotFound` if the invoice does not exist.
    // ----------------------------------------------------------------
    // get_invoice
    // ----------------------------------------------------------------
    /// Access: Anyone
    pub fn get_invoice(env: Env, invoice_id: u64) -> Result<Invoice, ContractError> {
        if !invoice_exists(&env, invoice_id) {
            return Err(ContractError::InvoiceNotFound);
        }
        Ok(load_invoice(&env, invoice_id))
    }

    /// Access: Anyone
    pub fn get_invoice_count(env: Env) -> u64 {
        crate::invoice::read_next_invoice_id(&env).saturating_sub(1)
    }

    // ----------------------------------------------------------------
    // query_nft_metadata
    // ----------------------------------------------------------------
    /// Get NFT metadata for an invoice
    ///
    /// Returns complete NFT metadata including invoice ID, amount, due date,
    /// discount rate, token address, current owner, and mint timestamp.
    ///
    /// # Arguments
    /// * `env` - Soroban environment
    /// * `invoice_id` - The invoice ID
    ///
    /// # Returns
    /// Option containing the NFT metadata if the NFT exists, None otherwise
    ///
    /// # Access
    /// Anyone
    pub fn query_nft_metadata(env: Env, invoice_id: u64) -> Option<crate::nft::InvoiceNftMetadata> {
        crate::nft::query_nft_metadata(env, invoice_id)
    }

    // ----------------------------------------------------------------
    // query_nft_owner
    // ----------------------------------------------------------------
    /// Get the owner of an invoice NFT
    ///
    /// Returns the current owner address of the NFT representing the invoice.
    ///
    /// # Arguments
    /// * `env` - Soroban environment
    /// * `invoice_id` - The invoice ID
    ///
    /// # Returns
    /// Option containing the owner address if the NFT exists, None otherwise
    ///
    /// # Access
    /// Anyone
    pub fn query_nft_owner(env: Env, invoice_id: u64) -> Option<Address> {
        crate::nft::query_nft_owner(env, invoice_id)
    }
}

// ----------------------------------------------------------------
// TOKEN HELPERS
// ----------------------------------------------------------------

fn token_client<'a>(env: &'a Env, token: &Address) -> TokenClient<'a> {
    TokenClient::new(env, token)
}

fn discount_rate_as_i128(rate: u32) -> i128 {
    rate as i128
}

/// Compute 10^exp as i128 without std.  Used for token-decimal scaling.
/// Saturates at i128::MAX to prevent overflow for extreme inputs (max useful
/// value is 10^18 which fits comfortably inside i128).
fn ten_pow(exp: u32) -> i128 {
    let mut result: i128 = 1;
    for _ in 0..exp {
        result = result.saturating_mul(10);
    }
    result
}

// ----------------------------------------------------------------
// XLM PRECISION HANDLING
// ----------------------------------------------------------------
/// Check if a token address is the XLM SAC address
fn is_xlm_token(env: &Env, token: &Address) -> bool {
    if let Some(config) = crate::storage::get_config(env) {
        token == &config.xlm_sac_address
    } else {
        false
    }
}

/// Convert amount from XLM precision (7 decimals) to contract precision
fn normalize_xlm_amount(amount: i128) -> i128 {
    amount
}

/// Check if a token address is the USDC address
fn is_usdc_token(env: &Env, token: &Address) -> bool {
    if let Some(config) = crate::storage::get_config(env) {
        token == &config.usdc_sac_address
    } else {
        false
    }
}

/// Convert amount from USDC precision (6 decimals) to contract precision
fn normalize_usdc_amount(amount: i128) -> i128 {
    amount
}

/// Check if a token address is the EURC address
fn is_eurc_token(env: &Env, token: &Address) -> bool {
    if let Some(config) = crate::storage::get_config(env) {
        token == &config.eurc_sac_address
    } else {
        false
    }
}

/// Convert amount from EURC precision (6 decimals) to contract precision
fn normalize_eurc_amount(amount: i128) -> i128 {
    amount
}

fn validate_invoice_terms(
    env: &Env,
    amount: i128,
    due_date: u64,
    discount_rate: u32,
) -> Result<(), ContractError> {
    // Backward-compatible fallback (no token context).  Uses the minimum for a
    // 6-decimal token (USDC).  New call-sites should prefer
    // `validate_invoice_terms_with_token` so the check is token-aware.
    validate_invoice_terms_for_min(env, amount, 1_000_000, due_date, discount_rate)
}

/// Validate invoice terms using the decimal precision stored for `token`.
///
/// The minimum accepted amount is `1` whole unit in the token's own precision:
/// - 6-decimal token (USDC): minimum = 1_000_000  (= 1 USDC)
/// - 7-decimal token (XLM):  minimum = 10_000_000 (= 1 XLM)
///
/// Falls back to the 6-decimal floor when no decimals are registered for the
/// token (i.e. a token added via legacy code paths before Issue #23).
fn validate_invoice_terms_with_token(
    env: &Env,
    amount: i128,
    due_date: u64,
    discount_rate: u32,
    token: &Address,
) -> Result<(), ContractError> {
    let decimals: u32 = env
        .storage()
        .persistent()
        .get(&crate::storage::DataKey::TokenDecimals(token.clone()))
        .unwrap_or(6);

    let min_amount: i128 = ten_pow(decimals);
    validate_invoice_terms_for_min(env, amount, min_amount, due_date, discount_rate)
}

/// Core validation logic shared by the two public entry points above.
fn validate_invoice_terms_for_min(
    env: &Env,
    amount: i128,
    min_amount: i128,
    due_date: u64,
    discount_rate: u32,
) -> Result<(), ContractError> {
    if amount < min_amount {
        return Err(ContractError::InvalidAmount);
    }

    let max_rate: u32 = env
        .storage()
        .instance()
        .get(&crate::storage::DataKey::MaxDiscountRate)
        .unwrap_or(5000);
    if discount_rate == 0 || discount_rate > max_rate {
        return Err(ContractError::InvalidDiscountRate);
    }

    // The on-chain storage representation now uses u32 timestamps.
    if due_date > u64::from(u32::MAX) {
        return Err(ContractError::InvalidDueDate);
    }

    let now = env.ledger().timestamp();

    // Validate due date is in the future
    if due_date <= now {
        return Err(ContractError::InvalidDueDate);
    }

    if due_date < now.saturating_add(MIN_INVOICE_DURATION) {
        return Err(ContractError::DueDateTooSoon);
    }

    if due_date > now.saturating_add(MAX_INVOICE_DURATION) {
        return Err(ContractError::DueDateTooFar);
    }

    Ok(())
}

fn is_approved_token(env: &Env, token: &Address) -> bool {
    // The explicit allowlist flag is authoritative once set — `initialize()`
    // sets it `true` for the three core tokens (USDC/EURC/XLM) and
    // `remove_token()` sets it `false`, so an explicit `false` here means
    // the token was deliberately removed and must not fall through to the
    // Config-based check below (which would otherwise re-approve a removed
    // core token unconditionally).
    if let Some(approved) = env
        .storage()
        .persistent()
        .get::<_, bool>(&crate::storage::DataKey::ApprovedToken(token.clone()))
    {
        return approved;
    }

    // No explicit flag has ever been recorded for this token — fall back to
    // the wired tokens in Config (covers state that predates the explicit
    // ApprovedToken flag).
    if let Some(config) = crate::storage::get_config(env) {
        if token == &config.usdc_sac_address
            || token == &config.eurc_sac_address
            || token == &config.xlm_sac_address
        {
            return true;
        }
    }

    false
}

fn notify_distribution_funding(env: &Env, lp: &Address, amount_usdc_equivalent: i128) {
    let Some(dist_contract) = env
        .storage()
        .instance()
        .get::<_, Address>(&crate::storage::DataKey::DistributionContract)
    else {
        return;
    };

    let args = vec![
        env,
        lp.clone().into_val(env),
        amount_usdc_equivalent.into_val(env),
    ];
    env.invoke_contract::<()>(&dist_contract, &Symbol::new(env, "accrue_lp"), args);
}

fn notify_distribution_settlement(
    env: &Env,
    freelancer: &Address,
    payer: &Address,
    settled_on_time: bool,
) {
    let Some(dist_contract) = env
        .storage()
        .instance()
        .get::<_, Address>(&crate::storage::DataKey::DistributionContract)
    else {
        return;
    };

    let args = vec![
        env,
        freelancer.clone().into_val(env),
        payer.clone().into_val(env),
        settled_on_time.into_val(env),
    ];
    env.invoke_contract::<()>(&dist_contract, &Symbol::new(env, "accrue_settlement"), args);
}

// ----------------------------------------------------------------
// TEST MODULES
// ----------------------------------------------------------------

pub(crate) mod test;
// Issue #124 / #641 / #639: multisig admin module + error-path coverage
// (AlreadySigned / ProposalExpired / ThresholdNotReached).
mod tests_multisig_admin;
mod tests_insurance_integration;
mod tests_lifecycle_integration;
mod tests_min_invoice_amount;
mod tests_new_features;
mod tests_oracle_registry;
mod tests_storage;
mod tests_storage_layout;
// Issue #MEV-1: resolve_fund_queue maturity delay
mod tests_mev_mitigation;
// Issue #invoice-count: get_invoice_count underflow safety
mod tests_invoice_count;
// Issue #batch-reputation: batch_submit increments invoices_submitted
mod tests_batch_submit_reputation;
// Issue #pause-checks: expire_invoice and appeal_default pause guards
mod tests_pause_checks;
// Economic security regression suite (threat-model B0/B1/D1) — declared here
// so the consolidated `es_*` tests run in every required CI cargo test pass.
mod tests_economic_security;
// Issue #34 reputation-weighted queue was previously ORPHANED (file present
// but never declared), so its queue-integrity/griefing guards never ran.
mod tests_lp_priority_queue;
// Issue #848: ADR-011 reputation cross-contract consistency audit
mod tests_reputation_state_isolation;
// Issue #849: ADR-004 lazy reputation decay multi-year idle coverage
mod tests_reputation_decay_long_idle;
// Issue #850: ADR-007 NFT event emission verification
mod tests_nft_event_emission;
// Issue #851: NFT transfer reputation and insurance consistency
mod tests_nft_transfer_consistency;
