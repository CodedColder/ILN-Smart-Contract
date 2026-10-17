#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token::StellarAssetClient,
    Address, Env, Symbol,
};

const HALF_TOKEN: i128 = 5_000_000;
const HUNDRED_USDC_STROOPS: i128 = 1_000_000_000;
/// Default LP reward rate: 10,000,000 stroops per 100 USDC.
const DEFAULT_LP_REWARD_RATE: i128 = 10_000_000;
/// Default freelancer reward rate: 5,000,000 stroops per settlement.
const DEFAULT_FREELANCER_REWARD_RATE: i128 = HALF_TOKEN;
/// Default payer reward rate: 5,000,000 stroops per on-time settlement.
const DEFAULT_PAYER_REWARD_RATE: i128 = HALF_TOKEN;
/// Defense-in-depth ceiling for a single `accrue_lp` call (~1,000,000 USDC
/// at 7-decimal stroops). Prevents a compromised/misconfigured ILN from
/// accruing absurd volumes in one invocation.
pub const MAX_LP_ACCRUAL_PER_CALL: i128 = 10_000_000_000_000; // 1e13

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DistributionError {
    /// Contract has already been initialized.
    AlreadyInitialized = 1,
}

#[contracttype]
pub enum StorageKey {
    Initialized,
    IlnContract,
    GovToken,
    LpFundedVolume(Address),
    FreelancerSettled(Address),
    PayerOnTimeSettled(Address),
    Claimed(Address),
    /// Reward rate per 100 USDC of LP volume (in stroops).
    LpRewardRate,
    /// Reward rate per freelancer settlement (in stroops).
    FreelancerRewardRate,
    /// Reward rate per on-time payer settlement (in stroops).
    PayerRewardRate,
}

/// Emitted once, when the contract is initialised (Issue #538).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractInitialized {
    pub iln_contract: Address,
    pub gov_token: Address,
}

/// Emitted when an LP's funded volume accrual increases (Issue #538).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LpVolumeAccrued {
    pub lp: Address,
    pub amount_usdc_equivalent: i128,
}

/// Emitted when a settlement is recorded for a freelancer/payer (Issue #538).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SettlementAccrued {
    pub freelancer: Address,
    pub payer: Address,
    pub settled_on_time: bool,
}

/// Emitted when a participant claims accrued governance tokens (Issue #538).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TokensClaimed {
    pub claimer: Address,
    pub amount: i128,
}

/// Emitted when a reward rate is updated via governance.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RewardRateUpdated {
    pub rate_type: Symbol,
    pub old_rate: i128,
    pub new_rate: i128,
}

#[contract]
pub struct IlnDistribution;

#[contractimpl]
impl IlnDistribution {
    /// Initialize the distribution contract with the ILN core contract and governance token.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `iln_contract` - Address of the ILN core contract (sole authorized caller for accruals).
    /// * `gov_token` - Address of the governance token to mint rewards in.
    ///
    /// # Access
    /// * Callable once during deployment.
    ///
    /// # Panics
    /// * Panics with `"already initialized"` if called more than once.
    /// * Panics with `"governance token SAC admin must be the distribution
    ///   contract"` if `gov_token` is not a SAC administered by this
    ///   contract (Issue #861). The check runs before any state is written,
    ///   so a rejected deployment leaves the contract uninitialized and
    ///   retryable.
    pub fn initialize(env: Env, iln_contract: Address, gov_token: Address) -> Result<(), DistributionError> {
        if env.storage().instance().has(&StorageKey::Initialized) {
            soroban_sdk::panic_with_error!(&env, DistributionError::AlreadyInitialized);
        }

        Self::require_gov_token_mint_authority(&env, &gov_token);

        env.storage()
            .instance()
            .set(&StorageKey::Initialized, &true);
        env.storage()
            .instance()
            .set(&StorageKey::IlnContract, &iln_contract);
        env.storage()
            .instance()
            .set(&StorageKey::GovToken, &gov_token);
        env.storage()
            .instance()
            .set(&StorageKey::LpRewardRate, &DEFAULT_LP_REWARD_RATE);
        env.storage().instance().set(
            &StorageKey::FreelancerRewardRate,
            &DEFAULT_FREELANCER_REWARD_RATE,
        );
        env.storage()
            .instance()
            .set(&StorageKey::PayerRewardRate, &DEFAULT_PAYER_REWARD_RATE);

        env.events().publish(
            (symbol_short!("init"),),
            ContractInitialized {
                iln_contract,
                gov_token,
            },
        );

        Ok(())
    }

    /// Re-check the governance-token mint authority invariant (Issue #861).
    ///
    /// `claim_tokens` mints through `StellarAssetClient::mint`, which only
    /// the SAC admin may call, so a governance token administered by an
    /// EOA/multisig would make every non-empty claim revert at runtime.
    /// `initialize` enforces the invariant at construction; this view
    /// re-checks it against live chain state for deployment tooling
    /// (`scripts/smoke-test.ts`).
    ///
    /// # Returns
    /// * `true` when the governance token's SAC admin is this contract.
    ///
    /// # Panics
    /// * Panics with `"not initialized"` if `initialize` has not run.
    pub fn verify_mint_authority(env: Env) -> bool {
        let gov_token: Address = match env.storage().instance().get(&StorageKey::GovToken) {
            Some(token) => token,
            None => panic!("not initialized"),
        };
        StellarAssetClient::new(&env, &gov_token).admin() == env.current_contract_address()
    }

    /// Record LP-funded volume for reward accrual.
    ///
    /// Called by the ILN core contract when an LP funds an invoice.
    /// Accumulates volume that determines the LP's governance token reward.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `lp` - Address of the liquidity provider.
    /// * `amount_usdc_equivalent` - Volume in USDC stroops (7 decimals).
    ///
    /// # Access
    /// * Restricted to the ILN core contract via `require_auth`.
    ///
    /// # Behavior
    /// * Non-positive and amounts exceeding `MAX_LP_ACCRUAL_PER_CALL` are silently ignored.
    pub fn accrue_lp(env: Env, lp: Address, amount_usdc_equivalent: i128) {
        Self::require_iln_invoker(&env);

        // Defense-in-depth: ignore non-positive and absurdly large settlements
        // rather than trusting upstream blindly (even though ILN is the sole
        // intended caller).
        if amount_usdc_equivalent <= 0 || amount_usdc_equivalent > MAX_LP_ACCRUAL_PER_CALL {
            return;
        }

        let key = StorageKey::LpFundedVolume(lp.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&key, &current.saturating_add(amount_usdc_equivalent));

        env.events().publish(
            (symbol_short!("lp_accr"), lp.clone()),
            LpVolumeAccrued {
                lp,
                amount_usdc_equivalent,
            },
        );
    }

    /// Record a settlement for freelancer and payer reward accrual.
    ///
    /// Called by the ILN core contract when an invoice is settled.
    /// Increments the freelancer's settlement count and (if on-time) the payer's
    /// on-time settlement count.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `freelancer` - Address of the freelancer receiving payment.
    /// * `payer` - Address of the payer making payment.
    /// * `settled_on_time` - Whether the settlement met the deadline.
    ///
    /// # Access
    /// * Restricted to the ILN core contract via `require_auth`.
    pub fn accrue_settlement(env: Env, freelancer: Address, payer: Address, settled_on_time: bool) {
        Self::require_iln_invoker(&env);

        let freelancer_key = StorageKey::FreelancerSettled(freelancer.clone());
        let freelancer_count: u64 = env
            .storage()
            .persistent()
            .get(&freelancer_key)
            .unwrap_or(0_u64);
        env.storage()
            .persistent()
            .set(&freelancer_key, &freelancer_count.saturating_add(1));

        if settled_on_time {
            let payer_key = StorageKey::PayerOnTimeSettled(payer.clone());
            let payer_count: u64 = env.storage().persistent().get(&payer_key).unwrap_or(0_u64);
            env.storage()
                .persistent()
                .set(&payer_key, &payer_count.saturating_add(1));
        }

        env.events().publish(
            (symbol_short!("settled"), freelancer.clone(), payer.clone()),
            SettlementAccrued {
                freelancer,
                payer,
                settled_on_time,
            },
        );
    }

    /// Claim accrued governance tokens for the caller.
    ///
    /// Mints the difference between total earned and already claimed.
    /// Uses saturating subtraction so repeated calls return 0 without error.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `claimer` - Address claiming tokens (must authorize).
    ///
    /// # Access
    /// * Restricted to the claimer via `require_auth`.
    ///
    /// # Returns
    /// * The amount of tokens minted (0 if nothing claimable).
    pub fn claim_tokens(env: Env, claimer: Address) -> i128 {
        claimer.require_auth();

        let total_earned = Self::total_earned(&env, &claimer);
        let claimed_key = StorageKey::Claimed(claimer.clone());
        let already_claimed: i128 = env.storage().persistent().get(&claimed_key).unwrap_or(0);

        let claimable = total_earned.saturating_sub(already_claimed);
        if claimable <= 0 {
            return 0;
        }

        let gov_token: Address = env.storage().instance().get(&StorageKey::GovToken).unwrap();
        StellarAssetClient::new(&env, &gov_token).mint(&claimer, &claimable);

        env.storage()
            .persistent()
            .set(&claimed_key, &already_claimed.saturating_add(claimable));

        env.events().publish(
            (symbol_short!("claimed"), claimer.clone()),
            TokensClaimed {
                claimer,
                amount: claimable,
            },
        );

        claimable
    }

    /// Get the total governance tokens earned by a participant.
    ///
    /// Computes rewards from LP volume, freelancer settlements, and on-time payer
    /// settlements using current reward rates. May differ from previously claimed
    /// amounts if rates have changed since claiming.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `participant` - Address to query.
    ///
    /// # Returns
    /// * Total earned in governance token stroops.
    pub fn get_accrual(env: Env, participant: Address) -> i128 {
        Self::total_earned(&env, &participant)
    }

    fn total_earned(env: &Env, participant: &Address) -> i128 {
        let lp_volume: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::LpFundedVolume(participant.clone()))
            .unwrap_or(0);
        let freelancer_settled: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::FreelancerSettled(participant.clone()))
            .unwrap_or(0_u64);
        let payer_on_time: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::PayerOnTimeSettled(participant.clone()))
            .unwrap_or(0_u64);

        let lp_reward_rate: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::LpRewardRate)
            .unwrap_or(DEFAULT_LP_REWARD_RATE);
        let freelancer_reward_rate: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::FreelancerRewardRate)
            .unwrap_or(DEFAULT_FREELANCER_REWARD_RATE);
        let payer_reward_rate: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::PayerRewardRate)
            .unwrap_or(DEFAULT_PAYER_REWARD_RATE);

        let lp_reward = lp_volume
            .saturating_div(HUNDRED_USDC_STROOPS)
            .saturating_mul(lp_reward_rate);
        let freelancer_reward = (freelancer_settled as i128).saturating_mul(freelancer_reward_rate);
        let payer_reward = (payer_on_time as i128).saturating_mul(payer_reward_rate);

        lp_reward
            .saturating_add(freelancer_reward)
            .saturating_add(payer_reward)
    }

/// Set LP reward rate (requires governance contract authorization).
///
/// Access: Anyone
pub fn set_lp_reward_rate(env: Env, new_rate: i128) {
        Self::require_governance_invoker(&env);
        let old_rate: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::LpRewardRate)
            .unwrap_or(DEFAULT_LP_REWARD_RATE);
        env.storage()
            .instance()
            .set(&StorageKey::LpRewardRate, &new_rate);
        env.events().publish(
            (symbol_short!("rw_upd"),),
            RewardRateUpdated {
                rate_type: Symbol::new(&env, "lp_reward"),
                old_rate,
                new_rate,
            },
        );
    }

/// Set freelancer reward rate (requires governance contract authorization).
///
/// Access: Anyone
pub fn set_freelancer_reward_rate(env: Env, new_rate: i128) {
        Self::require_governance_invoker(&env);
        let old_rate: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::FreelancerRewardRate)
            .unwrap_or(DEFAULT_FREELANCER_REWARD_RATE);
        env.storage()
            .instance()
            .set(&StorageKey::FreelancerRewardRate, &new_rate);
        env.events().publish(
            (symbol_short!("rw_upd"),),
            RewardRateUpdated {
                rate_type: Symbol::new(&env, "freelancer_reward"),
                old_rate,
                new_rate,
            },
        );
    }

/// Set payer reward rate (requires governance contract authorization).
///
/// Access: Anyone
pub fn set_payer_reward_rate(env: Env, new_rate: i128) {
        Self::require_governance_invoker(&env);
        let old_rate: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::PayerRewardRate)
            .unwrap_or(DEFAULT_PAYER_REWARD_RATE);
        env.storage()
            .instance()
            .set(&StorageKey::PayerRewardRate, &new_rate);
        env.events().publish(
            (symbol_short!("rw_upd"),),
            RewardRateUpdated {
                rate_type: Symbol::new(&env, "payer_reward"),
                old_rate,
                new_rate,
            },
        );
    }

/// Get current LP reward rate.
///
/// Access: Anyone
pub fn get_lp_reward_rate(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::LpRewardRate)
            .unwrap_or(DEFAULT_LP_REWARD_RATE)
    }

/// Get current freelancer reward rate.
///
/// Access: Anyone
pub fn get_freelancer_reward_rate(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::FreelancerRewardRate)
            .unwrap_or(DEFAULT_FREELANCER_REWARD_RATE)
    }

/// Get current payer reward rate.
///
/// Access: Anyone
pub fn get_payer_reward_rate(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::PayerRewardRate)
            .unwrap_or(DEFAULT_PAYER_REWARD_RATE)
    }

    fn require_iln_invoker(env: &Env) {
        let iln_contract: Address = env
            .storage()
            .instance()
            .get(&StorageKey::IlnContract)
            .unwrap();
        iln_contract.require_auth();
    }

    fn require_governance_invoker(env: &Env) {
        let iln_contract: Address = env
            .storage()
            .instance()
            .get(&StorageKey::IlnContract)
            .unwrap();
        iln_contract.require_auth();
    }

    /// Issue #861 — the governance token must be a SAC administered by this
    /// contract, otherwise `claim_tokens` cannot mint and every claim
    /// reverts at runtime. Panics with a stable message so deployment tooling
    /// and tests can assert on it; also exposed as `verify_mint_authority`.
    fn require_gov_token_mint_authority(env: &Env, gov_token: &Address) {
        let admin = StellarAssetClient::new(env, gov_token).admin();
        if admin != env.current_contract_address() {
            panic!("governance token SAC admin must be the distribution contract");
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, token::Client as TokenClient, Address};

    #[cfg(test)]
    use super::{HALF_TOKEN, HUNDRED_USDC_STROOPS};

    #[contract]
    pub struct MockIln;

    #[contractimpl]
    impl MockIln {
        /// `accrue_lp` contract entry point.
///
/// Access: Anyone
pub fn accrue_lp(env: Env, dist: Address, lp: Address, amount: i128) {
            IlnDistributionClient::new(&env, &dist).accrue_lp(&lp, &amount);
        }

        /// `accrue_settlement` contract entry point.
///
/// Access: Anyone
pub fn accrue_settlement(
            env: Env,
            dist: Address,
            freelancer: Address,
            payer: Address,
            on_time: bool,
        ) {
            IlnDistributionClient::new(&env, &dist).accrue_settlement(
                &freelancer,
                &payer,
                &on_time,
            );
        }
    }

    #[test]
    fn lp_earns_on_funding_and_cannot_double_claim() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);
        let iln = MockIlnClient::new(&env, &iln_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        let gov_token = gov_token_id.address();
        let token_client = TokenClient::new(&env, &gov_token);

        dist.initialize(&iln_id, &gov_token);

        let lp = Address::generate(&env);
        iln.accrue_lp(&dist_id, &lp, &HUNDRED_USDC_STROOPS);

        let claimed = dist.claim_tokens(&lp);
        assert_eq!(claimed, 10_000_000);
        assert_eq!(token_client.balance(&lp), 10_000_000);

        let second_claim = dist.claim_tokens(&lp);
        assert_eq!(second_claim, 0);
        assert_eq!(token_client.balance(&lp), 10_000_000);
    }

    #[test]
    fn freelancer_and_payer_earn_on_settlement() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);
        let iln = MockIlnClient::new(&env, &iln_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        let gov_token = gov_token_id.address();
        let token_client = TokenClient::new(&env, &gov_token);

        dist.initialize(&iln_id, &gov_token);

        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);

        iln.accrue_settlement(&dist_id, &freelancer, &payer, &true);

        assert_eq!(dist.claim_tokens(&freelancer), HALF_TOKEN);
        assert_eq!(dist.claim_tokens(&payer), HALF_TOKEN);
        assert_eq!(token_client.balance(&freelancer), HALF_TOKEN);
        assert_eq!(token_client.balance(&payer), HALF_TOKEN);
    }

    #[test]
    fn late_settlement_does_not_reward_payer() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);
        let iln = MockIlnClient::new(&env, &iln_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        let gov_token = gov_token_id.address();

        dist.initialize(&iln_id, &gov_token);

        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);

        iln.accrue_settlement(&dist_id, &freelancer, &payer, &false);

        assert_eq!(dist.claim_tokens(&freelancer), HALF_TOKEN);
        assert_eq!(dist.claim_tokens(&payer), 0);
    }

    #[test]
    fn governance_can_update_lp_reward_rate() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        let gov_token = gov_token_id.address();

        dist.initialize(&iln_id, &gov_token);

        // Check default rate
        assert_eq!(dist.get_lp_reward_rate(), DEFAULT_LP_REWARD_RATE);

        // Update rate via governance (with ILN auth)
        dist.set_lp_reward_rate(&20_000_000);
        assert_eq!(dist.get_lp_reward_rate(), 20_000_000);
    }

    #[test]
    fn governance_can_update_freelancer_reward_rate() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        let gov_token = gov_token_id.address();

        dist.initialize(&iln_id, &gov_token);

        // Check default rate
        assert_eq!(
            dist.get_freelancer_reward_rate(),
            DEFAULT_FREELANCER_REWARD_RATE
        );

        // Update rate
        dist.set_freelancer_reward_rate(&8_000_000);
        assert_eq!(dist.get_freelancer_reward_rate(), 8_000_000);
    }

    #[test]
    fn governance_can_update_payer_reward_rate() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        let gov_token = gov_token_id.address();

        dist.initialize(&iln_id, &gov_token);

        // Check default rate
        assert_eq!(dist.get_payer_reward_rate(), DEFAULT_PAYER_REWARD_RATE);

        // Update rate
        dist.set_payer_reward_rate(&7_000_000);
        assert_eq!(dist.get_payer_reward_rate(), 7_000_000);
    }

    #[test]
    fn updated_rates_affect_reward_calculation() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);
        let iln = MockIlnClient::new(&env, &iln_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        let gov_token = gov_token_id.address();
        let token_client = TokenClient::new(&env, &gov_token);

        dist.initialize(&iln_id, &gov_token);

        let lp = Address::generate(&env);

        // Update LP reward rate to 20_000_000
        dist.set_lp_reward_rate(&20_000_000);

        // Accrue 100 USDC
        iln.accrue_lp(&dist_id, &lp, &HUNDRED_USDC_STROOPS);

        // Claim should give 20_000_000 instead of default 10_000_000
        let claimed = dist.claim_tokens(&lp);
        assert_eq!(claimed, 20_000_000);
        assert_eq!(token_client.balance(&lp), 20_000_000);
    }

    #[test]
    fn accrue_lp_rejects_negative_and_zero_amounts() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);
        let iln = MockIlnClient::new(&env, &iln_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        dist.initialize(&iln_id, &gov_token_id.address());

        let lp = Address::generate(&env);
        iln.accrue_lp(&dist_id, &lp, &0);
        iln.accrue_lp(&dist_id, &lp, &-1);
        iln.accrue_lp(&dist_id, &lp, &i128::MIN);

        assert_eq!(dist.get_accrual(&lp), 0);
        assert_eq!(dist.claim_tokens(&lp), 0);
    }

    #[test]
    fn accrue_lp_rejects_amounts_above_sanity_ceiling() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);
        let iln = MockIlnClient::new(&env, &iln_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        dist.initialize(&iln_id, &gov_token_id.address());

        let lp = Address::generate(&env);
        // Just over the ceiling and i128::MAX must not inflate accrual.
        iln.accrue_lp(&dist_id, &lp, &(MAX_LP_ACCRUAL_PER_CALL + 1));
        iln.accrue_lp(&dist_id, &lp, &i128::MAX);
        assert_eq!(dist.get_accrual(&lp), 0);

        // Boundary: exact ceiling is accepted.
        iln.accrue_lp(&dist_id, &lp, &MAX_LP_ACCRUAL_PER_CALL);
        let expected_units = MAX_LP_ACCRUAL_PER_CALL / HUNDRED_USDC_STROOPS;
        assert_eq!(
            dist.get_accrual(&lp),
            expected_units.saturating_mul(DEFAULT_LP_REWARD_RATE)
        );
    }

    /// #838 — Verify double-initialize panics (most critical error path).
    #[test]
    #[should_panic(expected = "already initialized")]
    fn initialize_rejects_double_init() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        dist.initialize(&iln_id, &gov_token_id.address());

        // Second init must panic
        dist.initialize(&iln_id, &gov_token_id.address());
    }

    /// #839 — Regression: accrue_lp rejects non-ILN caller.
    #[test]
    #[should_panic]
    fn accrue_lp_rejects_non_iln_caller() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        dist.initialize(&iln_id, &gov_token_id.address());

        let lp = Address::generate(&env);
        let random_caller = Address::generate(&env);

        // Call directly from a non-ILN address — must fail auth
        env.as_contract(&random_caller, || {
            IlnDistributionClient::new(&env, &dist_id).accrue_lp(&lp, &1000);
        });
    }

    /// #839 — Regression: accrue_settlement rejects non-ILN caller.
    #[test]
    #[should_panic]
    fn accrue_settlement_rejects_non_iln_caller() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        dist.initialize(&iln_id, &gov_token_id.address());

        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);
        let random_caller = Address::generate(&env);

        // Call directly from a non-ILN address — must fail auth
        env.as_contract(&random_caller, || {
            IlnDistributionClient::new(&env, &dist_id).accrue_settlement(
                &freelancer,
                &payer,
                &true,
            );
        });
    }

    /// Issue #861 — a governance token administered by any address other
    /// than this contract cannot be minted by `claim_tokens`, so `initialize`
    /// must reject it instead of shipping a distribution contract whose every
    /// non-empty claim reverts at runtime.
    #[test]
    #[should_panic(expected = "governance token SAC admin must be the distribution contract")]
    fn initialize_rejects_governance_token_with_foreign_sac_admin() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        // SAC administered by an unrelated address (EOA/multisig stand-in).
        let gov_token_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
        dist.initialize(&iln_id, &gov_token_id.address());
    }

    /// Issue #861 — the happy path: a SAC administered by the distribution
    /// contract passes the invariant check and `verify_mint_authority`
    /// re-confirms it against live chain state.
    #[test]
    fn verify_mint_authority_accepts_distribution_owned_sac() {
        let env = Env::default();
        env.mock_all_auths();

        let iln_id = env.register_contract(None, MockIln);
        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
        dist.initialize(&iln_id, &gov_token_id.address());

        assert!(dist.verify_mint_authority());
    }

    /// Issue #861 — `verify_mint_authority` before `initialize` proves no
    /// state was written by a skipped/failed initialization.
    #[test]
    #[should_panic(expected = "not initialized")]
    fn verify_mint_authority_panics_before_initialize() {
        let env = Env::default();
        env.mock_all_auths();

        let dist_id = env.register_contract(None, IlnDistribution);
        let dist = IlnDistributionClient::new(&env, &dist_id);

        dist.verify_mint_authority();
    }

    /// Issue #660 / #661 — property-based tests for the reward-conservation
    /// invariant documented in `docs/formal-verification-distribution.md`.
    /// Randomized sequences of accrual, reward-rate updates, and claims
    /// replace the fixed-scenario tests above to check the invariant holds
    /// under arbitrary interleavings, not just scripted rate-change cases.
    mod proptest_invariants {
        use super::*;
        use proptest::prelude::*;

        #[derive(Clone, Debug)]
        enum Op {
            AccrueLp(i128),
            AccrueSettlement(bool),
            SetLpRate(i128),
            SetFreelancerRate(i128),
            SetPayerRate(i128),
            Claim,
        }

        fn op_strategy() -> impl Strategy<Value = Op> {
            prop_oneof![
                (1i128..=1_000_000_000).prop_map(Op::AccrueLp),
                any::<bool>().prop_map(Op::AccrueSettlement),
                (0i128..=50_000_000).prop_map(Op::SetLpRate),
                (0i128..=50_000_000).prop_map(Op::SetFreelancerRate),
                (0i128..=50_000_000).prop_map(Op::SetPayerRate),
                Just(Op::Claim),
            ]
        }

        struct DistEnv {
            dist: IlnDistributionClient<'static>,
            iln: MockIlnClient<'static>,
            dist_id: Address,
            token: TokenClient<'static>,
            participant: Address,
        }

        fn setup_dist() -> DistEnv {
            let env = Env::default();
            env.mock_all_auths();

            let iln_id = env.register_contract(None, MockIln);
            let dist_id = env.register_contract(None, IlnDistribution);
            let dist = IlnDistributionClient::new(&env, &dist_id);
            let iln = MockIlnClient::new(&env, &iln_id);

            let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
            let token = TokenClient::new(&env, &gov_token_id.address());

            dist.initialize(&iln_id, &gov_token_id.address());

            let participant = Address::generate(&env);

            DistEnv {
                dist,
                iln,
                dist_id,
                token,
                participant,
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]

            /// Invariant D1 (docs/formal-verification-distribution.md § 2):
            /// cumulative tokens minted to a participant never exceeds the
            /// *highest* total_earned() value ever observed up to that
            /// point, for any interleaving of accrual, rate updates, and
            /// claims.
            ///
            /// This is a high-water-mark bound, not a live one: total_earned
            /// is recomputed from the *current* reward rate against
            /// cumulative historical volume/settlement counts, so a rate cut
            /// can transiently drop the live total_earned below an amount
            /// already claimed at a higher historical rate (see the
            /// documented residual risk) — that's expected, and is exactly
            /// why this test tracks the running maximum rather than
            /// asserting against the live value at every step.
            #[test]
            fn prop_cumulative_claims_never_exceed_earned(
                ops in proptest::collection::vec(op_strategy(), 1..40),
            ) {
                let t = setup_dist();
                let mut high_water_earned: i128 = 0;
                for op in ops {
                    match op {
                        Op::AccrueLp(amount) => {
                            t.iln.accrue_lp(&t.dist_id, &t.participant, &amount);
                        }
                        Op::AccrueSettlement(on_time) => {
                            t.iln.accrue_settlement(
                                &t.dist_id,
                                &t.participant,
                                &t.participant,
                                &on_time,
                            );
                        }
                        Op::SetLpRate(rate) => {
                            t.dist.set_lp_reward_rate(&rate);
                        }
                        Op::SetFreelancerRate(rate) => {
                            t.dist.set_freelancer_reward_rate(&rate);
                        }
                        Op::SetPayerRate(rate) => {
                            t.dist.set_payer_reward_rate(&rate);
                        }
                        Op::Claim => {
                            t.dist.claim_tokens(&t.participant);
                        }
                    }
                    let cumulative_claimed = t.token.balance(&t.participant);
                    let total_earned = t.dist.get_accrual(&t.participant);
                    high_water_earned = high_water_earned.max(total_earned);
                    prop_assert!(cumulative_claimed <= high_water_earned);
                }
            }

            /// The cumulative amount minted to a participant (their gov-token
            /// balance) never decreases across a randomized operation
            /// sequence, including across rate cuts — claim_tokens only ever
            /// mints, it never burns or claws back a prior payout.
            #[test]
            fn prop_claimed_high_water_mark_is_monotonic(
                ops in proptest::collection::vec(op_strategy(), 1..40),
            ) {
                let t = setup_dist();
                let mut prev_balance: i128 = 0;
                for op in ops {
                    match op {
                        Op::AccrueLp(amount) => {
                            t.iln.accrue_lp(&t.dist_id, &t.participant, &amount);
                        }
                        Op::AccrueSettlement(on_time) => {
                            t.iln.accrue_settlement(
                                &t.dist_id,
                                &t.participant,
                                &t.participant,
                                &on_time,
                            );
                        }
                        Op::SetLpRate(rate) => {
                            t.dist.set_lp_reward_rate(&rate);
                        }
                        Op::SetFreelancerRate(rate) => {
                            t.dist.set_freelancer_reward_rate(&rate);
                        }
                        Op::SetPayerRate(rate) => {
                            t.dist.set_payer_reward_rate(&rate);
                        }
                        Op::Claim => {
                            t.dist.claim_tokens(&t.participant);
                        }
                    }
                    let balance = t.token.balance(&t.participant);
                    prop_assert!(balance >= prev_balance);
                    prev_balance = balance;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests_distribution_proptest;
