use crate::events::{
    emit_parameter_updated, PARAM_BONUS_BPS, PARAM_HIGH_REP_THRESHOLD, PARAM_MIN_DISCOUNT_RATE_BPS,
};
use soroban_sdk::{contracterror, contracttype, Address, Env};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub high_rep_threshold: u32,
    pub bonus_bps: u32,
    pub min_discount_rate_bps: u32,
}

#[contracttype]
pub enum ConfigKey {
    Config,
    Admin,
}

#[contracterror]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Unauthorized = 1,
    InvalidBonusBps = 2,
    InvalidMinDiscountRate = 3,
    InvalidHighRepThreshold = 4,
}

const MAX_BONUS_BPS: u32 = 500;
const MAX_REPUTATION_SCORE: u32 = 100;

pub fn get_admin(env: &Env) -> Result<Address, ConfigError> {
    env.storage()
        .instance()
        .get(&ConfigKey::Admin)
        .ok_or(ConfigError::Unauthorized)
}

pub fn set_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&ConfigKey::Admin, admin);
}

pub fn get_config(env: &Env) -> Result<Config, ConfigError> {
    env.storage()
        .instance()
        .get(&ConfigKey::Config)
        .ok_or(ConfigError::Unauthorized)
}

pub fn set_config(env: &Env, config: &Config) -> Result<(), ConfigError> {
    if config.high_rep_threshold > MAX_REPUTATION_SCORE {
        return Err(ConfigError::InvalidHighRepThreshold);
    }
    if config.bonus_bps > MAX_BONUS_BPS {
        return Err(ConfigError::InvalidBonusBps);
    }
    if config.min_discount_rate_bps == 0 {
        return Err(ConfigError::InvalidMinDiscountRate);
    }
    env.storage().instance().set(&ConfigKey::Config, config);
    Ok(())
}

pub fn update_config(
    env: &Env,
    caller: &Address,
    high_rep_threshold: u32,
    bonus_bps: u32,
    min_discount_rate_bps: u32,
) -> Result<(), ConfigError> {
    let admin = get_admin(env)?;
    let old_config = get_config(env)?;
    caller.require_auth();
    if caller != &admin {
        return Err(ConfigError::Unauthorized);
    }

    let new_config = Config {
        high_rep_threshold,
        bonus_bps,
        min_discount_rate_bps,
    };

    set_config(env, &new_config)?;

    emit_parameter_updated(
        env,
        PARAM_HIGH_REP_THRESHOLD,
        old_config.high_rep_threshold as i128,
        high_rep_threshold as i128,
        caller,
    );
    emit_parameter_updated(
        env,
        PARAM_BONUS_BPS,
        old_config.bonus_bps as i128,
        bonus_bps as i128,
        caller,
    );
    emit_parameter_updated(
        env,
        PARAM_MIN_DISCOUNT_RATE_BPS,
        old_config.min_discount_rate_bps as i128,
        min_discount_rate_bps as i128,
        caller,
    );

    Ok(())
}
