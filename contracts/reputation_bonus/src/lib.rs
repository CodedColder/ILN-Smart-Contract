#![no_std]

pub mod config;
pub mod errors;
pub mod events;
pub mod invoice;
pub mod rate_logic;
pub mod reputation;

use crate::config::{get_config, set_admin, set_config, update_config, Config};
use crate::errors::ContractError;
use crate::events::{emit_config_set, emit_initialized};
use crate::invoice::{handle_default, mark_paid, submit_invoice, Invoice};
use crate::reputation::{read_reputation, ReputationScore};
use soroban_sdk::{contract, contractimpl, Address, Env};

#[contract]
pub struct ReputationBonusContract;

#[contractimpl]
impl ReputationBonusContract {
    /// `init` contract entry point.
///
/// Access: Anyone
pub fn init(env: Env, admin: Address) {
        set_admin(&env, &admin);
        emit_initialized(&env, &admin);
    }

    /// `set_config` contract entry point.
///
/// Access: Anyone
pub fn set_config(env: Env, config: Config) -> Result<(), ContractError> {
        set_config(&env, &config).map_err(|e| match e {
            crate::config::ConfigError::InvalidHighRepThreshold => ContractError::ConfigErrorInvalidHighRepThreshold,
            crate::config::ConfigError::InvalidBonusBps => ContractError::ConfigErrorInvalidBonusBps,
            crate::config::ConfigError::InvalidMinDiscountRate => ContractError::ConfigErrorInvalidMinDiscountRate,
            crate::config::ConfigError::Unauthorized => ContractError::ConfigErrorUnauthorized,
        })?;
        emit_config_set(
            &env,
            config.high_rep_threshold,
            config.bonus_bps,
            config.min_discount_rate_bps,
        );
        Ok(())
    }

    /// `get_config` contract entry point.
///
/// Access: Anyone
pub fn get_config(env: Env) -> Result<Config, ContractError> {
        get_config(&env).map_err(|_| ContractError::ConfigErrorUnauthorized)
    }

    /// `update_config` contract entry point.
///
/// Access: Anyone
pub fn update_config(
        env: Env,
        caller: Address,
        high_rep_threshold: u32,
        bonus_bps: u32,
        min_discount_rate_bps: u32,
    ) -> Result<(), ContractError> {
        update_config(
            &env,
            &caller,
            high_rep_threshold,
            bonus_bps,
            min_discount_rate_bps,
        )
        .map_err(|e| match e {
            crate::config::ConfigError::InvalidHighRepThreshold => ContractError::ConfigErrorInvalidHighRepThreshold,
            crate::config::ConfigError::InvalidBonusBps => ContractError::ConfigErrorInvalidBonusBps,
            crate::config::ConfigError::InvalidMinDiscountRate => ContractError::ConfigErrorInvalidMinDiscountRate,
            crate::config::ConfigError::Unauthorized => ContractError::ConfigErrorUnauthorized,
        })
    }

    /// `get_reputation` contract entry point.
///
/// Access: Anyone
pub fn get_reputation(env: Env, address: Address) -> ReputationScore {
        read_reputation(&env, &address)
    }

    /// `submit_invoice` contract entry point.
///
/// Access: Anyone
pub fn submit_invoice(
        env: Env,
        freelancer: Address,
        payer: Address,
        amount: i128,
        due_date: u64,
        base_discount_rate_bps: u32,
    ) -> Result<Invoice, ContractError> {
        submit_invoice(
            &env,
            &freelancer,
            &payer,
            amount,
            due_date,
            base_discount_rate_bps,
        )
    }

    /// `mark_paid` contract entry point.
///
/// Access: Anyone
pub fn mark_paid(env: Env, invoice_id: u64) -> Result<(), ContractError> {
        mark_paid(&env, invoice_id)
    }

    /// `handle_default` contract entry point.
///
/// Access: Anyone
pub fn handle_default(env: Env, invoice_id: u64) -> Result<(), ContractError> {
        handle_default(&env, invoice_id)
    }
}
