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
    contract, contracterror, contractimpl, contracttype, panic_with_error,
    token::Client as TokenClient, vec, Address, BytesN, Env, IntoVal, Map, Symbol, Vec,
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
/// Issue #805: minimum number of ledgers a voter's balance checkpoint must
/// predate a proposal's creation ledger before it can back a vote on that
/// proposal (~50 s at 5 s/ledger). An atomic flash loan lives and is repaid
/// inside a single transaction, so it can never age a checkpoint past this
/// bound — closing the same-transaction first-vote snapshot exploit without
/// requiring on-chain enumeration of all token holders (infeasible in
/// Soroban). Honest holders checkpoint once via `checkpoint_balance` (also
/// recorded automatically at `create_proposal`) and are then eligible on all
/// later proposals.
const MIN_VOTE_HOLD_LEDGERS: u32 = 10;

/// Default maximum transitive delegation chain depth.
const DEFAULT_MAX_DELEGATION_DEPTH: u32 = 10;

/// Issue #985: Minimum production execution delay (24 hours at ~5 s/ledger = 17,280 ledgers).
pub const MIN_PRODUCTION_EXECUTION_DELAY_LEDGERS: u32 = 17_280;
/// Issue #985: Maximum execution delay allowed to prevent governance deadlocks (30 days at ~5 s/ledger = 518,400 ledgers).
pub const MAX_EXECUTION_DELAY_LEDGERS: u32 = 518_400;

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
    /// Issue #805: the voter has no balance checkpoint predating this
    /// proposal by at least `MIN_VOTE_HOLD_LEDGERS`. Call
    /// `checkpoint_balance` and wait out the holding period (or vote on a
    /// later proposal) before voting.
    InsufficientHoldingPeriod = 29,
    /// Issue #985: Execution delay violates production minimum or maximum bounds.
    ExecutionDelayOutOfBounds = 35,
    /// Issue #989: Invalid transition in veto state machine.
    InvalidVetoTransition = 36,
    /// Issue #990: Admin veto power has already been permanently disabled.
    VetoPowerAlreadyDisabled = 37,
    /// Issue #990: Consent reason hash is required and cannot be empty.
    ConsentRequired = 38,
    /// Issue #992: the proposal (or stored delay) predates the timelock and
    /// carries no usable eta; call `rebase_legacy_proposal_eta` /
    /// `migrate_legacy_execution_delay` before it can be executed.
    LegacyDelayMigrationRequired = 39,
    /// Issue #998: the proposer recorded on the proposal does not match the
    /// proposer bound at creation time; the lifecycle transition is refused.
    ProposerIdentityMismatch = 40,
    /// Issue #1001: the proposal has no recorded snapshot ledger, so no
    /// checkpoint can be bound to it (fail-closed, no fallback to "now").
    SnapshotLedgerMissing = 41,
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

/// Issues #991/#993: single-call snapshot of the timelock / veto controls, so
/// operators and indexers can confirm the post-handoff posture (veto off,
/// timelock on) without stitching together several reads.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TimelockStatus {
    /// Ledgers a passed proposal must wait before it can execute.
    pub execution_delay: u32,
    /// `true` while the multisig veto can still block proposals.
    pub veto_enabled: bool,
    /// Configured veto signer count (0 if unconfigured).
    pub veto_signer_count: u32,
    /// Configured veto approval threshold (0 if unconfigured).
    pub veto_threshold: u32,
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

/// Issue #1007: why a proposal transitioned `Active -> Rejected`.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RejectionReason {
    /// Total votes cast were below the configured quorum.
    QuorumNotReached,
    /// Quorum was met but `votes_for <= votes_against`.
    MajorityAgainst,
}

/// Issue #1007: emitted when voting closes and a proposal is rejected.
///
/// Soroban discards every event and storage write of a call that returns
/// `Err`, so the rejection is reported by a *successful* `execute_proposal`
/// call carrying this event rather than by `QuorumNotReached` /
/// `ProposalRejected` (those variants are retained for ABI stability).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalRejectedEvent {
    pub proposal_id: u64,
    pub reason: RejectionReason,
    pub votes_for: i128,
    pub votes_against: i128,
    /// Quorum (in votes) that applied when the proposal was rejected.
    pub quorum: i128,
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

/// Issue #990: Permanent on-chain audit record of veto-power disablement.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VetoDisableRecord {
    pub authorized_by: Address,
    pub disabled_at_ledger: u32,
    pub reason_hash: BytesN<32>,
}

/// Issue #990: Emitted when veto power is disabled, recording caller authorization and reason hash.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VetoDisableRecorded {
    pub authorized_by: Address,
    pub disabled_at_ledger: u32,
    pub reason_hash: BytesN<32>,
}

/// Issue #989: Veto lifecycle states in the veto state machine.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VetoStatus {
    Uninitiated = 0,
    Pending = 1,
    Executed = 2,
}

/// Issue #989: State record for the veto state machine.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VetoState {
    pub status: VetoStatus,
    pub approvals_count: u32,
    pub threshold: u32,
    pub reason_hash: BytesN<32>,
}

/// Issue #989: Emitted when a veto state transition occurs.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VetoStateTransition {
    pub proposal_id: u64,
    pub previous_status: VetoStatus,
    pub new_status: VetoStatus,
    pub approvals_count: u32,
    pub threshold: u32,
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
    /// Issue #805: last proven balance checkpoint per voter (see
    /// `BalanceCheckpoint`). Written by `checkpoint_balance` and (when
    /// absent) for the proposer at `create_proposal`.
    BalanceCheckpoint(Address),
    /// Issue #805: ledger sequence at which a proposal was created. The
    /// reference point for the `MIN_VOTE_HOLD_LEDGERS` eligibility rule in
    /// `cast_vote`. Stored beside `Proposal` (rather than inside it) so the
    /// `GovernanceProposal` XDR schema — and every persisted proposal —
    /// stays byte-compatible.
    ProposalCreatedLedger(u64),
    /// Issue #1098: reverse delegation adjacency — the set of addresses whose
    /// forward `Delegation` pointer currently targets this address. Required to
    /// enforce invariant DEL2 (delegation chain depth ≤ `MaxDelegationDepth`)
    /// for *every* insertion order, including chains grown by appending at the
    /// leaf. A forward-only check cannot observe how deep a *delegator* already
    /// sits inside an inbound chain, so this adjacency closes that gap.
    Delegators(Address),
    /// Issue #1099: fail-closed persistent mirror of the `HasVoted` +
    /// `AppliedVoteWeight` temporary vote receipt. Temporary entries can be
    /// archived by the host, which would resurrect the "already voted" guard
    /// and allow a double vote. This persistent record stores the applied
    /// weight (0 is never written, so presence means "voted") and is the
    /// authoritative guard; the temporary entry is retained for fast reads and
    /// backwards compatibility.
    VoteReceipt(u64, Address),
    /// Issue #1099: per-signer persistent mirror of `VetoApprovals`. Because a
    /// single temporary `Vec` can be archived, the veto threshold tally is
    /// derived from these durable per-signer flags so an archived approval list
    /// cannot reset a partially-collected veto multisig.
    VetoApprovalReceipt(u64, Address),
    /// Issue #989: Veto state machine record for a proposal.
    VetoState(u64),
    /// Issue #990: Record of veto power disable authorization and reason.
    VetoDisableRecord,
    /// Issue #992: audit record of `migrate_legacy_execution_delay`.
    LegacyDelayMigration,
    /// Issue #998: proposer bound at `create_proposal`, immutable thereafter.
    /// Stored beside `Proposal` so the `GovernanceProposal` schema is unchanged.
    ProposalProposer(u64),
    /// Issue #1001: the checkpoint a voter's latest `checkpoint_balance`
    /// replaced. Together with `BalanceCheckpoint` this bounds history to two
    /// slots per voter while letting a refresh after a proposal's snapshot
    /// ledger leave the older, eligible checkpoint usable for that proposal.
    PriorBalanceCheckpoint(Address),
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

    // ── Initialise ────────────────────────────────────────────────

    /// `initialize` contract entry point.
///
/// Access: Anyone
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
        env.storage().instance().set(
            &StorageKey::MaxDelegationDepth,
            &DEFAULT_MAX_DELEGATION_DEPTH,
        );
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
///
/// Access: Anyone
pub fn get_min_quorum_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::MinQuorumBps)
            .unwrap_or(DEFAULT_MIN_QUORUM_BPS)
    }

    pub fn get_max_delegation_depth(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::MaxDelegationDepth)
            .unwrap_or(DEFAULT_MAX_DELEGATION_DEPTH)
    }

    /// `set_max_delegation_depth` contract entry point.
///
/// # Arguments
/// * `env` — host environment
/// * `max_depth` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn set_max_delegation_depth(env: Env, max_depth: u32) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(&env)?;
        iln_contract.require_auth();
        let old_value: u32 = Self::get_max_delegation_depth(env.clone());
        env.storage()
            .instance()
            .set(&StorageKey::MaxDelegationDepth, &max_depth);
        env.events().publish(
            (Symbol::new(&env, "max_delegation_depth_updated"),),
            (old_value, max_depth),
        );
        Ok(())
    }

/// Returns the configured governance token total supply used for quorum
///
/// Access: Anyone
pub fn get_gov_token_total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::GovTokenTotalSupply)
            .unwrap_or(0)
    }

/// Updates the governance token total supply used for quorum
///
/// # Arguments
/// * `env` — host environment
/// * `total_supply` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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
/// # Arguments
/// * `env` — host environment
/// * `min_quorum_bps` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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

    /// `create_proposal` contract entry point.
///
/// # Arguments
/// * `env` — host environment
/// * `proposer` — see signature
/// * `action_type` — see signature
/// * `description_hash` — see signature
/// * `proposed_value` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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

        // Issue #805: record the creation ledger as the reference point for
        // the checkpoint-eligibility rule, and seed the proposer's balance
        // checkpoint (preserved when one already exists so an older,
        // longer-aged checkpoint keeps backing the proposer's future votes).
        let created_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&StorageKey::ProposalCreatedLedger(id), &created_ledger);
        let checkpoint_key = StorageKey::BalanceCheckpoint(proposer.clone());
        if !env.storage().persistent().has(&checkpoint_key) {
            env.storage().persistent().set(
                &checkpoint_key,
                &BalanceCheckpoint {
                    balance: proposer_balance,
                    ledger: created_ledger,
                },
            );
        }

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(id), &proposal);
        // Issue #998: bind the proposer identity immutably at creation.
        env.storage()
            .persistent()
            .set(&StorageKey::ProposalProposer(id), &proposer);
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
///
/// Access: Anyone
pub fn get_min_proposal_deposit(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::MinProposalDeposit)
            .unwrap_or(DEFAULT_PROPOSAL_DEPOSIT)
    }

/// Updates the forfeitable proposal deposit amount.
///
/// # Arguments
/// * `env` — host environment
/// * `amount` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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
        env.storage()
            .instance()
            .get(&StorageKey::ProposalDepositSink)
    }

/// Sets (or, when `sink` is `None`, clears) the forfeiture destination.
///
/// # Arguments
/// * `env` — host environment
/// * `sink` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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
///
/// Access: Anyone
pub fn get_proposal_deposit(env: Env, proposal_id: u64) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::ProposalDeposit(proposal_id))
            .unwrap_or(0)
    }

/// Returns `true` once a proposal's deposit has been settled (refunded
///
/// Access: Anyone
pub fn is_proposal_deposit_settled(env: Env, proposal_id: u64) -> bool {
        env.storage()
            .persistent()
            .get(&StorageKey::ProposalDepositSettled(proposal_id))
            .unwrap_or(false)
    }

    // ── Issue #805: balance checkpoints (flash-loan-resistant snapshots) ──

/// Record (or refresh) the caller's proven governance-token balance.
///
/// # Arguments
/// * `env` — host environment
/// * `voter` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn checkpoint_balance(env: Env, voter: Address) -> Result<(), GovernanceError> {
        voter.require_auth();
        // #844: fallible helper — no panic when called before `initialize()`.
        let token_addr = Self::get_gov_token(&env)?;
        let token = TokenClient::new(&env, &token_addr);
        let balance = token.balance(&voter);
        // Issue #1001: a refresh must not destroy the proof backing proposals
        // whose snapshot ledger predates it. Demote the replaced checkpoint
        // into the single prior slot (state stays bounded at two entries per
        // voter). A same-ledger refresh just overwrites the latest slot.
        let latest_key = StorageKey::BalanceCheckpoint(voter.clone());
        if let Some(old) = env
            .storage()
            .persistent()
            .get::<StorageKey, BalanceCheckpoint>(&latest_key)
        {
            if old.ledger < env.ledger().sequence() {
                env.storage()
                    .persistent()
                    .set(&StorageKey::PriorBalanceCheckpoint(voter.clone()), &old);
            }
        }
        env.storage().persistent().set(
            &latest_key,
            &BalanceCheckpoint {
                balance,
                ledger: env.ledger().sequence(),
            },
        );
        Ok(())
    }

/// Returns the voter's last recorded balance checkpoint, if any.
///
/// Access: Anyone
pub fn get_voter_checkpoint(env: Env, voter: Address) -> Option<BalanceCheckpoint> {
        env.storage()
            .persistent()
            .get(&StorageKey::BalanceCheckpoint(voter))
    }

/// Issue #1001: the checkpoint replaced by the voter's latest refresh, if any.
///
/// Access: Anyone
pub fn get_voter_prior_checkpoint(env: Env, voter: Address) -> Option<BalanceCheckpoint> {
        env.storage()
            .persistent()
            .get(&StorageKey::PriorBalanceCheckpoint(voter))
    }

/// Issue #1001: the checkpoint that would back `voter`'s vote on
/// `proposal_id` (bound to the proposal's snapshot ledger), if any.
///
/// Access: Anyone
pub fn get_eligible_checkpoint(
        env: Env,
        voter: Address,
        proposal_id: u64,
    ) -> Option<BalanceCheckpoint> {
        let snapshot: u32 = env
            .storage()
            .persistent()
            .get(&StorageKey::ProposalCreatedLedger(proposal_id))?;
        Self::eligible_checkpoint(&env, &voter, snapshot)
    }

/// Issue #998: the proposer bound to `proposal_id` at creation.
///
/// Access: Anyone
pub fn get_proposal_proposer(env: Env, proposal_id: u64) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&StorageKey::ProposalProposer(proposal_id))
    }

/// Returns the ledger sequence at which `proposal_id` was created
///
/// Access: Anyone
pub fn get_proposal_created_ledger(env: Env, proposal_id: u64) -> Option<u32> {
        env.storage()
            .persistent()
            .get(&StorageKey::ProposalCreatedLedger(proposal_id))
    }

    /// Refund the escrowed deposit to the proposer. Idempotent: a second
    /// call after settlement is a no-op returning `false` (no second
    /// transfer), which is what prevents double-refunds on the
    /// `Passed -> Executed` second `execute_proposal` call.
    fn refund_proposal_deposit(
        env: &Env,
        proposal_id: u64,
        proposer: &Address,
    ) -> Result<bool, GovernanceError> {
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
        let token_addr = Self::get_gov_token(env)
            .unwrap_or_else(|e| soroban_sdk::panic_with_error!(env, e));
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
    fn forfeit_proposal_deposit(
        env: &Env,
        proposal_id: u64,
        proposer: &Address,
    ) -> Result<bool, GovernanceError> {
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
        let sink: Option<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::ProposalDepositSink);
        if let Some(dest) = sink.clone() {
            let token_addr = Self::get_gov_token(env)
            .unwrap_or_else(|e| soroban_sdk::panic_with_error!(env, e));
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
///
/// Access: Anyone
pub fn is_quadratic_voting_enabled(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&StorageKey::QuadraticVotingEnabled)
            .unwrap_or(false)
    }

/// Enables or disables quadratic voting.
///
/// # Arguments
/// * `env` — host environment
/// * `enabled` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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
///
/// Access: Anyone
pub fn get_applied_vote_weight(env: Env, proposal_id: u64, voter: Address) -> Option<i128> {
        // Issue #1099: prefer the fast temporary receipt, but fall back to the
        // durable mirror so an archived temporary entry does not erase the
        // applied weight.
        env.storage()
            .temporary()
            .get(&StorageKey::AppliedVoteWeight(proposal_id, voter.clone()))
            .or_else(|| {
                env.storage()
                    .persistent()
                    .get(&StorageKey::VoteReceipt(proposal_id, voter))
            })
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
            let mid = lo.saturating_add(hi.saturating_sub(lo).saturating_add(1).saturating_div(2));
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
/// # Arguments
/// * `env` — host environment
/// * `min_balance` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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
/// # Arguments
/// * `env` — host environment
/// * `delegator` — see signature
/// * `delegate` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn delegate_votes(
        env: Env,
        delegator: Address,
        delegate: Address,
    ) -> Result<(), GovernanceError> {
        delegator.require_auth();

        if delegator == delegate {
            return Err(GovernanceError::CannotDelegateToSelf);
        }

        // Issue #805: gate the contributed weight the same way `cast_vote`
        // gates first votes — otherwise a flash-funded address could
        // permanently inflate a terminal's `DelegatedToMe` tally and the
        // terminal's later vote. The current ledger is the reference point:
        // only balances checkpointed at least `MIN_VOTE_HOLD_LEDGERS` ago
        // count (capped at the live balance via `min`).
        let now_ledger = env.ledger().sequence();
        let live_balance = Self::get_own_balance_for_delegation(&env, &delegator)?;
        let proven_balance = Self::proven_own_balance(&env, &delegator, live_balance, now_ledger)?;

        // ── Cycle detection ───────────────────────────────────────
        // Walk the forward chain from `delegate`. If we reach `delegator`,
        // the new edge would close a cycle. Bounded by `max_depth` so a
        // pre-existing over-deep/cyclic graph cannot spin forever.
        let max_depth = Self::get_max_delegation_depth(env.clone());
        let mut cursor: Option<Address> = Self::get_delegate_raw(&env, &delegate);
        let mut hops = 0u32;
        while let Some(next) = cursor.clone() {
            if next == delegator {
                return Err(GovernanceError::DelegationCyclePrevented);
            }
            hops += 1;
            if hops > max_depth {
                return Err(GovernanceError::MaxDelegationDepthExceeded);
            }
            cursor = Self::get_delegate_raw(&env, &next);
        }

        // ── Issue #1098: exact DEL2 depth check ───────────────────
        // The resulting chain from `delegator` is `1 + forward_depth(delegate)`,
        // and every predecessor of `delegator` inherits the same extension, so
        // the longest chain that will exist after the write is
        // `longest_inbound_depth(delegator) + 1 + forward_depth(delegate)`.
        // Validation happens before any mutation so a rejected call leaves no
        // partial state behind.
        let downstream = Self::forward_delegation_depth(&env, &delegate);
        let upstream = Self::longest_inbound_depth(&env, &delegator);
        if upstream
            .saturating_add(1)
            .saturating_add(downstream)
            > max_depth
        {
            return Err(GovernanceError::MaxDelegationDepthExceeded);
        }

        // ── Find the terminal node for `delegate` ─────────────────
        let terminal = Self::resolve_terminal(&env, &delegate);

        // ── Remove weight from old terminal if re-delegating ──────
        if let Some(old_delegate) = Self::get_delegate_raw(&env, &delegator) {
            let old_terminal = Self::resolve_terminal(&env, &old_delegate);
            Self::adjust_delegated_to_me(&env, &old_terminal, -proven_balance);
            // Issue #1098: keep the reverse adjacency in sync.
            Self::remove_delegator(&env, &old_delegate, &delegator);
        }

        // ── Store forward pointer ─────────────────────────────────
        env.storage()
            .persistent()
            .set(&StorageKey::Delegation(delegator.clone()), &delegate);
        // Issue #1098: record the reverse edge so later depth checks can see
        // how deep this delegator sits inside an inbound chain.
        Self::add_delegator(&env, &delegate, &delegator);

        // ── Add weight to new terminal ────────────────────────────
        Self::adjust_delegated_to_me(&env, &terminal, proven_balance);

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
/// # Arguments
/// * `env` — host environment
/// * `delegator` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn undelegate_votes(env: Env, delegator: Address) -> Result<(), GovernanceError> {
        delegator.require_auth();

        if let Some(old_delegate) = Self::get_delegate_raw(&env, &delegator) {
            let old_terminal = Self::resolve_terminal(&env, &old_delegate);
            let delegator_balance = Self::get_own_balance_for_delegation(&env, &delegator)?;
            Self::adjust_delegated_to_me(&env, &old_terminal, -delegator_balance);

            // Issue #1098: drop the reverse adjacency edge too.
            Self::remove_delegator(&env, &old_delegate, &delegator);

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
///
/// Access: Anyone
pub fn get_delegate(env: Env, addr: Address) -> Option<Address> {
        Self::get_delegate_raw(&env, &addr)
    }

    // ── cast_vote ─────────────────────────────────────────────────

/// Cast a vote on an active proposal.
///
/// # Arguments
/// * `env` — host environment
/// * `voter` — see signature
/// * `proposal_id` — see signature
/// * `support` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn cast_vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
        support: bool,
    ) -> Result<(), GovernanceError> {
        voter.require_auth();

        let mut proposal: GovernanceProposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(GovernanceError::ProposalNotFound)?;

        let now = env.ledger().timestamp();
        if now >= proposal.voting_end {
            return Err(GovernanceError::VotingEnded);
        }
        if proposal.status != ProposalStatus::Active {
            return Err(GovernanceError::ProposalNotActive);
        }

        let voted_key = StorageKey::HasVoted(proposal_id, voter.clone());
        // Issue #1099: the temporary receipt may have been archived by the
        // host; consult the durable mirror too so an archived receipt cannot
        // be used to vote twice. Fail-closed: presence in either store blocks.
        if env.storage().temporary().has(&voted_key)
            || env
                .storage()
                .persistent()
                .has(&StorageKey::VoteReceipt(proposal_id, voter.clone()))
        {
            return Err(GovernanceError::AlreadyVoted);
        }

        let token_addr = Self::get_gov_token(&env)?;
        let token = TokenClient::new(&env, &token_addr);

        // Own snapshotted (or checkpoint-proven) balance.
        //
        // Issue #805: the old code snapshotted `token.balance(&voter)` here,
        // so a flash-borrow + first-vote + repay inside one transaction
        // permanently locked in the inflated amount. Now the first vote
        // requires an aged pre-proposal checkpoint and carries
        // `min(checkpoint, current)` — a same-transaction loan satisfies
        // neither. The proposer's creation-time snapshot (set in
        // `create_proposal`, where real funds must clear the balance gate
        // and escrow) is still honoured as-is.
        let snapshot_key = StorageKey::VoteWeightSnapshot(proposal_id, voter.clone());
        let own_balance: i128 = match env.storage().persistent().get(&snapshot_key) {
            Some(w) => w,
            None => {
                let current = token.balance(&voter);
                // Issue #1001: the checkpoint is bound to the proposal's
                // snapshot ledger. A proposal with no recorded snapshot
                // ledger fails closed instead of silently using "now".
                let created_ledger: u32 = env
                    .storage()
                    .persistent()
                    .get(&StorageKey::ProposalCreatedLedger(proposal_id))
                    .ok_or(GovernanceError::SnapshotLedgerMissing)?;
                let proven = Self::proven_own_balance(&env, &voter, current, created_ledger)?;
                env.storage().persistent().set(&snapshot_key, &proven);
                proven
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

        // Issue #1099: durable mirror of the receipt. Temporary storage can be
        // archived, which would clear the "already voted" guard above; this
        // persistent record is the fail-closed source of truth.
        let receipt_key = StorageKey::VoteReceipt(proposal_id, voter.clone());
        env.storage().persistent().set(&receipt_key, &weight);
        env.storage().persistent().extend_ttl(
            &receipt_key,
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

    /// `set_execution_delay` contract entry point.
///
/// # Arguments
/// * `env` — host environment
/// * `admin` — see signature
/// * `delay` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
pub fn set_execution_delay(
        env: Env,
        admin: Address,
        delay: u32,
    ) -> Result<(), GovernanceError> {
        admin.require_auth();

        // Issue #997: fail closed. The admin is only ever seeded by
        // `initialize`; an uninitialized contract must not let the first
        // caller install themselves as admin via this entry point.
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::Admin)
            .ok_or(GovernanceError::NotInitialized)?;
        if admin != stored_admin {
            return Err(GovernanceError::Unauthorized);
        }

        let old_value: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::ExecutionDelay)
            .unwrap_or(0_u32);

        // Issue #985: Validate maximum delay bound to prevent governance deadlocks
        if delay > MAX_EXECUTION_DELAY_LEDGERS {
            return Err(GovernanceError::ExecutionDelayOutOfBounds);
        }

        // Issue #985: Once veto power is disabled (full decentralization), enforce
        // the minimum production delay bound (at least 24 hours).
        if !Self::is_veto_power_enabled(env.clone()) && delay < MIN_PRODUCTION_EXECUTION_DELAY_LEDGERS {
            return Err(GovernanceError::ExecutionDelayOutOfBounds);
        }

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

    /// `get_execution_delay` contract entry point.
///
/// Access: Anyone
pub fn get_execution_delay(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::ExecutionDelay)
            .unwrap_or(0)
    }

    /// Issue #993: combined read of the timelock and veto controls.
    pub fn get_timelock_status(env: Env) -> TimelockStatus {
        TimelockStatus {
            execution_delay: Self::get_execution_delay(env.clone()),
            veto_enabled: Self::is_veto_power_enabled(env.clone()),
            veto_signer_count: Self::get_veto_signers(env.clone()).len(),
            veto_threshold: Self::get_veto_threshold(env),
        }
    }

    /// Issue #993: earliest ledger at which `proposal_id` may execute.
    /// `0` until the proposal has passed (the ETA is only stamped on the
    /// Active -> Passed transition).
    pub fn get_proposal_eta(env: Env, proposal_id: u64) -> Result<u32, GovernanceError> {
        Ok(Self::get_proposal(env, proposal_id)?.eta_ledger)
    }

    // ── execute_proposal ─────────────────────────────────────────

    /// `execute_proposal` contract entry point.
///
/// Access: Anyone
pub fn execute_proposal(env: Env, proposal_id: u64) -> Result<(), GovernanceError> {
        let mut proposal: GovernanceProposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(GovernanceError::ProposalNotFound)?;
        // Issue #998: refuse any transition on a proposal whose proposer
        // no longer matches the identity bound at creation.
        Self::verified_proposer(&env, &proposal)?;

        let now = env.ledger().timestamp();
        if now < proposal.voting_end {
            return Err(GovernanceError::VotingOngoing);
        }

        if proposal.status == ProposalStatus::Active {
            let total_votes = proposal.votes_for.saturating_add(proposal.votes_against);
            let min_quorum_bps: u32 = env
                .storage()
                .instance()
                .get(&StorageKey::MinQuorumBps)
                .unwrap_or(DEFAULT_MIN_QUORUM_BPS);

            // Issue #622: total_supply used to be a caller-supplied argument,
            // letting a caller inflate it (lowering the effective quorum) or
            // deflate it (blocking quorum entirely). Read the contract-stored
            // value instead (seeded at initialize, only updatable via the
            // ILN-contract-gated set_gov_token_total_supply) — it can no
            // longer be chosen by whoever happens to call execute_proposal.
            let total_supply: i128 = env
                .storage()
                .instance()
                .get(&StorageKey::GovTokenTotalSupply)
                .unwrap_or(0);

            let quorum = if total_supply <= 0 {
                0_i128
            } else {
                total_supply.saturating_mul(min_quorum_bps as i128) / 10_000_i128
            };

            if total_votes < quorum {
                Self::reject_proposal(
                    &env,
                    &mut proposal,
                    RejectionReason::QuorumNotReached,
                    quorum,
                )?;
                return Ok(());
            }

            if proposal.votes_for <= proposal.votes_against {
                Self::reject_proposal(
                    &env,
                    &mut proposal,
                    RejectionReason::MajorityAgainst,
                    quorum,
                )?;
                return Ok(());
            }

            proposal.status = ProposalStatus::Passed;

            // Issue #992: legacy zero-delay state is clamped to the
            // production floor once veto power is retired.
            let delay = Self::effective_execution_delay(&env);
            // Issue #987: Calculate eta using wrapping_add to cleanly support ledger rollover.
            // Issue #992: 0 is the "legacy, no eta" sentinel; a wrap landing
            // exactly on 0 moves one ledger later (never earlier).
            proposal.eta_ledger = Self::compute_eta(env.ledger().sequence(), delay);

            env.storage()
                .persistent()
                .set(&StorageKey::Proposal(proposal_id), &proposal);
            // Issue #814: passed — refund the deposit (idempotent, so the
            // later Passed -> Executed call does not double-pay).
            Self::refund_proposal_deposit(&env, proposal_id, &proposal.proposer)?;
            return Ok(());
        }

        if proposal.status == ProposalStatus::Passed {
            // Issue #992: a Passed proposal with no eta predates the timelock.
            // It may only skip the wait while the delay regime is still the
            // legacy zero-delay one; otherwise it must be rebased first.
            if proposal.eta_ledger == 0 && Self::effective_execution_delay(&env) > 0 {
                return Err(GovernanceError::LegacyDelayMigrationRequired);
            }
            let current_ledger = env.ledger().sequence();
            // Issue #987: Verify eta comparison across ledger rollover.
            if !Self::check_eta_reached(current_ledger, proposal.eta_ledger) {
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
                //
                // Issue #1007: return `Ok` so the event above is committed.
                // Returning `Err(ExecutionFailed)` would roll the event back
                // and leave the rejected execution invisible on-chain.
                return Ok(());
            }

            proposal.status = ProposalStatus::Executed;
            env.storage()
                .persistent()
                .set(&StorageKey::Proposal(proposal_id), &proposal);

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

        Err(GovernanceError::AlreadyResolved)
    }

    // ── Issue #642: configure_veto_multisig ───────────────────────

/// Configure (or reconfigure) the veto multisig signer set and the
///
/// # Arguments
/// * `env` — host environment
/// * `signers` — see signature
/// * `threshold` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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
///
/// Access: Anyone
pub fn get_veto_signers(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&StorageKey::VetoSigners)
            .unwrap_or(Vec::new(&env))
    }

/// Returns the configured veto multisig approval threshold (0 if unconfigured).
///
/// Access: Anyone
pub fn get_veto_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::VetoThreshold)
            .unwrap_or(0)
    }

/// Returns the veto signers who have already approved vetoing
///
/// Access: Anyone
pub fn get_veto_approvals(env: Env, proposal_id: u64) -> Vec<Address> {
        // Issue #1099: derive from the durable per-signer flags so an archived
        // temporary approval list does not read as "no approvals".
        Self::veto_approvals_from_persistent(&env, proposal_id)
    }

    /// Issue #1099: rebuild the veto approval list from the persistent
    /// per-signer receipts (ordered by the configured signer set).
    fn veto_approvals_from_persistent(env: &Env, proposal_id: u64) -> Vec<Address> {
        let signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::VetoSigners)
            .unwrap_or(Vec::new(env));
        let mut approvals = Vec::new(env);
        for signer in signers.iter() {
            if env
                .storage()
                .persistent()
                .has(&StorageKey::VetoApprovalReceipt(
                    proposal_id,
                    signer.clone(),
                ))
            {
                approvals.push_back(signer);
            }
        }
        approvals
    }

    // ── Issue #68 / #642: veto_proposal ───────────────────────────

/// Approve vetoing an active (or passed) proposal. Once `threshold`
///
/// # Arguments
/// * `env` — host environment
/// * `signer` — see signature
/// * `proposal_id` — see signature
/// * `reason_hash` — see signature
///
/// # Returns
/// * `Ok(...)` on success; see Errors
///
/// # Errors
/// * Authorization / validation errors as defined by this contract
///
/// Access: Caller (require_auth)
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
        let mut proposal: GovernanceProposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(GovernanceError::ProposalNotFound)?;

        // Issue #998: proposer identity must match the creation-time binding.
        Self::verified_proposer(&env, &proposal)?;

        // ── Guard: only Active or Passed proposals are vetoable ───
        match proposal.status {
            ProposalStatus::Active | ProposalStatus::Passed => {}
            _ => return Err(GovernanceError::NotVetoable),
        }

        // Issue #989: Veto transition state machine check
        let veto_state_key = StorageKey::VetoState(proposal_id);
        let current_veto_state = env
            .storage()
            .instance()
            .get::<_, VetoState>(&veto_state_key)
            .unwrap_or(VetoState {
                status: VetoStatus::Uninitiated,
                approvals_count: 0,
                threshold,
                reason_hash: reason_hash.clone(),
            });

        if current_veto_state.status == VetoStatus::Executed || proposal.status == ProposalStatus::Vetoed {
            return Err(GovernanceError::InvalidVetoTransition);
        }

        let prev_status = current_veto_state.status;

        // ── Record this signer's approval (Issue #1099: durable) ──
        let receipt_key = StorageKey::VetoApprovalReceipt(proposal_id, signer.clone());
        if env.storage().persistent().has(&receipt_key) {
            return Err(GovernanceError::VetoAlreadyApproved);
        }
        env.storage().persistent().set(&receipt_key, &true);
        env.storage().persistent().extend_ttl(
            &receipt_key,
            VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS,
            VOTE_RECEIPT_TTL_LEDGERS,
        );

        // Durable per-signer flags are authoritative, so rebuild the list from
        // them (an archived temporary list can no longer reset the tally). The
        // temporary copy is kept only as a fast-read mirror.
        let approvals_key = StorageKey::VetoApprovals(proposal_id);
        let approvals: Vec<Address> = Self::veto_approvals_from_persistent(&env, proposal_id);
        env.storage().temporary().set(&approvals_key, &approvals);
        env.storage().temporary().extend_ttl(
            &approvals_key,
            VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS,
            VOTE_RECEIPT_TTL_LEDGERS,
        );

        if approvals.len() < threshold {
            let next_state = VetoState {
                status: VetoStatus::Pending,
                approvals_count: approvals.len(),
                threshold,
                reason_hash: reason_hash.clone(),
            };
            env.storage().instance().set(&veto_state_key, &next_state);

            env.events().publish(
                (Symbol::new(&env, "veto_state_transition"), proposal_id),
                VetoStateTransition {
                    proposal_id,
                    previous_status: prev_status,
                    new_status: VetoStatus::Pending,
                    approvals_count: approvals.len(),
                    threshold,
                },
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
        for approved in approvals.iter() {
            env.storage()
                .persistent()
                .remove(&StorageKey::VetoApprovalReceipt(proposal_id, approved));
        }

        proposal.status = ProposalStatus::Vetoed;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        let next_state = VetoState {
            status: VetoStatus::Executed,
            approvals_count: approvals.len(),
            threshold,
            reason_hash: reason_hash.clone(),
        };
        env.storage().instance().set(&veto_state_key, &next_state);

        env.events().publish(
            (Symbol::new(&env, "veto_state_transition"), proposal_id),
            VetoStateTransition {
                proposal_id,
                previous_status: prev_status,
                new_status: VetoStatus::Executed,
                approvals_count: approvals.len(),
                threshold,
            },
        );

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

    /// Issue #989: Returns the current state of the veto transition state machine for a proposal.
    /// Access: Anyone
    pub fn get_veto_state(env: Env, proposal_id: u64) -> VetoState {
        if let Some(state) = env
            .storage()
            .instance()
            .get::<_, VetoState>(&StorageKey::VetoState(proposal_id))
        {
            return state;
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::VetoThreshold)
            .unwrap_or(0);

        let empty_hash = BytesN::from_array(&env, &[0u8; 32]);

        if let Some(proposal) = env
            .storage()
            .persistent()
            .get::<_, GovernanceProposal>(&StorageKey::Proposal(proposal_id))
        {
            if proposal.status == ProposalStatus::Vetoed {
                return VetoState {
                    status: VetoStatus::Executed,
                    approvals_count: threshold,
                    threshold,
                    reason_hash: empty_hash,
                };
            }
        }

        VetoState {
            status: VetoStatus::Uninitiated,
            approvals_count: 0,
            threshold,
            reason_hash: empty_hash,
        }
    }

    // ── Issue #68 / #990: disable_veto_power ──────────────────────

    /// Permanently disable the admin veto power.
    ///
    /// # Arguments
    /// * `env` — host environment
    ///
    /// # Returns
    /// * `Ok(...)` on success; see Errors
    ///
    /// # Errors
    /// * Authorization / validation errors as defined by this contract
    ///
    /// Access: Caller (require_auth)
    pub fn disable_veto_power(env: Env) -> Result<(), GovernanceError> {
        let empty_hash = BytesN::from_array(&env, &[0u8; 32]);
        Self::internal_disable_veto_power(&env, empty_hash)
    }

    /// Issue #990: Permanently disable the admin veto power with an explicit reason hash.
    ///
    /// Access: Caller (require_auth)
    pub fn disable_veto_power_with_reason(
        env: Env,
        reason_hash: BytesN<32>,
    ) -> Result<(), GovernanceError> {
        let empty_hash = BytesN::from_array(&env, &[0u8; 32]);
        if reason_hash == empty_hash {
            return Err(GovernanceError::ConsentRequired);
        }
        Self::internal_disable_veto_power(&env, reason_hash)
    }

    fn internal_disable_veto_power(
        env: &Env,
        reason_hash: BytesN<32>,
    ) -> Result<(), GovernanceError> {
        let iln_contract = Self::get_iln_contract(env)?;
        iln_contract.require_auth();

        if !Self::is_veto_power_enabled(env.clone()) {
            return Err(GovernanceError::VetoPowerAlreadyDisabled);
        }

        env.storage()
            .instance()
            .set(&StorageKey::VetoPowerEnabled, &false);

        let record = VetoDisableRecord {
            authorized_by: iln_contract.clone(),
            disabled_at_ledger: env.ledger().sequence(),
            reason_hash: reason_hash.clone(),
        };
        env.storage()
            .instance()
            .set(&StorageKey::VetoDisableRecord, &record);

        env.events().publish(
            (Symbol::new(env, "veto_power_disabled"),),
            VetoPowerDisabled {
                disabled_by: iln_contract.clone(),
            },
        );

        env.events().publish(
            (Symbol::new(env, "veto_disable_recorded"), iln_contract),
            VetoDisableRecorded {
                authorized_by: record.authorized_by,
                disabled_at_ledger: record.disabled_at_ledger,
                reason_hash,
            },
        );

        Ok(())
    }

    /// Issue #990: Returns the recorded veto-disable authorization details if disabled.
    /// Access: Anyone
    pub fn get_veto_disable_record(env: Env) -> Option<VetoDisableRecord> {
        env.storage()
            .instance()
            .get(&StorageKey::VetoDisableRecord)
    }

/// Returns `true` when admin veto power is still active.
///
/// Access: Anyone
pub fn is_veto_power_enabled(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&StorageKey::VetoPowerEnabled)
            .unwrap_or(false)
    }

    // ── Getters ──────────────────────────────────────────────────

    /// `get_proposal` contract entry point.
///
/// Access: Anyone
pub fn get_proposal(env: Env, proposal_id: u64) -> Result<GovernanceProposal, GovernanceError> {
        env.storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(GovernanceError::ProposalNotFound)
    }

    /// `list_proposals` contract entry point.
    ///
    /// Access: Anyone
    pub fn list_proposals(
        env: Env,
        status: Option<ProposalStatus>,
        cursor: Option<u64>,
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
        let start_id = match cursor {
            Some(c) => {
                if c <= 1 {
                    return result;
                }
                (c - 1).min(count)
            }
            None => count,
        };

        for id in (1..=start_id).rev() {
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
                    result.push_back(proposal);
                    if result.len() == actual_page_size {
                        break;
                    }
                }
            }
        }

        result
    }

    /// `has_voted` contract entry point.
///
/// Access: Anyone
pub fn has_voted(env: Env, voter: Address, proposal_id: u64) -> bool {
        env.storage().temporary().has(&StorageKey::HasVoted(proposal_id, voter.clone()))
            || env
                .storage()
                .persistent()
                .has(&StorageKey::VoteReceipt(proposal_id, voter))
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

    /// Issue #1007: persist the `Active -> Rejected` transition, settle the
    /// deposit and emit `proposal_rejected`. Runs inside a call that returns
    /// `Ok`, so the state change and the event are both committed.
    fn reject_proposal(
        env: &Env,
        proposal: &mut GovernanceProposal,
        reason: RejectionReason,
        quorum: i128,
    ) -> Result<(), GovernanceError> {
        proposal.status = ProposalStatus::Rejected;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal.id), &*proposal);
        // Issue #814: rejected / expired without quorum — forfeit the deposit.
        Self::forfeit_proposal_deposit(env, proposal.id, &proposal.proposer)?;
        env.events().publish(
            (Symbol::new(env, "proposal_rejected"), proposal.id),
            ProposalRejectedEvent {
                proposal_id: proposal.id,
                reason,
                votes_for: proposal.votes_for,
                votes_against: proposal.votes_against,
                quorum,
            },
        );
        Ok(())
    }

    fn get_delegate_raw(env: &Env, addr: &Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&StorageKey::Delegation(addr.clone()))
    }

    /// Issue #1098: direct predecessors of `addr` in the delegation graph.
    fn get_delegators(env: &Env, addr: &Address) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&StorageKey::Delegators(addr.clone()))
            .unwrap_or_else(|| Vec::new(env))
    }

    /// Issue #1098: record that `delegator` now points at `delegate`.
    fn add_delegator(env: &Env, delegate: &Address, delegator: &Address) {
        let key = StorageKey::Delegators(delegate.clone());
        let mut list = Self::get_delegators(env, delegate);
        if !list.contains(delegator.clone()) {
            list.push_back(delegator.clone());
            env.storage().persistent().set(&key, &list);
        }
    }

    /// Issue #1098: drop the `delegator -> delegate` reverse edge.
    fn remove_delegator(env: &Env, delegate: &Address, delegator: &Address) {
        let key = StorageKey::Delegators(delegate.clone());
        let list = Self::get_delegators(env, delegate);
        let mut filtered = Vec::new(env);
        for item in list.iter() {
            if item != delegator.clone() {
                filtered.push_back(item);
            }
        }
        if filtered.is_empty() {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &filtered);
        }
    }

    /// Issue #1098: number of edges on the forward delegation chain starting at
    /// `start` (0 for a terminal). The DEL2 invariant guarantees this never
    /// exceeds `MaxDelegationDepth`, so the walk is bounded.
    fn forward_delegation_depth(env: &Env, start: &Address) -> u32 {
        let max_depth = Self::get_max_delegation_depth(env.clone());
        let mut current = start.clone();
        let mut depth = 0u32;
        while depth < max_depth {
            match Self::get_delegate_raw(env, &current) {
                Some(next) => {
                    current = next;
                    depth += 1;
                }
                None => break,
            }
        }
        depth
    }

    /// Issue #1098: length (in edges) of the longest delegation path that
    /// terminates at `start`, computed over the reverse adjacency. Used to
    /// compute the resulting chain length for every predecessor of a delegator
    /// when a new edge is added, so DEL2 is enforced regardless of insertion
    /// order. Iterative (explicit stack) to keep host stack depth bounded;
    /// memoized so shared sub-paths are visited once.
    fn longest_inbound_depth(env: &Env, start: &Address) -> u32 {
        let mut best: Map<Address, u32> = Map::new(env);
        let mut stack: Vec<(Address, bool)> = Vec::new(env);
        stack.push_back((start.clone(), false));
        while let Some((node, processed)) = stack.pop_back() {
            if processed {
                let preds = Self::get_delegators(env, &node);
                let mut node_best = 0u32;
                for p in preds.iter() {
                    let candidate = best.get(p.clone()).unwrap_or(0).saturating_add(1);
                    if candidate > node_best {
                        node_best = candidate;
                    }
                }
                best.set(node.clone(), node_best);
            } else {
                if best.contains_key(node.clone()) {
                    continue;
                }
                stack.push_back((node.clone(), true));
                let preds = Self::get_delegators(env, &node);
                for p in preds.iter() {
                    if !best.contains_key(p.clone()) {
                        stack.push_back((p, false));
                    }
                }
            }
        }
        best.get(start.clone()).unwrap_or(0)
    }

    /// Walk forward pointers to find the terminal node (one with no further delegate).
    fn resolve_terminal(env: &Env, start: &Address) -> Address {
        let max_depth = env
            .storage()
            .instance()
            .get(&StorageKey::MaxDelegationDepth)
            .unwrap_or(DEFAULT_MAX_DELEGATION_DEPTH);
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

    /// Issue #805: the checkpoint-proven own balance usable at reference
    /// ledger `ref_ledger` (a proposal's creation ledger for votes, the
    /// current ledger for delegations).
    ///
    /// Requires a `BalanceCheckpoint` with
    /// `checkpoint.ledger + MIN_VOTE_HOLD_LEDGERS <= ref_ledger` and returns
    /// `min(checkpoint.balance, current)`: the `min` pins the weight to
    /// funds that demonstrably survived from the checkpoint to now, so a
    /// checkpoint recorded with flash funds (repaid before the vote) and a
    /// live balance inflated with flash funds (no aged checkpoint) are both
    /// worthless. Anything else fails with `InsufficientHoldingPeriod`.
    fn proven_own_balance(
        env: &Env,
        voter: &Address,
        current: i128,
        ref_ledger: u32,
    ) -> Result<i128, GovernanceError> {
        match Self::eligible_checkpoint(env, voter, ref_ledger) {
            Some(c) => Ok(c.balance.min(current)),
            None => Err(GovernanceError::InsufficientHoldingPeriod),
        }
    }

    /// Issue #1001: newest of the latest/prior checkpoint slots recorded at
    /// least `MIN_VOTE_HOLD_LEDGERS` before `ref_ledger`.
    fn eligible_checkpoint(env: &Env, voter: &Address, ref_ledger: u32) -> Option<BalanceCheckpoint> {
        let latest: Option<BalanceCheckpoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::BalanceCheckpoint(voter.clone()));
        let prior: Option<BalanceCheckpoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::PriorBalanceCheckpoint(voter.clone()));
        [latest, prior]
            .into_iter()
            .flatten()
            .filter(|c| c.ledger.saturating_add(MIN_VOTE_HOLD_LEDGERS) <= ref_ledger)
            .max_by_key(|c| c.ledger)
    }

    /// Issue #998: returns the proposer bound at creation, refusing the
    /// transition if the stored proposal disagrees. Proposals created before
    /// the binding existed are backfilled once from the persisted proposal.
    fn verified_proposer(
        env: &Env,
        proposal: &GovernanceProposal,
    ) -> Result<Address, GovernanceError> {
        let key = StorageKey::ProposalProposer(proposal.id);
        match env.storage().persistent().get::<StorageKey, Address>(&key) {
            Some(bound) if bound == proposal.proposer => Ok(bound),
            Some(_) => Err(GovernanceError::ProposerIdentityMismatch),
            None => {
                env.storage().persistent().set(&key, &proposal.proposer);
                Ok(proposal.proposer.clone())
            }
        }
    }

    /// Issues #987/#992: `sequence + delay` with rollover wrap, never 0 (the
    /// legacy "no eta" sentinel); a wrap onto 0 moves one ledger later.
    pub fn compute_eta(sequence: u32, delay: u32) -> u32 {
        match sequence.wrapping_add(delay) {
            0 => 1,
            eta => eta,
        }
    }

    /// Issue #992: delay actually applied to new `Passed` proposals. Once
    /// veto power is retired the production floor is enforced even if the
    /// stored (legacy) value is lower.
    fn effective_execution_delay(env: &Env) -> u32 {
        let stored: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::ExecutionDelay)
            .unwrap_or(0_u32);
        let veto_enabled: bool = env
            .storage()
            .instance()
            .get(&StorageKey::VetoPowerEnabled)
            .unwrap_or(false);
        if !veto_enabled && stored < MIN_PRODUCTION_EXECUTION_DELAY_LEDGERS {
            MIN_PRODUCTION_EXECUTION_DELAY_LEDGERS
        } else {
            stored
        }
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
#[cfg(test)]
mod tests_differential;
#[cfg(test)]
mod tests_proof_obligations;
