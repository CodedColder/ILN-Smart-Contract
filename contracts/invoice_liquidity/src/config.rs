use crate::events::ParameterUpdated;
use soroban_sdk::{contracttype, Address, Env, Symbol};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub high_rep_threshold: u32,
    pub bonus_bps: u32,
    pub min_discount_rate_bps: u32,
    pub decay_rate_bps: u32, // Basis points to decay per period (e.g., 50 = 0.5%)
    pub decay_period_ledgers: u64, // Ledger count between decay applications
    pub dispute_timeout_ledgers: u64, // Ledger count after which a dispute can be auto-resolved
    pub xlm_sac_address: Address, // Stellar Asset Contract address for native XLM wrapper
    pub usdc_sac_address: Address, // USDC contract address
    pub eurc_sac_address: Address, // EURC contract address
    pub price_oracle: Option<Address>, // Optional price oracle for USD normalisation
    /// Maximum acceptable oracle data age in ledgers before fund_invoice rejects it.
    /// Default: 17_280 (≈ 24 hours at one ledger per 5 seconds).
    /// Updatable by governance via set_max_oracle_age().
    pub max_oracle_age_ledgers: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Unauthorized,
    InvalidBonusBps,
    InvalidMinDiscountRate,
    InvalidDecayRate,
    InvalidRepThreshold,
}

const MAX_BONUS_BPS: u32 = 500;
const MAX_DECAY_RATE_BPS: u32 = 500;
const MIN_REP_THRESHOLD: u32 = 0;
const MAX_REP_THRESHOLD: u32 = 100;
const MAX_MIN_DISCOUNT_RATE_BPS: u32 = 10_000;

#[allow(clippy::too_many_arguments)]
pub fn update_config(
    env: &Env,
    caller: &Address,
    high_rep_threshold: u32,
    bonus_bps: u32,
    min_discount_rate_bps: u32,
    decay_rate_bps: u32,
    decay_period_ledgers: u64,
    dispute_timeout_ledgers: u64,
    xlm_sac_address: Address,
    usdc_sac_address: Address,
    eurc_sac_address: Address,
) -> Result<(), ConfigError> {
    let admin = crate::storage::get_admin(env).ok_or(ConfigError::Unauthorized)?;
    let old_config = crate::storage::get_config(env).ok_or(ConfigError::Unauthorized)?;
    caller.require_auth();
    if caller != &admin {
        return Err(ConfigError::Unauthorized);
    }

    if bonus_bps > MAX_BONUS_BPS {
        return Err(ConfigError::InvalidBonusBps);
    }
    if min_discount_rate_bps == 0 || min_discount_rate_bps >= MAX_MIN_DISCOUNT_RATE_BPS {
        return Err(ConfigError::InvalidMinDiscountRate);
    }
    if decay_rate_bps > MAX_DECAY_RATE_BPS {
        return Err(ConfigError::InvalidDecayRate);
    }
    if high_rep_threshold > MAX_REP_THRESHOLD {
        return Err(ConfigError::InvalidRepThreshold);
    }

    let new_config = Config {
        high_rep_threshold,
        bonus_bps,
        min_discount_rate_bps,
        decay_rate_bps,
        decay_period_ledgers,
        dispute_timeout_ledgers,
        xlm_sac_address,
        usdc_sac_address,
        eurc_sac_address,
        price_oracle: old_config.price_oracle,
        max_oracle_age_ledgers: old_config.max_oracle_age_ledgers,
    };

    crate::storage::set_config(env, &new_config);

    let emit = |param_name: &str, old_value: i128, new_value: i128| {
        let pn = Symbol::new(env, param_name);
        env.events().publish(
            (
                Symbol::new(env, "parameter_updated"),
                pn.clone(),
                caller.clone(),
            ),
            ParameterUpdated {
                param_name: pn,
                old_value,
                new_value,
                updated_by: caller.clone(),
            },
        );
    };

    // Stable audit identifiers for each numeric protocol parameter.
    emit(
        "high_rep_threshold",
        old_config.high_rep_threshold as i128,
        high_rep_threshold as i128,
    );
    emit("bonus_bps", old_config.bonus_bps as i128, bonus_bps as i128);
    emit(
        "min_discount_rate_bps",
        old_config.min_discount_rate_bps as i128,
        min_discount_rate_bps as i128,
    );
    emit(
        "decay_rate_bps",
        old_config.decay_rate_bps as i128,
        decay_rate_bps as i128,
    );
    emit(
        "decay_period_ledgers",
        old_config.decay_period_ledgers as i128,
        decay_period_ledgers as i128,
    );
    emit(
        "dispute_timeout_ledgers",
        old_config.dispute_timeout_ledgers as i128,
        dispute_timeout_ledgers as i128,
    );

    Ok(())
}

pub fn set_price_oracle(env: &Env, caller: &Address, oracle: Address) -> Result<(), ConfigError> {
    let admin = crate::storage::get_admin(env).ok_or(ConfigError::Unauthorized)?;
    let mut config = crate::storage::get_config(env).ok_or(ConfigError::Unauthorized)?;
    if caller != &admin {
        return Err(ConfigError::Unauthorized);
    }

    config.price_oracle = Some(oracle);
    crate::storage::set_config(env, &config);
    Ok(())
}

/// Update the maximum oracle data age (in ledgers). Admin only.
pub fn set_max_oracle_age(
    env: &Env,
    caller: &Address,
    max_age_ledgers: u64,
) -> Result<(), ConfigError> {
    let admin = crate::storage::get_admin(env).ok_or(ConfigError::Unauthorized)?;
    let mut config = crate::storage::get_config(env).ok_or(ConfigError::Unauthorized)?;
    if caller != &admin {
        return Err(ConfigError::Unauthorized);
    }

    config.max_oracle_age_ledgers = max_age_ledgers;
    crate::storage::set_config(env, &config);
    Ok(())
}
