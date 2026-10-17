//! ILN Governance Contract
//!
//! Issue #59 — GovernanceProposal struct with full spec fields.
//! Issue #61 — cast_vote() with anti-double-vote protection and VoteCast event.
//! Issue #64 — delegate_votes() / undelegate_votes() with transitive delegation
//!             and cycle detection.
//! Issue #68 — veto_proposal() admin emergency block with governance-controlled
//!             disable mechanism.

#![no_std]

#[cfg(test)]
extern crate std;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token::Client as TokenClient, vec,
    Address, BytesN, Env, IntoVal, Symbol, Vec,
};

/// Vote receipts only need to outlive the active voting window.
const VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS: u32 = 50_000;
const VOTE_RECEIPT_TTL_LEDGERS: u32 = 69_120;
/// Default minimum quorum = 10% (1000 bps).
const DEFAULT_MIN_QUORUM_BPS: u32 = 1_000;
/// Default voting window: 3 days at ~5 s/ledger ≈ 51_840 ledgers.
/// Expressed in seconds to match `env.ledger().timestamp()`.
const VOTING_PERIOD_SECS: u64 = 259_200;
/// Default minimum token balance required to submit a proposal (1 000 stroops).
const DEFAULT_MIN_PROPOSAL_BALANCE: i128 = 1_000;
/// Issue #814: default forfeitable proposal deposit (0 = disabled, backwards
/// compatible). Governance can raise via `set_min_proposal_deposit`.
const DEFAULT_PROPOSAL_DEPOSIT: i128 = 0;

/// Default maximum transitive delegation chain depth.
const DEFAULT_MAX_DELEGATION_DEPTH: u32 = 10;

// ================================================================
// Governance error enum
// ================================================================

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GovernanceError {
    AlreadyInitialized = 1,
    ProposalNotFound = 2,
    VotingEnded = 3,
    ProposalNotActive = 4,
    NoVotingPower = 5,
    AlreadyVoted = 6,
    VotingOngoing = 7,
    QuorumNotReached = 8,
    ProposalRejected = 9,
    AlreadyResolved = 10,
    /// Issue #64: Delegating to self is not allowed.
    CannotDelegateToSelf = 11,
    /// Issue #64: Delegation would create a cycle.
    DelegationCyclePrevented = 12,
    TimelockNotExpired = 13,
    Unauthorized = 14,
    /// Invalid quorum basis points (must be 1..=10_000).
    InvalidQuorumBps = 15,
    /// Issue #68: caller is not the admin.
    NotAdmin = 16,
    /// Issue #68: proposal cannot be vetoed in its current status.
    NotVetoable = 17,
    /// Issue #68: admin veto power has been disabled by governance.
    VetoPowerDisabled = 18,
    /// Proposer does not hold the minimum required token balance.
    InsufficientProposerBalance = 19,
    /// Issue #531: the cross-contract execution call failed. The proposal
    /// remains in `Passed` status so `execute_proposal` can be retried.
    ExecutionFailed = 20,
    /// Delegation chain exceeds the maximum depth cap.
    MaxDelegationDepthExceeded = 21,
    /// Issue #642: veto multisig has not been configured yet.
    VetoMultisigNotConfigured = 22,
    /// Issue #642: caller is not a configured veto signer.
    NotVetoSigner = 23,
    /// Issue #642: this signer has already approved the veto for this proposal.
    VetoAlreadyApproved = 24,
    /// Issue #642: signer list/threshold combination is invalid (empty
    /// signer set, duplicate signer, or threshold outside `1..=signers.len()`).
    InvalidVetoMultisigConfig = 25,
    /// Issue #814: configured deposit amount is invalid (negative).
    InvalidProposalDeposit = 26,
    /// Issue #814: deposit for this proposal was already settled (refunded
    /// or forfeited) — prevents double-refund / double-forfeit.
    DepositAlreadySettled = 27,
    /// #844: contract called before `initialize()`.
    NotInitialized = 28,
    /// #1000: the proposal was vetoed (cancelled) and can never execute.
    ProposalCancelled = 29,
    /// #1000: a later proposal of the same action kind already executed, so
    /// this stale passed proposal must not overwrite the newer outcome.
    ProposalSuperseded = 30,
    /// #1003: the quorum denominator (token total supply) or quorum bps is
    /// missing / non-positive — finalization fails closed instead of
    /// treating the quorum as zero.
    QuorumDataMissing = 31,
    /// #1004: stored vote tallies are inconsistent (negative or overflowing).
    InvalidVoteAccounting = 32,
    /// #1006: the id is within `ProposalCount` but its storage entry is
    /// missing (expired/archived) or does not match the requested id.
    ProposalStorageCorrupt = 33,
}

// ================================================================
// ProposalAction
// ================================================================

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalAction {
    UpdateFeeRate(u32),
    /// Add a token to the allowlist. The tuple carries the token address and
    /// its decimal precision (e.g. 6 for USDC, 7 for XLM) — required since
    /// Issue #23 introduced the token decimals registry.
    AddToken(Address, u32),
    RemoveToken(Address),
    UpdateMaxDiscountRate(u32),
    /// Issue #545: Update reputation decay parameters on the ILN contract.
    /// Tuple: (rate_bps, period_ledgers)
    UpdateDecayParams(u32, u64),
    /// Issue #544: Update distribution reward parameters.
    /// Tuple: (half_token, hundred_usdc_stroops, lp_multiplier)
    UpdateDistributionRewardParams(i128, i128, i128),
    /// Issue #533: Update fee tier configuration on the ILN contract.
    UpdateFeeTiers(Vec<FeeTierConfig>),
    /// Issue #539: Upgrade the ILN contract WASM via governance vote.
    Upgrade(BytesN<32>),
    /// Update LP reward rate (in stroops per 100 USDC volume).
    UpdateLpRewardRate(i128),
    /// Update freelancer reward rate (in stroops per settlement).
    UpdateFreelancerRewardRate(i128),
    /// Update payer reward rate (in stroops per on-time settlement).
    UpdatePayerRewardRate(i128),
    /// Update insurance pool coverage cap (in stroops).
    UpdateInsuranceCoverageCap(i128),
    /// Update insurance pool premium rates (in bps).
    UpdateInsurancePremiumRate(u32),
    /// Issue #532: register (or update) the default oracle for a feed type
    /// on the ILN contract's oracle registry.
    RegisterOracle(OracleFeedType, Address),
    /// Issue #532: remove the default oracle for a feed type from the ILN
    /// contract's oracle registry.
    RemoveOracle(OracleFeedType),
    /// Issue #532: register (or update) a per-token override oracle for a
    /// feed type on the ILN contract's oracle registry. Tuple: (feed_type,
    /// token, oracle) — takes priority over the feed-type-wide default
    /// registered via `RegisterOracle` when resolving the oracle for this
    /// exact token.
    RegisterTokenOracle(OracleFeedType, Address, Address),
    /// Issue #704: update reputation_bonus contract parameters.
    /// Tuple: (high_rep_threshold, bonus_bps, min_discount_rate_bps) —
    /// mirrors reputation_bonus::config::Config's fields.
    UpdateReputationBonusParams(u32, u32, u32),
    /// Issue #655: update the ILN contract's per-invoice size cap for a
    /// staged mainnet rollout (0 = uncapped). Raised over time as
    /// confidence in the deployment grows.
    UpdateMaxInvoiceAmount(i128),
    /// Issue #655: update the ILN contract's cumulative funded-volume cap
    /// for a given token, for a staged mainnet rollout (0 = uncapped).
    /// Tuple: (token, cap).
    UpdateTokenVolumeCap(Address, i128),
}

/// Issue #532: mirrors `invoice_liquidity::oracle_registry::OracleFeedType`.
/// Soroban contracts share no Rust types across crates — cross-contract
/// calls decode structurally (same unit-variant names, same order), the
/// same way `FeeTierConfig` below mirrors the ILN contract's fee tier
/// struct. Keep variant names/order in sync with the ILN contract's enum.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OracleFeedType {
    Price,
    Identity,
    Credit,
}

/// Issue #533: Fee tier configuration.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeTierConfig {
    /// Minimum invoice amount for this tier (inclusive, in stroops).
    pub min_amount: i128,
    /// Fee rate in basis points for this tier.
    pub fee_rate_bps: u32,
}

// ================================================================
// ProposalStatus
// ================================================================

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalStatus {
    Active,
    Passed,
    Rejected,
    Executed,
    /// Issue #68: proposal was blocked by the admin via veto_proposal().
    Vetoed,
}

// ================================================================
// GovernanceProposal struct
// ================================================================

#[contracttype]
#[derive(Clone, Debug)]
pub struct GovernanceProposal {
    pub id: u64,
    pub proposer: Address,
    pub description_hash: BytesN<32>,
    pub action_type: ProposalAction,
    pub proposed_value: i128,
    pub status: ProposalStatus,
    pub votes_for: i128,
    pub votes_against: i128,
    pub created_at: u64,
    pub voting_end: u64,
    pub eta_ledger: u32,
}

// ================================================================
// Events
// ================================================================

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VoteCast {
    pub proposal_id: u64,
    pub voter: Address,
    pub support: bool,
    pub weight: i128,
}

/// Issue #64
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VotesDelegated {
    pub delegator: Address,
    pub delegate: Address,
}

/// Issue #64
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VotesUndelegated {
    pub delegator: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalExecuted {
    pub proposal_id: u64,
    pub action_type: ProposalAction,
    pub proposed_value: i128,
    pub votes_for: i128,
    pub votes_against: i128,
}

/// Issue #531: emitted when a proposal's cross-contract execution call
/// fails. The proposal remains `Passed` (not `Executed`) so a subsequent
/// `execute_proposal` call can retry it.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalExecutionFailed {
    pub proposal_id: u64,
    pub action_type: ProposalAction,
}

/// Issue #68: emitted when the admin vetoes a proposal.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalVetoed {
    pub proposal_id: u64,
    pub admin: Address,
    pub reason_hash: BytesN<32>,
}

/// Emitted when a new governance proposal is created.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalCreated {
    pub proposal_id: u64,
    pub proposer: Address,
    pub action_type: ProposalAction,
    pub proposed_value: i128,
    pub voting_end: u64,
}

/// Emitted once, when the contract is initialised (Issue #538).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct GovernanceInitialized {
    pub iln_contract: Address,
    pub gov_token: Address,
    pub admin: Address,
}

/// Emitted whenever a governance-controlled numeric parameter changes
/// (Issue #538: event emission completeness audit).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct GovernanceParameterUpdated {
    pub param_name: Symbol,
    pub old_value: i128,
    pub new_value: i128,
}

/// Emitted when admin veto power is permanently disabled (Issue #68 / #538).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VetoPowerDisabled {
    pub disabled_by: Address,
}

/// Issue #642: emitted when the veto multisig signer set / threshold is
/// (re)configured.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VetoMultisigConfigured {
    pub signers: Vec<Address>,
    pub threshold: u32,
}

/// Issue #814: emitted when a proposal deposit is escrowed at creation.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalDepositEscrowed {
    pub proposal_id: u64,
    pub proposer: Address,
    pub amount: i128,
}

/// Issue #814: emitted when a proposal deposit is refunded (Passed/Executed).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalDepositRefunded {
    pub proposal_id: u64,
    pub proposer: Address,
    pub amount: i128,
}

/// Issue #814: emitted when a proposal deposit is forfeited (Rejected/
/// expired-without-quorum / Vetoed). `sink` is the forfeiture destination
/// when one is configured, otherwise `None` (funds remain locked in the
/// governance contract, still unrecoverable by the proposer).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalDepositForfeited {
    pub proposal_id: u64,
    pub proposer: Address,
    pub amount: i128,
    pub sink: Option<Address>,
}

/// Issue #642: emitted each time a configured veto signer approves a
/// pending veto that has not yet reached threshold.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VetoApproved {
    pub proposal_id: u64,
    pub signer: Address,
    pub approvals: u32,
    pub threshold: u32,
}

// ================================================================
// Storage keys
// ================================================================

#[contracttype]
pub enum StorageKey {
    IlnContract,
    GovToken,
    /// Configurable minimum participation required for proposal passing.
    /// Expressed in basis points (bps) of total supply, e.g. 1000 = 10%.
    MinQuorumBps,
    /// Issue #622: governance token total supply used as the quorum
    /// denominator. Seeded at `initialize` time and only updatable via the
    /// ILN-contract-gated `set_gov_token_total_supply` — no longer a
    /// caller-supplied `execute_proposal` argument.
    GovTokenTotalSupply,
    Proposal(u64),
    ProposalCount,
    VoteWeightSnapshot(u64, Address),
    HasVoted(u64, Address),
    /// Issue #530: `true` when vote weight is `sqrt(balance + delegated)`
    /// instead of linear. Defaults to `false` for backwards compatibility.
    QuadraticVotingEnabled,
    /// Issue #530: the actual weight applied to the tally for this voter on
    /// this proposal (post square-root transform when quadratic voting is
    /// enabled, otherwise equal to the linear balance). Recorded alongside
    /// `HasVoted` as the vote receipt.
    AppliedVoteWeight(u64, Address),
    /// Issue #64: forward delegation pointer — Delegation(X) = Y means X delegates to Y.
    Delegation(Address),
    /// Issue #64: running tally of total delegated weight pointing (transitively) at Address.
    DelegatedToMe(Address),
    ExecutionDelay,
    /// Issue #68: the admin address (set at initialise time).
    Admin,
    MaxDelegationDepth,
    /// Issue #68: when `true`, admin veto power is active; when `false`, it has been disabled.
    VetoPowerEnabled,
    /// Configurable minimum token balance a proposer must hold.
    MinProposalBalance,
    /// Issue #544: distribution contract address for reward param updates.
    DistributionContract,
    /// Issue #704: reputation_bonus contract address for
    /// UpdateReputationBonusParams execution.
    ReputationBonusContract,
    /// Issue #642: veto multisig signer set (replaces single-admin veto).
    VetoSigners,
    /// Issue #642: number of `VetoSigners` approvals required before a veto
    /// actually takes effect.
    VetoThreshold,
    /// Issue #642: signers who have already approved the pending veto of a
    /// given proposal (cleared once the veto executes).
    VetoApprovals(u64),
    /// Issue #814: governance-configurable forfeitable deposit escrowed at
    /// `create_proposal` (0 = disabled, default for backwards compatibility).
    MinProposalDeposit,
    /// Issue #814: optional forfeiture destination (treasury sink). When set,
    /// forfeited deposits are transferred there; when unset, forfeited funds
    /// stay locked in this contract (still unrecoverable by the proposer).
    ProposalDepositSink,
    /// Issue #814: escrowed deposit amount per proposal (removed on settle).
    ProposalDeposit(u64),
    /// Issue #814: `true` once a proposal's deposit has been settled
    /// (refunded or forfeited) — guards against double-refund.
    ProposalDepositSettled(u64),
    /// #1000: `(action kind) -> (created_at, proposal id)` of the most
    /// recently created proposal of that kind that has executed. One entry
    /// per action kind, so state stays bounded.
    LastExecuted(u32),
}

// ================================================================
// Contract
// ================================================================

#[contract]
pub struct GovContract;

#[contractimpl]
impl GovContract {
    // ── #844: helper to read the ILN contract address or return NotInitialized ──
    fn get_iln_contract(env: &Env) -> Result<Address, GovernanceError> {
        env.storage()
            .instance()
            .get(&StorageKey::IlnContract)
            .ok_or(GovernanceError::NotInitialized)
    }

    /// Same helper for the gov-token address — returns `NotInitialized` when
    /// the contract has not been initialized yet.
    fn get_gov_token(env: &Env) -> Result<Address, GovernanceError> {
        env.storage()
            .instance()
            .get(&StorageKey::GovToken)
            .ok_or(GovernanceError::NotInitialized)
    }

    /// Helper for the distribution contract address.
    fn get_distribution_contract(env: &Env) -> Result<Address, GovernanceError> {
        env.storage()
            .instance()
            .get(&StorageKey::DistributionContract)
            .ok_or(GovernanceError::NotInitialized)
    }

    /// Helper for the reputation bonus contract address.
    fn get_reputation_bonus_contract(env: &Env) -> Result<Address, GovernanceError> {
        env.storage()
            .instance()
            .get(&StorageKey::ReputationBonusContract)
            .ok_or(GovernanceError::NotInitialized)
    }

    /// #1006: load a proposal, failing closed. Ids that were never issued
    /// return `ProposalNotFound`; ids that were issued (<= `ProposalCount`)
    /// but whose entry is missing or carries a different id return
    /// `ProposalStorageCorrupt` rather than being treated as nonexistent.
    fn load_proposal(env: &Env, proposal_id: u64) -> Result<GovernanceProposal, GovernanceError> {
        let count: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::ProposalCount)
            .unwrap_or(0);
        if proposal_id == 0 || proposal_id > count {
            return Err(GovernanceError::ProposalNotFound);
        }
        let proposal: GovernanceProposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(GovernanceError::ProposalStorageCorrupt)?;
        if proposal.id != proposal_id {
            return Err(GovernanceError::ProposalStorageCorrupt);
        }
        Ok(proposal)
    }

    /// #1000: stable discriminant per action kind (ignores payload).
    fn action_kind(action: &ProposalAction) -> u32 {
        match action {
            ProposalAction::UpdateFeeRate(_) => 0,
            ProposalAction::AddToken(..) => 1,
            ProposalAction::RemoveToken(_) => 2,
            ProposalAction::UpdateMaxDiscountRate(_) => 3,
            ProposalAction::UpdateDecayParams(..) => 4,
            ProposalAction::UpdateDistributionRewardParams(..) => 5,
            ProposalAction::UpdateFeeTiers(_) => 6,
            ProposalAction::Upgrade(_) => 7,
            ProposalAction::UpdateLpRewardRate(_) => 8,
            ProposalAction::UpdateFreelancerRewardRate(_) => 9,
            ProposalAction::UpdatePayerRewardRate(_) => 10,
            ProposalAction::UpdateInsuranceCoverageCap(_) => 11,
            ProposalAction::UpdateInsurancePremiumRate(_) => 12,
            ProposalAction::RegisterOracle(..) => 13,
            ProposalAction::RemoveOracle(_) => 14,
            ProposalAction::RegisterTokenOracle(..) => 15,
            ProposalAction::UpdateReputationBonusParams(..) => 16,
            ProposalAction::UpdateMaxInvoiceAmount(_) => 17,
            ProposalAction::UpdateTokenVolumeCap(..) => 18,
        }
    }

    // ── Initialise ────────────────────────────────────────────────

    pub fn initialize(
        env: Env,
        iln_contract: Address,
        distribution_contract: Address,
        reputation_bonus_contract: Address,
        gov_token: Address,
        admin: Address,
        gov_token_total_supply: i128,
    ) -> Result<(), GovernanceError> {
        if env.storage().instance().has(&StorageKey::IlnContract) {
            return Err(GovernanceError::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&StorageKey::IlnContract, &iln_contract);
        env.storage()
            .instance()
            .set(&StorageKey::DistributionContract, &distribution_contract);
        env.storage().instance().set(
            &StorageKey::ReputationBonusContract,
            &reputation_bonus_contract,
        );
        env.storage()
            .instance()
            .set(&StorageKey::GovToken, &gov_token);
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::VetoPowerEnabled, &true);
        env.storage()
            .instance()
            .set(&StorageKey::MinQuorumBps, &DEFAULT_MIN_QUORUM_BPS);
        env.storage().instance().set(&StorageKey::MaxDelegationDepth, &DEFAULT_MAX_DELEGATION_DEPTH);
        env.storage()
            .instance()
            .set(&StorageKey::ProposalCount, &0_u64);
        // Issue #622: total_supply used to be a caller-supplied argument to
        // execute_proposal, letting any caller inflate or deflate it to
        // manipulate quorum. soroban-sdk 21.x's token::Client has no
        // total_supply() query (SEP-41's TokenInterface/StellarAssetInterface
        // don't expose one), so a live on-chain read isn't available here —
        // instead this value is seeded at initialize time and can only be
        // updated afterwards via set_gov_token_total_supply, which requires
        // the same iln_contract authorization as set_min_quorum_bps /
        // set_min_proposal_balance. It is no longer settable by whoever
        // happens to call execute_proposal.
        env.storage()
            .instance()
            .set(&StorageKey::GovTokenTotalSupply, &gov_token_total_supply);

        env.events().publish(
            (Symbol::new(&env, "initialized"), admin.clone()),
            GovernanceInitialized {
                iln_contract,
                gov_token,
                admin,
            },
        );
        Ok(())
    }

    /// Returns the configured minimum quorum in bps (e.g. 1000 = 10%).
    pub fn get_min_quorum_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::MinQuorumBps)
            .unwrap_or(DEFAULT_MIN_QUORUM_BPS)
    }

    pub fn get_max_delegation_depth(env: Env) -> u32 {
        env.storage().instance().get(&StorageKey::MaxDelegationDepth).unwrap_or(DEFAULT_MAX_DELEGATION_DEPTH)
    }

    pub fn set_max_delegation_depth(env: Env, max_depth: u32) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();
        let old_value: u32 = Self::get_max_delegation_depth(env.clone());
        env.storage().instance().set(&StorageKey::MaxDelegationDepth, &max_depth);
        env.events().publish((Symbol::new(&env, "max_delegation_depth_updated"),), (old_value, max_depth));
        Ok(())
    }

    /// Returns the configured governance token total supply used for quorum
    /// calculations (see Issue #622).
    pub fn get_gov_token_total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::GovTokenTotalSupply)
            .unwrap_or(0)
    }

    /// Updates the governance token total supply used for quorum
    /// calculations.
    ///
    /// Authorization: the configured ILN contract address must authorize —
    /// the same trust boundary as `set_min_quorum_bps` /
    /// `set_min_proposal_balance`. This replaces the old caller-supplied
    /// `total_supply` argument on `execute_proposal` (Issue #622): quorum's
    /// denominator can no longer be chosen by whoever happens to call
    /// execute_proposal, only by the same authority that already controls
    /// the quorum bps and proposal-balance thresholds.
    pub fn set_gov_token_total_supply(env: Env, total_supply: i128) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();

        let old_value: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::GovTokenTotalSupply)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StorageKey::GovTokenTotalSupply, &total_supply);

        let pn = Symbol::new(&env, "gov_token_total_supply");
        env.events().publish(
            (Symbol::new(&env, "parameter_updated"), pn.clone()),
            GovernanceParameterUpdated {
                param_name: pn,
                old_value,
                new_value: total_supply,
            },
        );
        Ok(())
    }

    /// Updates the minimum quorum configuration.
    ///
    /// Authorization: the configured ILN contract address must authorize.
    pub fn set_min_quorum_bps(env: Env, min_quorum_bps: u32) -> Result<(), GovernanceError> {
        if min_quorum_bps == 0 || min_quorum_bps > 10_000 {
            return Err(GovernanceError::InvalidQuorumBps);
        }

        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();

        let old_value: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinQuorumBps)
            .unwrap_or(DEFAULT_MIN_QUORUM_BPS);
        env.storage()
            .instance()
            .set(&StorageKey::MinQuorumBps, &min_quorum_bps);

        let pn = Symbol::new(&env, "min_quorum_bps");
        env.events().publish(
            (Symbol::new(&env, "parameter_updated"), pn.clone()),
            GovernanceParameterUpdated {
                param_name: pn,
                old_value: old_value as i128,
                new_value: min_quorum_bps as i128,
            },
        );
        Ok(())
    }

    // ── Issue #59 / feat/create-proposal ─────────────────────────

    pub fn create_proposal(
        env: Env,
        proposer: Address,
        action_type: ProposalAction,
        description_hash: BytesN<32>,
        proposed_value: i128,
    ) -> Result<u64, GovernanceError> {
        proposer.require_auth();

        // ── Balance check ─────────────────────────────────────────
        let token_addr = Self::get_gov_token(&env)?;
        let token = TokenClient::new(&env, &token_addr);
        let proposer_balance = token.balance(&proposer);

        let min_balance: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::MinProposalBalance)
            .unwrap_or(DEFAULT_MIN_PROPOSAL_BALANCE);

        if proposer_balance < min_balance {
            return Err(GovernanceError::InsufficientProposerBalance);
        }

        // Issue #814: forfeitable anti-spam deposit, distinct from the static
        // `min_balance` holding gate above (which a wallet can satisfy once
        // and then spam many proposals). Escrowed from the proposer now,
        // refunded on Passed/Executed, forfeited on Rejected/expiry/Vetoed.
        let deposit: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::MinProposalDeposit)
            .unwrap_or(DEFAULT_PROPOSAL_DEPOSIT);
        if deposit < 0 {
            return Err(GovernanceError::InvalidProposalDeposit);
        }
        if deposit > 0 && proposer_balance < deposit {
            return Err(GovernanceError::InsufficientProposerBalance);
        }

        let count: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::ProposalCount)
            .unwrap_or(0);
        let id = count.saturating_add(1);

        let now = env.ledger().timestamp();
        let voting_end = now.saturating_add(VOTING_PERIOD_SECS);

        let proposal = GovernanceProposal {
            id,
            proposer: proposer.clone(),
            description_hash,
            action_type: action_type.clone(),
            proposed_value,
            status: ProposalStatus::Active,
            votes_for: 0,
            votes_against: 0,
            created_at: now,
            voting_end,
            eta_ledger: 0,
        };

        // Snapshot the proposer's balance at proposal creation time.
        env.storage().persistent().set(
            &StorageKey::VoteWeightSnapshot(id, proposer.clone()),
            &proposer_balance,
        );

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(id), &proposal);
        env.storage()
            .instance()
            .set(&StorageKey::ProposalCount, &id);

        // Issue #814: escrow the deposit after persisting the proposal so a
        // failed transfer rolls the whole creation back atomically.
        if deposit > 0 {
            let this = env.current_contract_address();
            token.transfer(&proposer, &this, &deposit);
            env.storage()
                .persistent()
                .set(&StorageKey::ProposalDeposit(id), &deposit);
            env.events().publish(
                (Symbol::new(&env, "proposal_deposit_escrowed"), id),
                ProposalDepositEscrowed {
                    proposal_id: id,
                    proposer: proposer.clone(),
                    amount: deposit,
                },
            );
        }

        env.events().publish(
            (Symbol::new(&env, "proposal_created"), id, proposer.clone()),
            ProposalCreated {
                proposal_id: id,
                proposer,
                action_type,
                proposed_value,
                voting_end,
            },
        );

        Ok(id)
    }

    // ── Issue #814: forfeitable proposal deposit ───────────────────

    /// Returns the governance-configurable forfeitable deposit escrowed at
    /// `create_proposal`. `0` (default) disables the escrow for backwards
    /// compatibility — only the static `MinProposalBalance` gate applies.
    pub fn get_min_proposal_deposit(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::MinProposalDeposit)
            .unwrap_or(DEFAULT_PROPOSAL_DEPOSIT)
    }

    /// Updates the forfeitable proposal deposit amount.
    ///
    /// Authorization: the configured ILN contract address must authorize
    /// (same pattern as `set_min_quorum_bps` / `set_min_proposal_balance`).
    pub fn set_min_proposal_deposit(env: Env, amount: i128) -> Result<(), GovernanceError> {
        if amount < 0 {
            return Err(GovernanceError::InvalidProposalDeposit);
        }
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();

        let old_value: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::MinProposalDeposit)
            .unwrap_or(DEFAULT_PROPOSAL_DEPOSIT);
        env.storage()
            .instance()
            .set(&StorageKey::MinProposalDeposit, &amount);

        let pn = Symbol::new(&env, "min_proposal_deposit");
        env.events().publish(
            (Symbol::new(&env, "parameter_updated"), pn.clone()),
            GovernanceParameterUpdated {
                param_name: pn,
                old_value,
                new_value: amount,
            },
        );
        Ok(())
    }

    /// Returns the configured forfeiture sink (treasury address), if any.
    /// When unset, forfeited deposits stay locked in this contract.
    pub fn get_proposal_deposit_sink(env: Env) -> Option<Address> {
        env.storage().instance().get(&StorageKey::ProposalDepositSink)
    }

    /// Sets (or, when `sink` is `None`, clears) the forfeiture destination.
    ///
    /// Authorization: the configured ILN contract address must authorize.
    /// Decision (Issue #814): forfeited deposits go to this treasury sink
    /// rather than the insurance pool — the insurance pool prices coverage
    /// risk, while spam penalties are treasury revenue; mixing them would
    /// distort pool accounting. Documented in
    /// `docs/governance-security-summary.md`.
    pub fn set_proposal_deposit_sink(
        env: Env,
        sink: Option<Address>,
    ) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();
        env.storage()
            .instance()
            .set(&StorageKey::ProposalDepositSink, &sink);
        Ok(())
    }

    /// Returns the escrowed (not yet settled) deposit for `proposal_id`,
    /// or `0` when none was escrowed or it was already settled.
    pub fn get_proposal_deposit(env: Env, proposal_id: u64) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::ProposalDeposit(proposal_id))
            .unwrap_or(0)
    }

    /// Returns `true` once a proposal's deposit has been settled (refunded
    /// or forfeited). Never-set deposits report `false` until a settlement
    /// is recorded — callers should check `get_proposal_deposit` together
    /// with this flag.
    pub fn is_proposal_deposit_settled(env: Env, proposal_id: u64) -> bool {
        env.storage()
            .persistent()
            .get(&StorageKey::ProposalDepositSettled(proposal_id))
            .unwrap_or(false)
    }

    /// Refund the escrowed deposit to the proposer. Idempotent: a second
    /// call after settlement is a no-op returning `false` (no second
    /// transfer), which is what prevents double-refunds on the
    /// `Passed -> Executed` second `execute_proposal` call.
    fn refund_proposal_deposit(env: &Env, proposal_id: u64, proposer: &Address) -> Result<bool, GovernanceError> {
        if env
            .storage()
            .persistent()
            .get::<StorageKey, bool>(&StorageKey::ProposalDepositSettled(proposal_id))
            .unwrap_or(false)
        {
            return Ok(false);
        }
        let amount: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::ProposalDeposit(proposal_id))
            .unwrap_or(0);
        // Mark settled before transferring so a re-entrant retry cannot
        // double-pay even if the token call traps midway (the whole frame
        // would roll back, but the flag makes the intent explicit).
        env.storage()
            .persistent()
            .set(&StorageKey::ProposalDepositSettled(proposal_id), &true);
        if amount <= 0 {
            env.storage()
                .persistent()
                .remove(&StorageKey::ProposalDeposit(proposal_id));
            return Ok(false);
        }
        env.storage()
            .persistent()
            .remove(&StorageKey::ProposalDeposit(proposal_id));
        let token_addr = Self::get_gov_token(env)?;
        let token = TokenClient::new(env, &token_addr);
        let this = env.current_contract_address();
        token.transfer(&this, proposer, &amount);
        env.events().publish(
            (Symbol::new(env, "proposal_deposit_refunded"), proposal_id),
            ProposalDepositRefunded {
                proposal_id,
                proposer: proposer.clone(),
                amount,
            },
        );
        Ok(true)
    }

    /// Forfeit the escrowed deposit to the configured sink (or lock it in
    /// this contract when no sink is set). Idempotent like the refund path.
    fn forfeit_proposal_deposit(env: &Env, proposal_id: u64, proposer: &Address) -> Result<bool, GovernanceError> {
        if env
            .storage()
            .persistent()
            .get::<StorageKey, bool>(&StorageKey::ProposalDepositSettled(proposal_id))
            .unwrap_or(false)
        {
            return Ok(false);
        }
        let amount: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::ProposalDeposit(proposal_id))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&StorageKey::ProposalDepositSettled(proposal_id), &true);
        if amount <= 0 {
            env.storage()
                .persistent()
                .remove(&StorageKey::ProposalDeposit(proposal_id));
            return Ok(false);
        }
        env.storage()
            .persistent()
            .remove(&StorageKey::ProposalDeposit(proposal_id));
        let sink: Option<Address> = env.storage().instance().get(&StorageKey::ProposalDepositSink);
        if let Some(dest) = sink.clone() {
            let token_addr = Self::get_gov_token(env)?;
            let token = TokenClient::new(env, &token_addr);
            let this = env.current_contract_address();
            token.transfer(&this, &dest, &amount);
        }
        env.events().publish(
            (Symbol::new(env, "proposal_deposit_forfeited"), proposal_id),
            ProposalDepositForfeited {
                proposal_id,
                proposer: proposer.clone(),
                amount,
                sink,
            },
        );
        Ok(true)
    }

    // ── Issue #530: quadratic voting toggle ───────────────────────

    /// Returns whether quadratic voting (`sqrt(balance + delegated)` weight)
    /// is enabled. Defaults to `false` (linear weighting) for backwards
    /// compatibility with proposals created before this feature existed.
    pub fn is_quadratic_voting_enabled(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&StorageKey::QuadraticVotingEnabled)
            .unwrap_or(false)
    }

    /// Enables or disables quadratic voting.
    ///
    /// Authorization: the configured ILN contract address must authorize
    /// (same governance-controlled-toggle pattern as `set_min_quorum_bps`).
    pub fn set_quadratic_voting_enabled(env: Env, enabled: bool) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();

        let old_value: bool = env
            .storage()
            .instance()
            .get(&StorageKey::QuadraticVotingEnabled)
            .unwrap_or(false);
        env.storage()
            .instance()
            .set(&StorageKey::QuadraticVotingEnabled, &enabled);

        let pn = Symbol::new(&env, "quadratic_voting_enabled");
        env.events().publish(
            (Symbol::new(&env, "parameter_updated"), pn.clone()),
            GovernanceParameterUpdated {
                param_name: pn,
                old_value: old_value as i128,
                new_value: enabled as i128,
            },
        );
        Ok(())
    }

    /// Returns the actual weight applied to `voter`'s vote on `proposal_id`,
    /// i.e. the vote receipt recorded by Issue #530. `None` if the address
    /// has not voted on this proposal (or its temporary-storage TTL expired).
    pub fn get_applied_vote_weight(env: Env, proposal_id: u64, voter: Address) -> Option<i128> {
        env.storage()
            .temporary()
            .get(&StorageKey::AppliedVoteWeight(proposal_id, voter))
    }

    /// Integer square root (floor) via binary search. `n` is assumed `>= 0`
    /// (token balances and delegated weight tallies are non-negative).
    fn isqrt(n: i128) -> i128 {
        if n <= 1 {
            return n.max(0);
        }
        let mut lo: i128 = 0;
        let mut hi: i128 = n;
        while lo < hi {
            let mid = lo.saturating_add(
                hi.saturating_sub(lo)
                    .saturating_add(1)
                    .saturating_div(2)
            );
            match mid.checked_mul(mid) {
                Some(sq) if sq <= n => lo = mid,
                _ => hi = mid.saturating_sub(1),
            }
        }
        lo
    }


    /// Returns the configured minimum proposer balance.
    pub fn get_min_proposal_balance(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::MinProposalBalance)
            .unwrap_or(DEFAULT_MIN_PROPOSAL_BALANCE)
    }

    /// Updates the minimum proposer balance.
    ///
    /// Authorization: the configured ILN contract address must authorize.
    pub fn set_min_proposal_balance(env: Env, min_balance: i128) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();

        let old_value: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::MinProposalBalance)
            .unwrap_or(DEFAULT_MIN_PROPOSAL_BALANCE);
        env.storage()
            .instance()
            .set(&StorageKey::MinProposalBalance, &min_balance);

        let pn = Symbol::new(&env, "min_proposal_balance");
        env.events().publish(
            (Symbol::new(&env, "parameter_updated"), pn.clone()),
            GovernanceParameterUpdated {
                param_name: pn,
                old_value,
                new_value: min_balance,
            },
        );
        Ok(())
    }

    // ── Issue #64: delegate_votes ─────────────────────────────────

    /// Delegate the caller's voting weight to `delegate`.
    ///
    /// * Cannot delegate to self.
    /// * Rejects delegation if it would create a cycle in the chain.
    /// * Re-delegation overwrites the previous delegation and adjusts the
    ///   `DelegatedToMe` tally on both old and new terminal nodes.
    ///
    /// Emits `VotesDelegated`.
    pub fn delegate_votes(
        env: Env,
        delegator: Address,
        delegate: Address,
    ) -> Result<(), GovernanceError> {
        delegator.require_auth();

        if delegator == delegate {
            return Err(GovernanceError::CannotDelegateToSelf);
        }

        // ── Cycle detection ───────────────────────────────────────
        // Walk the forward chain from `delegate`.
        // If we reach `delegator` at any point, the new edge would close a cycle.
        let max_depth = Self::get_max_delegation_depth(env.clone());
        let mut cursor: Option<Address> = Self::get_delegate_raw(&env, &delegate);
        let mut depth = 0u32;
        while let Some(ref next) = cursor.clone() {
            if depth >= max_depth {
                return Err(GovernanceError::MaxDelegationDepthExceeded);
            }
            if *next == delegator {
                return Err(GovernanceError::DelegationCyclePrevented);
            }
            cursor = Self::get_delegate_raw(&env, next);
            depth += 1;
        }

        // ── Find the terminal node for `delegate` ─────────────────
        let terminal = Self::resolve_terminal(&env, &delegate);

        // ── Remove weight from old terminal if re-delegating ──────
        if let Some(old_delegate) = Self::get_delegate_raw(&env, &delegator) {
            let old_terminal = Self::resolve_terminal(&env, &old_delegate);
            let delegator_balance = Self::get_own_balance_for_delegation(&env, &delegator)?;
            Self::adjust_delegated_to_me(&env, &old_terminal, -delegator_balance);
        }

        // ── Store forward pointer ─────────────────────────────────
        env.storage()
            .persistent()
            .set(&StorageKey::Delegation(delegator.clone()), &delegate);

        // ── Add weight to new terminal ────────────────────────────
        let delegator_balance = Self::get_own_balance_for_delegation(&env, &delegator)?;
        Self::adjust_delegated_to_me(&env, &terminal, delegator_balance);

        env.events().publish(
            (
                Symbol::new(&env, "votes_delegated"),
                delegator.clone(),
                delegate.clone(),
            ),
            VotesDelegated {
                delegator,
                delegate,
            },
        );

        Ok(())
    }

    // ── Issue #64: undelegate_votes ───────────────────────────────

    /// Remove the caller's delegation.
    ///
    /// Emits `VotesUndelegated`.
    pub fn undelegate_votes(env: Env, delegator: Address) -> Result<(), GovernanceError> {
        delegator.require_auth();

        if let Some(old_delegate) = Self::get_delegate_raw(&env, &delegator) {
            let old_terminal = Self::resolve_terminal(&env, &old_delegate);
            let delegator_balance = Self::get_own_balance_for_delegation(&env, &delegator)?;
            Self::adjust_delegated_to_me(&env, &old_terminal, -delegator_balance);

            env.storage()
                .persistent()
                .remove(&StorageKey::Delegation(delegator.clone()));
        }

        env.events().publish(
            (Symbol::new(&env, "votes_undelegated"), delegator.clone()),
            VotesUndelegated { delegator },
        );

        Ok(())
    }

    // ── Issue #64: get_delegate ───────────────────────────────────

    /// Return the direct delegate for `addr`, if any.
    pub fn get_delegate(env: Env, addr: Address) -> Option<Address> {
        Self::get_delegate_raw(&env, &addr)
    }

    // ── cast_vote ─────────────────────────────────────────────────

    /// Cast a vote on an active proposal.
    ///
    /// Issue #64: weight = own snapshot balance + DelegatedToMe tally.
    pub fn cast_vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
        support: bool,
    ) -> Result<(), GovernanceError> {
        voter.require_auth();

        let mut proposal: GovernanceProposal = Self::load_proposal(&env, proposal_id)?;

        let now = env.ledger().timestamp();
        if now >= proposal.voting_end {
            return Err(GovernanceError::VotingEnded);
        }
        if proposal.status != ProposalStatus::Active {
            return Err(GovernanceError::ProposalNotActive);
        }

        let voted_key = StorageKey::HasVoted(proposal_id, voter.clone());
        if env.storage().temporary().has(&voted_key) {
            return Err(GovernanceError::AlreadyVoted);
        }

        let token_addr = Self::get_gov_token(&env)?;
        let token = TokenClient::new(&env, &token_addr);

        // Own snapshotted (or current) balance.
        let snapshot_key = StorageKey::VoteWeightSnapshot(proposal_id, voter.clone());
        let own_balance: i128 = match env.storage().persistent().get(&snapshot_key) {
            Some(w) => w,
            None => {
                let current = token.balance(&voter);
                env.storage().persistent().set(&snapshot_key, &current);
                current
            }
        };

        // Issue #64: add delegated weight.
        let delegated: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::DelegatedToMe(voter.clone()))
            .unwrap_or(0_i128);

        let raw_weight = own_balance.saturating_add(delegated);

        // Issue #530: quadratic voting reduces whale influence by weighting
        // votes by sqrt(balance + delegated) instead of the raw balance.
        // Off by default so proposals created before this feature shipped
        // keep their original linear semantics.
        let weight = if Self::is_quadratic_voting_enabled(env.clone()) {
            Self::isqrt(raw_weight)
        } else {
            raw_weight
        };

        if weight == 0 {
            return Err(GovernanceError::NoVotingPower);
        }

        if support {
            proposal.votes_for = proposal.votes_for.saturating_add(weight);
        } else {
            proposal.votes_against = proposal.votes_against.saturating_add(weight);
        }

        env.storage().temporary().set(&voted_key, &true);
        env.storage().temporary().extend_ttl(
            &voted_key,
            VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS,
            VOTE_RECEIPT_TTL_LEDGERS,
        );

        // Issue #530: record the actual weight applied (vote receipt).
        let applied_weight_key = StorageKey::AppliedVoteWeight(proposal_id, voter.clone());
        env.storage().temporary().set(&applied_weight_key, &weight);
        env.storage().temporary().extend_ttl(
            &applied_weight_key,
            VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS,
            VOTE_RECEIPT_TTL_LEDGERS,
        );
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (Symbol::new(&env, "vote_cast"), proposal_id, voter.clone()),
            VoteCast {
                proposal_id,
                voter,
                support,
                weight,
            },
        );

        Ok(())
    }

    // ── Issue #62: set_execution_delay / get_execution_delay ──

    pub fn set_execution_delay(
        env: Env,
        admin: Address,
        delay: u32,
    ) -> Result<(), GovernanceError> {
        admin.require_auth();

        if let Some(stored_admin) = env
            .storage()
            .instance()
            .get::<StorageKey, Address>(&StorageKey::Admin)
        {
            if admin != stored_admin {
                return Err(GovernanceError::Unauthorized);
            }
        } else {
            env.storage().instance().set(&StorageKey::Admin, &admin);
        }

        let old_value: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::ExecutionDelay)
            .unwrap_or(0_u32);
        env.storage()
            .instance()
            .set(&StorageKey::ExecutionDelay, &delay);

        let pn = Symbol::new(&env, "execution_delay");
        env.events().publish(
            (Symbol::new(&env, "parameter_updated"), pn.clone()),
            GovernanceParameterUpdated {
                param_name: pn,
                old_value: old_value as i128,
                new_value: delay as i128,
            },
        );
        Ok(())
    }

    pub fn get_execution_delay(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::ExecutionDelay)
            .unwrap_or(0)
    }

    // ── execute_proposal ─────────────────────────────────────────

    pub fn execute_proposal(env: Env, proposal_id: u64) -> Result<(), GovernanceError> {
        let mut proposal: GovernanceProposal = Self::load_proposal(&env, proposal_id)?;

        // #1000: a vetoed (cancelled) proposal is dead regardless of timing.
        if proposal.status == ProposalStatus::Vetoed {
            return Err(GovernanceError::ProposalCancelled);
        }

        let now = env.ledger().timestamp();
        if now < proposal.voting_end {
            return Err(GovernanceError::VotingOngoing);
        }

        if proposal.status == ProposalStatus::Active {
            // #1004: validate vote-weight accounting before using the tallies.
            if proposal.votes_for < 0 || proposal.votes_against < 0 {
                return Err(GovernanceError::InvalidVoteAccounting);
            }
            let total_votes = proposal
                .votes_for
                .checked_add(proposal.votes_against)
                .ok_or(GovernanceError::InvalidVoteAccounting)?;
            let min_quorum_bps: u32 = env
                .storage()
                .instance()
                .get(&StorageKey::MinQuorumBps)
                .ok_or(GovernanceError::QuorumDataMissing)?;

            // Issue #622: total_supply used to be a caller-supplied argument,
            // letting a caller inflate it (lowering the effective quorum) or
            // deflate it (blocking quorum entirely). Read the contract-stored
            // value instead (seeded at initialize, only updatable via the
            // ILN-contract-gated set_gov_token_total_supply) — it can no
            // longer be chosen by whoever happens to call execute_proposal.
            //
            // #1003: a missing or non-positive supply used to yield
            // `quorum = 0`, so any proposal with one vote passed. Fail closed.
            let total_supply: i128 = env
                .storage()
                .instance()
                .get(&StorageKey::GovTokenTotalSupply)
                .unwrap_or(0);
            if total_supply <= 0 || min_quorum_bps == 0 || min_quorum_bps > 10_000 {
                return Err(GovernanceError::QuorumDataMissing);
            }
            let quorum = total_supply.saturating_mul(min_quorum_bps as i128) / 10_000_i128;

            if total_votes < quorum {
                proposal.status = ProposalStatus::Rejected;
                env.storage()
                    .persistent()
                    .set(&StorageKey::Proposal(proposal_id), &proposal);
                // Issue #814: expired without quorum — forfeit the deposit.
                Self::forfeit_proposal_deposit(&env, proposal_id, &proposal.proposer)?;
                return Err(GovernanceError::QuorumNotReached);
            }

            if proposal.votes_for <= proposal.votes_against {
                proposal.status = ProposalStatus::Rejected;
                env.storage()
                    .persistent()
                    .set(&StorageKey::Proposal(proposal_id), &proposal);
                // Issue #814: rejected — forfeit the deposit.
                Self::forfeit_proposal_deposit(&env, proposal_id, &proposal.proposer)?;
                return Err(GovernanceError::ProposalRejected);
            }

            proposal.status = ProposalStatus::Passed;

            let delay = env
                .storage()
                .instance()
                .get(&StorageKey::ExecutionDelay)
                .unwrap_or(0_u32);
            proposal.eta_ledger = env.ledger().sequence().saturating_add(delay);

            env.storage()
                .persistent()
                .set(&StorageKey::Proposal(proposal_id), &proposal);
            // Issue #814: passed — refund the deposit (idempotent, so the
            // later Passed -> Executed call does not double-pay).
            Self::refund_proposal_deposit(&env, proposal_id, &proposal.proposer)?;
            return Ok(());
        }

        if proposal.status == ProposalStatus::Passed {
            // #1000: a later proposal of the same kind already executed.
            let kind = Self::action_kind(&proposal.action_type);
            if let Some((last_created, _)) = env
                .storage()
                .persistent()
                .get::<StorageKey, (u64, u64)>(&StorageKey::LastExecuted(kind))
            {
                if last_created > proposal.created_at {
                    return Err(GovernanceError::ProposalSuperseded);
                }
            }
            let current_ledger = env.ledger().sequence();
            if current_ledger < proposal.eta_ledger {
                return Err(GovernanceError::TimelockNotExpired);
            }

            let iln_contract = Self::get_iln_contract(&env)?;

            // Issue #531: capture the outcome of the cross-contract call instead
            // of firing-and-forgetting it. A failed call (callee panics / returns
            // an error) must NOT mark the proposal `Executed` — it stays `Passed`
            // so a subsequent `execute_proposal` call can retry it.
            let succeeded = match proposal.action_type.clone() {
                ProposalAction::UpdateFeeRate(rate) => {
                    let args: Vec<soroban_sdk::Val> = vec![&env, rate.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "update_fee_rate", args)
                }
                ProposalAction::AddToken(token, decimals) => {
                    let args: Vec<soroban_sdk::Val> =
                        vec![&env, token.into_val(&env), decimals.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "add_token", args)
                }
                ProposalAction::RemoveToken(token) => {
                    let args: Vec<soroban_sdk::Val> = vec![&env, token.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "remove_token", args)
                }
                ProposalAction::UpdateMaxDiscountRate(rate) => {
                    let args: Vec<soroban_sdk::Val> = vec![&env, rate.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "update_max_discount", args)
                }
                ProposalAction::UpdateDecayParams(rate_bps, period_ledgers) => {
                    let args: Vec<soroban_sdk::Val> =
                        vec![&env, rate_bps.into_val(&env), period_ledgers.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "update_decay_params", args)
                }
                ProposalAction::UpdateDistributionRewardParams(
                    half_token,
                    hundred_usdc_stroops,
                    lp_multiplier,
                ) => {
                    let dist_contract = Self::get_distribution_contract(&env)?;
                    let args: Vec<soroban_sdk::Val> = vec![
                        &env,
                        half_token.into_val(&env),
                        hundred_usdc_stroops.into_val(&env),
                        lp_multiplier.into_val(&env),
                    ];
                    Self::invoke_and_check(&env, &dist_contract, "update_reward_params", args)
                }
                ProposalAction::UpdateFeeTiers(tiers) => {
                    let args: Vec<soroban_sdk::Val> = vec![&env, tiers.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "update_fee_tiers", args)
                }
                ProposalAction::Upgrade(new_wasm_hash) => {
                    let args: Vec<soroban_sdk::Val> = vec![&env, new_wasm_hash.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "upgrade", args)
                }
                ProposalAction::UpdateLpRewardRate(rate) => {
                    let dist_contract = Self::get_distribution_contract(&env)?;
                    let args: Vec<soroban_sdk::Val> = vec![&env, rate.into_val(&env)];
                    Self::invoke_and_check(&env, &dist_contract, "set_lp_reward_rate", args)
                }
                ProposalAction::UpdateFreelancerRewardRate(rate) => {
                    let dist_contract = Self::get_distribution_contract(&env)?;
                    let args: Vec<soroban_sdk::Val> = vec![&env, rate.into_val(&env)];
                    Self::invoke_and_check(&env, &dist_contract, "set_freelancer_reward_rate", args)
                }
                ProposalAction::UpdatePayerRewardRate(rate) => {
                    let dist_contract = Self::get_distribution_contract(&env)?;
                    let args: Vec<soroban_sdk::Val> = vec![&env, rate.into_val(&env)];
                    Self::invoke_and_check(&env, &dist_contract, "set_payer_reward_rate", args)
                }
                ProposalAction::UpdateInsuranceCoverageCap(cap) => {
                    let insurance_contract = Self::get_iln_contract(&env)?;
                    let args: Vec<soroban_sdk::Val> = vec![&env, cap.into_val(&env)];
                    Self::invoke_and_check(
                        &env,
                        &insurance_contract,
                        "set_coverage_via_governance",
                        args,
                    )
                }
                ProposalAction::UpdateInsurancePremiumRate(rate) => {
                    let insurance_contract = Self::get_iln_contract(&env)?;
                    let args: Vec<soroban_sdk::Val> = vec![&env, rate.into_val(&env)];
                    Self::invoke_and_check(
                        &env,
                        &insurance_contract,
                        "set_premium_rate_via_governance",
                        args,
                    )
                }
                ProposalAction::UpdateReputationBonusParams(
                    high_rep_threshold,
                    bonus_bps,
                    min_discount_rate_bps,
                ) => {
                    let rep_contract = Self::get_reputation_bonus_contract(&env)?;
                    // update_config's `caller` param is checked against the
                    // reputation_bonus contract's stored admin — that admin
                    // must be set to this governance contract's own address
                    // at deployment time for this call to authorize.
                    let args: Vec<soroban_sdk::Val> = vec![
                        &env,
                        env.current_contract_address().into_val(&env),
                        high_rep_threshold.into_val(&env),
                        bonus_bps.into_val(&env),
                        min_discount_rate_bps.into_val(&env),
                    ];
                    Self::invoke_and_check(&env, &rep_contract, "update_config", args)
                }
                ProposalAction::RegisterOracle(feed_type, oracle) => {
                    let args: Vec<soroban_sdk::Val> =
                        vec![&env, feed_type.into_val(&env), oracle.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "register_oracle", args)
                }
                ProposalAction::RemoveOracle(feed_type) => {
                    let args: Vec<soroban_sdk::Val> = vec![&env, feed_type.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "remove_oracle", args)
                }
                ProposalAction::RegisterTokenOracle(feed_type, token, oracle) => {
                    let args: Vec<soroban_sdk::Val> = vec![
                        &env,
                        feed_type.into_val(&env),
                        token.into_val(&env),
                        oracle.into_val(&env),
                    ];
                    Self::invoke_and_check(&env, &iln_contract, "register_token_oracle", args)
                }
                ProposalAction::UpdateMaxInvoiceAmount(cap) => {
                    let args: Vec<soroban_sdk::Val> = vec![&env, cap.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "set_max_invoice_amount", args)
                }
                ProposalAction::UpdateTokenVolumeCap(token, cap) => {
                    let args: Vec<soroban_sdk::Val> =
                        vec![&env, token.into_val(&env), cap.into_val(&env)];
                    Self::invoke_and_check(&env, &iln_contract, "set_token_volume_cap", args)
                }
            };

            if !succeeded {
                env.events().publish(
                    (Symbol::new(&env, "proposal_execution_failed"), proposal_id),
                    ProposalExecutionFailed {
                        proposal_id,
                        action_type: proposal.action_type,
                    },
                );
                // Proposal storage is untouched here — it remains `Passed` with
                // its original `eta_ledger`, so calling `execute_proposal` again
                // retries the same cross-contract call (Issue #531 retry mechanism).
                return Err(GovernanceError::ExecutionFailed);
            }

            proposal.status = ProposalStatus::Executed;
            env.storage()
                .persistent()
                .set(&StorageKey::Proposal(proposal_id), &proposal);
            env.storage().persistent().set(
                &StorageKey::LastExecuted(kind),
                &(proposal.created_at, proposal_id),
            );

            // Issue #814: executed — ensure refund (no-op if already
            // refunded at Passed time; covers zero-deposit proposals).
            Self::refund_proposal_deposit(&env, proposal_id, &proposal.proposer)?;

            env.events().publish(
                (Symbol::new(&env, "proposal_executed"), proposal_id),
                ProposalExecuted {
                    proposal_id,
                    action_type: proposal.action_type,
                    proposed_value: proposal.proposed_value,
                    votes_for: proposal.votes_for,
                    votes_against: proposal.votes_against,
                },
            );

            return Ok(());
        }

        // #1000: distinguish cancelled (vetoed) from other terminal states.
        if proposal.status == ProposalStatus::Vetoed {
            return Err(GovernanceError::ProposalCancelled);
        }
        Err(GovernanceError::AlreadyResolved)
    }

    // ── Issue #642: configure_veto_multisig ───────────────────────

    /// Configure (or reconfigure) the veto multisig signer set and the
    /// number of signer approvals required to actually execute a veto.
    ///
    /// This replaces the single-admin veto with a multisig-gated one
    /// (Issue #642 / threat model "admin single point of failure" finding):
    /// once configured, no single key — including the stored `Admin` — can
    /// unilaterally block a governance proposal via `veto_proposal`.
    ///
    /// Authorization:
    /// * First call (bootstrap): the stored `Admin` address must authorize,
    ///   since no multisig authority exists yet to gate it instead.
    /// * Subsequent calls (reconfiguration): the configured `IlnContract`
    ///   address must authorize — the same governance-vote-gated pattern
    ///   used by `set_min_quorum_bps` / `disable_veto_power`. This closes
    ///   the loop: after bootstrap, the admin alone can no longer change
    ///   who holds veto power either.
    ///
    /// Emits `VetoMultisigConfigured { signers, threshold }`.
    pub fn configure_veto_multisig(
        env: Env,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), GovernanceError> {
        if signers.is_empty() || threshold == 0 || threshold > signers.len() {
            return Err(GovernanceError::InvalidVetoMultisigConfig);
        }
        for i in 0..signers.len() {
            for j in (i + 1)..signers.len() {
                if signers.get(i).unwrap() == signers.get(j).unwrap() {
                    return Err(GovernanceError::InvalidVetoMultisigConfig);
                }
            }
        }

        if env.storage().instance().has(&StorageKey::VetoSigners) {
            let iln_contract = Self::get_iln_contract(&env)?;
            iln_contract.require_auth();
        } else {
            let admin: Address = env
                .storage()
                .instance()
                .get(&StorageKey::Admin)
                .ok_or(GovernanceError::NotInitialized)?;
            admin.require_auth();
        }

        env.storage()
            .instance()
            .set(&StorageKey::VetoSigners, &signers);
        env.storage()
            .instance()
            .set(&StorageKey::VetoThreshold, &threshold);

        env.events().publish(
            (Symbol::new(&env, "veto_multisig_configured"),),
            VetoMultisigConfigured { signers, threshold },
        );

        Ok(())
    }

    /// Returns the configured veto multisig signer set (empty if unconfigured).
    pub fn get_veto_signers(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&StorageKey::VetoSigners)
            .unwrap_or(Vec::new(&env))
    }

    /// Returns the configured veto multisig approval threshold (0 if unconfigured).
    pub fn get_veto_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::VetoThreshold)
            .unwrap_or(0)
    }

    /// Returns the veto signers who have already approved vetoing
    /// `proposal_id`, if a veto is currently pending on it.
    pub fn get_veto_approvals(env: Env, proposal_id: u64) -> Vec<Address> {
        env.storage()
            .temporary()
            .get(&StorageKey::VetoApprovals(proposal_id))
            .unwrap_or(Vec::new(&env))
    }

    // ── Issue #68 / #642: veto_proposal ───────────────────────────

    /// Approve vetoing an active (or passed) proposal. Once `threshold`
    /// distinct configured veto signers have approved, the proposal
    /// transitions to `Vetoed` status; until then the approval is simply
    /// recorded.
    ///
    /// * `signer` must be one of the configured `VetoSigners` (Issue #642 —
    ///   no single admin key can veto unilaterally; a threshold of signers
    ///   must agree, mirroring the multisig authority used for core
    ///   contract admin actions).
    /// * The veto power must still be enabled; it cannot be used after
    ///   governance has called `disable_veto_power()`.
    /// * Only proposals in `Active` or `Passed` status can be vetoed — an
    ///   already-executed or already-vetoed proposal is not vetoable.
    ///
    /// Emits `VetoApproved` while approvals are accumulating, then
    /// `ProposalVetoed { proposal_id, admin: signer, reason_hash }` once the
    /// threshold is reached and the veto executes.
    pub fn veto_proposal(
        env: Env,
        signer: Address,
        proposal_id: u64,
        reason_hash: BytesN<32>,
    ) -> Result<(), GovernanceError> {
        signer.require_auth();

        // ── Guard: veto power must still be enabled ───────────────
        let enabled: bool = env
            .storage()
            .instance()
            .get(&StorageKey::VetoPowerEnabled)
            .unwrap_or(false);
        if !enabled {
            return Err(GovernanceError::VetoPowerDisabled);
        }

        // ── Auth: signer must be a configured veto multisig signer ─
        let signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::VetoSigners)
            .ok_or(GovernanceError::VetoMultisigNotConfigured)?;
        if !signers.contains(&signer) {
            return Err(GovernanceError::NotVetoSigner);
        }
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::VetoThreshold)
            .unwrap_or(0);

        // ── Load proposal ─────────────────────────────────────────
        let mut proposal: GovernanceProposal = Self::load_proposal(&env, proposal_id)?;

        // ── Guard: only Active or Passed proposals are vetoable ───
        match proposal.status {
            ProposalStatus::Active | ProposalStatus::Passed => {}
            _ => return Err(GovernanceError::NotVetoable),
        }

        // ── Record this signer's approval ─────────────────────────
        let approvals_key = StorageKey::VetoApprovals(proposal_id);
        let mut approvals: Vec<Address> = env
            .storage()
            .temporary()
            .get(&approvals_key)
            .unwrap_or(Vec::new(&env));
        if approvals.contains(&signer) {
            return Err(GovernanceError::VetoAlreadyApproved);
        }
        approvals.push_back(signer.clone());

        if approvals.len() < threshold {
            env.storage().temporary().set(&approvals_key, &approvals);
            env.storage().temporary().extend_ttl(
                &approvals_key,
                VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS,
                VOTE_RECEIPT_TTL_LEDGERS,
            );

            env.events().publish(
                (
                    Symbol::new(&env, "veto_approved"),
                    proposal_id,
                    signer.clone(),
                ),
                VetoApproved {
                    proposal_id,
                    signer,
                    approvals: approvals.len(),
                    threshold,
                },
            );
            return Ok(());
        }

        // ── Threshold reached: execute the veto ───────────────────
        env.storage().temporary().remove(&approvals_key);

        proposal.status = ProposalStatus::Vetoed;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        // Issue #814: vetoed spam is forfeited like a rejection (documented
        // in governance-security-summary.md).
        Self::forfeit_proposal_deposit(&env, proposal_id, &proposal.proposer)?;

        env.events().publish(
            (
                Symbol::new(&env, "proposal_vetoed"),
                proposal_id,
                signer.clone(),
            ),
            ProposalVetoed {
                proposal_id,
                admin: signer,
                reason_hash,
            },
        );

        Ok(())
    }

    // ── Issue #68: disable_veto_power ─────────────────────────────

    /// Permanently disable the admin veto power.
    ///
    /// Authorization: the configured ILN contract address must authorize
    /// (same pattern used by `set_min_quorum_bps` — governance votes trigger
    /// this via a cross-contract call from the ILN contract).
    ///
    /// Once disabled this cannot be re-enabled; it is a one-way switch
    /// intended to be called before mainnet launch.
    pub fn disable_veto_power(env: Env) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();

        env.storage()
            .instance()
            .set(&StorageKey::VetoPowerEnabled, &false);

        env.events().publish(
            (Symbol::new(&env, "veto_power_disabled"),),
            VetoPowerDisabled {
                disabled_by: iln_contract,
            },
        );

        Ok(())
    }

    /// Returns `true` when admin veto power is still active.
    pub fn is_veto_power_enabled(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&StorageKey::VetoPowerEnabled)
            .unwrap_or(false)
    }

    // ── Getters ──────────────────────────────────────────────────

    pub fn get_proposal(env: Env, proposal_id: u64) -> Result<GovernanceProposal, GovernanceError> {
        Self::load_proposal(&env, proposal_id)
    }

    pub fn list_proposals(
        env: Env,
        status: Option<ProposalStatus>,
        page: u32,
        page_size: u32,
    ) -> Vec<GovernanceProposal> {
        let count: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::ProposalCount)
            .unwrap_or(0);

        let mut result = Vec::new(&env);
        if count == 0 || page_size == 0 {
            return result;
        }

        let actual_page_size = if page_size > 20 { 20 } else { page_size };
        let skip = page.saturating_mul(actual_page_size) as u64;
        let mut skipped = 0_u64;

        for id in (1..=count).rev() {
            if let Some(proposal) = env
                .storage()
                .persistent()
                .get::<_, GovernanceProposal>(&StorageKey::Proposal(id))
            {
                let matches_status = match &status {
                    Some(s) => &proposal.status == s,
                    None => true,
                };

                if matches_status {
                    if skipped < skip {
                        skipped += 1;
                    } else {
                        result.push_back(proposal);
                        if result.len() == actual_page_size {
                            break;
                        }
                    }
                }
            }
        }

        result
    }

    pub fn has_voted(env: Env, voter: Address, proposal_id: u64) -> bool {
        env.storage()
            .temporary()
            .has(&StorageKey::HasVoted(proposal_id, voter))
    }

    // ── Private helpers ──────────────────────────────────────────

    /// Issue #531: invoke a cross-contract call and report whether it
    /// succeeded, instead of letting a trapped/failed call silently mark the
    /// proposal `Executed`. Uses `try_invoke_contract` so a callee panic or
    /// returned error surfaces here rather than aborting the whole
    /// `execute_proposal` transaction.
    fn invoke_and_check(
        env: &Env,
        contract: &Address,
        func_name: &str,
        args: Vec<soroban_sdk::Val>,
    ) -> bool {
        let result = env.try_invoke_contract::<(), soroban_sdk::Error>(
            contract,
            &Symbol::new(env, func_name),
            args,
        );
        matches!(result, Ok(Ok(())))
    }

    fn get_delegate_raw(env: &Env, addr: &Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&StorageKey::Delegation(addr.clone()))
    }

    /// Walk forward pointers to find the terminal node (one with no further delegate).
    fn resolve_terminal(env: &Env, start: &Address) -> Address {
        let max_depth = env.storage().instance().get(&StorageKey::MaxDelegationDepth).unwrap_or(DEFAULT_MAX_DELEGATION_DEPTH);
        let mut current = start.clone();
        let mut depth = 0u32;
        loop {
            if depth >= max_depth {
                break;
            }
            match Self::get_delegate_raw(env, &current) {
                Some(next) => {
                    current = next;
                    depth += 1;
                }
                None => break,
            }
        }
        current
    }

    /// Return the token balance of `addr` to use as the delegation weight.
    fn get_own_balance_for_delegation(env: &Env, addr: &Address) -> Result<i128, GovernanceError> {
        let token_addr = Self::get_gov_token(env)?;
        let token = TokenClient::new(env, &token_addr);
        Ok(token.balance(addr))
    }

    /// Add `delta` (may be negative) to the `DelegatedToMe` tally of `addr`.
    fn adjust_delegated_to_me(env: &Env, addr: &Address, delta: i128) {
        let key = StorageKey::DelegatedToMe(addr.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0_i128);
        let updated = current.saturating_add(delta);
        if updated <= 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &updated);
        }
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod tests_benchmarks;
