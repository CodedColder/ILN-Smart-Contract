/// Multi-signature Admin Module (Issue #124)
///
/// Implements a threshold-based multi-signature scheme for high-security admin operations.
/// Requires a configurable threshold of authorized signers to approve critical actions
/// such as pause, contract upgrade, or token removal.
///
/// Workflow:
/// 1. Any signer calls propose_admin_action() to create a proposal
/// 2. Signers call sign_admin_action() to approve the proposal
/// 3. Once threshold is reached, any signer calls execute_admin_action()
/// 4. Proposals expire after MULTISIG_WINDOW_LEDGERS if not executed

use soroban_sdk::{contracttype, Address, Env, Vec};
use crate::access::require_admin;
use crate::errors::ContractError;
use crate::storage::DataKey;

/// Number of ledgers a multisig proposal remains valid (approximately 24 hours)
pub const MULTISIG_WINDOW_LEDGERS: u64 = 17_280;

/// Enumeration of admin actions that require multi-sig approval
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminAction {
    /// Pause contract (emergency stop)
    Pause,
    /// Unpause contract (resume operations)
    Unpause,
    /// Remove a token from approved tokens list
    RemoveToken(Address),
    /// Change the fee rate
    SetFeeRate(u32),
    /// Set maximum discount rate
    SetMaxDiscount(u32),
    /// Update multisig configuration itself (change signers or threshold)
    ///
    /// Tuple variant `(new_signers, new_threshold)` — `#[contracttype]` does
    /// not support named-field enum variants.
    UpdateMultisig(Vec<Address>, u32),
    /// Issue #640: replace `old_signer` with `new_signer` in the signer
    /// set. Executing this proposal does not swap the signer immediately —
    /// it schedules the swap behind a timelock (see `schedule_rotation`),
    /// so a compromised or departing signer's key can be rotated without a
    /// contract upgrade while still giving the team a window to detect and
    /// cancel a malicious or mistaken rotation.
    ///
    /// Tuple variant `(old_signer, new_signer)`.
    RotateSigner(Address, Address),
}

/// Multi-signature admin configuration
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct MultisigAdmin {
    /// List of authorized signers
    pub signers: Vec<Address>,
    /// Number of signatures required to execute an action
    pub threshold: u32,
}

/// A proposal for an admin action awaiting signatures
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalState {
    Pending,
    Executed,
    Expired,
}

/// Issue #641: validate a signer list / threshold combination — non-empty,
/// no duplicate signers, and `1 <= threshold <= signers.len()`.
pub fn is_valid_config(signers: &Vec<Address>, threshold: u32) -> bool {
    if signers.is_empty() || threshold == 0 || threshold > signers.len() {
        return false;
    }
    for i in 0..signers.len() {
        for j in (i + 1)..signers.len() {
            if signers.get(i).unwrap() == signers.get(j).unwrap() {
                return false;
            }
        }
    }
    true
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct MultisigProposal {
    /// Unique proposal ID
    pub id: u64,
    /// The proposed action
    pub action: AdminAction,
    /// List of signers who have approved this proposal
    pub signers_approved: Vec<Address>,
    /// Current state of the proposal
    pub state: ProposalState,
    /// Ledger sequence number when this proposal expires
    pub expires_at: u64,
}

/// Validate that an address is in the signer list
pub fn is_signer(env: &Env, signers: &Vec<Address>, address: &Address) -> bool {
    for i in 0..signers.len() {
        if signers.get(i).unwrap() == *address {
            return true;
        }
    }
    false
}

/// Check if a signer has already approved a proposal
pub fn has_signed(proposal: &MultisigProposal, signer: &Address) -> bool {
    for i in 0..proposal.signers_approved.len() {
        if proposal.signers_approved.get(i).unwrap() == *signer {
            return true;
        }
    }
    false
}

/// Check if proposal has reached the approval threshold
pub fn threshold_reached(proposal: &MultisigProposal, threshold: u32) -> bool {
    proposal.signers_approved.len() as u32 >= threshold
}

/// Check if proposal has expired
pub fn is_expired(env: &Env, proposal: &MultisigProposal) -> bool {
    u64::from(env.ledger().sequence()) >= proposal.expires_at
}

// ================================================================
// Entry-point logic (Issue #641 wiring — see lib.rs for the thin
// #[contractimpl] wrappers that expose these as contract functions)
// ================================================================

/// Bootstrap the multisig admin signer set and approval threshold.
///
/// Authorization: the existing single `Admin` address must authorize —
/// the one-time bootstrap step that hands control to the multisig.
/// Fails with `MultisigAlreadyConfigured` if called again; reconfiguring
/// afterwards goes through the `UpdateMultisig` proposal flow instead.
pub fn initialize(env: &Env, signers: Vec<Address>, threshold: u32) -> Result<(), ContractError> {
    if env.storage().instance().has(&DataKey::MultisigAdmin) {
        return Err(ContractError::MultisigAlreadyConfigured);
    }
    if !is_valid_config(&signers, threshold) {
        return Err(ContractError::InvalidMultisigConfig);
    }
    require_admin(env)?;

    let config = MultisigAdmin { signers, threshold };
    env.storage().instance().set(&DataKey::MultisigAdmin, &config);
    env.storage().instance().set(&DataKey::NextProposalId, &1u64);
    Ok(())
}

fn load_config(env: &Env) -> Result<MultisigAdmin, ContractError> {
    env.storage()
        .instance()
        .get(&DataKey::MultisigAdmin)
        .ok_or(ContractError::MultisigNotConfigured)
}

fn require_signer(env: &Env, config: &MultisigAdmin, signer: &Address) -> Result<(), ContractError> {
    signer.require_auth();
    if !is_signer(env, &config.signers, signer) {
        return Err(ContractError::NotAuthorizedSigner);
    }
    Ok(())
}

/// Create a new multisig proposal for `action`.
///
/// Issue #641 (duplicate-submission protection): if a `Pending`,
/// non-expired proposal already exists for this exact `action`, returns
/// `DuplicateProposal` instead of creating a second one — closing the
/// race where two signers concurrently submit their own proposal for the
/// same logical admin action (e.g. two "pause" proposals in flight).
pub fn propose(env: &Env, proposer: &Address, action: AdminAction) -> Result<u64, ContractError> {
    let config = load_config(env)?;
    require_signer(env, &config, proposer)?;

    let pending_key = DataKey::PendingActionProposal(action.clone());
    if let Some(existing_id) = env.storage().instance().get::<_, u64>(&pending_key) {
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<_, MultisigProposal>(&DataKey::MultisigProposal(existing_id))
        {
            if existing.state == ProposalState::Pending && !is_expired(env, &existing) {
                return Err(ContractError::DuplicateProposal);
            }
        }
    }

    let id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::NextProposalId)
        .unwrap_or(1);
    env.storage().instance().set(&DataKey::NextProposalId, &(id + 1));

    let mut signers_approved = Vec::new(env);
    signers_approved.push_back(proposer.clone());

    let expires_at = env.ledger().sequence() as u64 + MULTISIG_WINDOW_LEDGERS;
    let proposal = MultisigProposal {
        id,
        action,
        signers_approved,
        state: ProposalState::Pending,
        expires_at,
    };
    let proposal_key = DataKey::MultisigProposal(id);
    env.storage().persistent().set(&proposal_key, &proposal);
    env.storage()
        .persistent()
        .extend_ttl(&proposal_key, 100_000, 200_000);
    // Issue #641: index this proposal as the in-flight one for its action,
    // so a concurrent duplicate proposal is rejected until this resolves.
    env.storage().instance().set(&pending_key, &id);

    Ok(id)
}

/// Approve a pending proposal. `signer` must be a configured multisig
/// signer who has not already approved this specific proposal — the
/// per-signer half of Issue #641's replay/duplication audit: the same
/// signer's approval can never be counted twice on the same proposal, and
/// since approvals are stored on the proposal they authorized, a signer's
/// approval can never be replayed onto a different proposal either.
pub fn sign(env: &Env, signer: &Address, proposal_id: u64) -> Result<(), ContractError> {
    let config = load_config(env)?;
    require_signer(env, &config, signer)?;

    let proposal_key = DataKey::MultisigProposal(proposal_id);
    let mut proposal: MultisigProposal = env
        .storage()
        .persistent()
        .get(&proposal_key)
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.state == ProposalState::Executed {
        return Err(ContractError::ProposalAlreadyExecuted);
    }
    if is_expired(env, &proposal) {
        proposal.state = ProposalState::Expired;
        env.storage().persistent().set(&proposal_key, &proposal);
        return Err(ContractError::ProposalExpired);
    }
    if has_signed(&proposal, signer) {
        return Err(ContractError::AlreadySigned);
    }

    proposal.signers_approved.push_back(signer.clone());
    env.storage().persistent().set(&proposal_key, &proposal);
    Ok(())
}

/// Execute a proposal once its approval threshold has been met, marking it
/// `Executed` and returning the `AdminAction` it authorized. The caller
/// (lib.rs) applies the actual state change — this module stays free of
/// pause/token-list/fee-rate specifics.
///
/// `caller` must be a configured multisig signer, but does not need to be
/// one of the proposal's approvers (any signer may trigger execution once
/// threshold is met).
pub fn execute(env: &Env, caller: &Address, proposal_id: u64) -> Result<AdminAction, ContractError> {
    let config = load_config(env)?;
    require_signer(env, &config, caller)?;

    let proposal_key = DataKey::MultisigProposal(proposal_id);
    let mut proposal: MultisigProposal = env
        .storage()
        .persistent()
        .get(&proposal_key)
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.state == ProposalState::Executed {
        return Err(ContractError::ProposalAlreadyExecuted);
    }
    if is_expired(env, &proposal) {
        proposal.state = ProposalState::Expired;
        env.storage().persistent().set(&proposal_key, &proposal);
        return Err(ContractError::ProposalExpired);
    }
    if !threshold_reached(&proposal, config.threshold) {
        return Err(ContractError::ThresholdNotReached);
    }

    proposal.state = ProposalState::Executed;
    let action = proposal.action.clone();
    env.storage().persistent().set(&proposal_key, &proposal);
    // Issue #641: clear the duplicate-proposal index now that this action
    // has resolved, so a fresh proposal for the same action can be made.
    env.storage()
        .instance()
        .remove(&DataKey::PendingActionProposal(action.clone()));

    Ok(action)
}

// ================================================================
// Issue #640: time-locked signer rotation
// ================================================================

/// Ledgers a scheduled signer rotation must wait before it can be
/// finalized — gives the team a window to detect and cancel a malicious
/// or mistaken rotation before it actually changes the signer set.
/// ~48 hours at 5s/ledger (double `MULTISIG_WINDOW_LEDGERS`, since a
/// signer-set change is higher-stakes than a routine admin action).
pub const ROTATION_TIMELOCK_LEDGERS: u64 = 34_560;

/// A signer rotation approved by the multisig but not yet applied.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PendingRotation {
    pub old_signer: Address,
    pub new_signer: Address,
    /// Ledger sequence at/after which `finalize_rotation` may be called.
    pub effective_at: u64,
}

/// Schedule a signer rotation. Called from `execute_proposal` once a
/// `RotateSigner` proposal reaches threshold — the multisig approval
/// starts the timelock clock, it does not itself change the signer set.
/// Only one rotation may be pending at a time.
pub fn schedule_rotation(
    env: &Env,
    old_signer: Address,
    new_signer: Address,
) -> Result<PendingRotation, ContractError> {
    if env.storage().instance().has(&DataKey::PendingSignerRotation) {
        return Err(ContractError::RotationAlreadyPending);
    }
    let config = load_config(env)?;
    if !is_signer(env, &config.signers, &old_signer) {
        return Err(ContractError::NotAuthorizedSigner);
    }
    if old_signer == new_signer || is_signer(env, &config.signers, &new_signer) {
        return Err(ContractError::InvalidMultisigConfig);
    }

    let rotation = PendingRotation {
        old_signer,
        new_signer,
        effective_at: env.ledger().sequence() as u64 + ROTATION_TIMELOCK_LEDGERS,
    };
    env.storage()
        .instance()
        .set(&DataKey::PendingSignerRotation, &rotation);
    Ok(rotation)
}

/// Finalize a scheduled rotation once its timelock has elapsed, swapping
/// `old_signer` for `new_signer` in the signer set — without a contract
/// upgrade. `caller` must be a current signer.
pub fn finalize_rotation(env: &Env, caller: &Address) -> Result<PendingRotation, ContractError> {
    let mut config = load_config(env)?;
    require_signer(env, &config, caller)?;

    let rotation: PendingRotation = env
        .storage()
        .instance()
        .get(&DataKey::PendingSignerRotation)
        .ok_or(ContractError::RotationNotFound)?;

    if (env.ledger().sequence() as u64) < rotation.effective_at {
        return Err(ContractError::RotationTimelockNotExpired);
    }

    let mut new_signers: Vec<Address> = Vec::new(env);
    for s in config.signers.iter() {
        if s != rotation.old_signer {
            new_signers.push_back(s);
        }
    }
    new_signers.push_back(rotation.new_signer.clone());
    config.signers = new_signers;

    env.storage().instance().set(&DataKey::MultisigAdmin, &config);
    env.storage()
        .instance()
        .remove(&DataKey::PendingSignerRotation);

    Ok(rotation)
}

/// Cancel a pending rotation before (or after) its timelock elapses — the
/// reaction mechanism the timelock exists to enable if a scheduled
/// rotation looks malicious or mistaken. `caller` must be a current
/// signer.
pub fn cancel_rotation(env: &Env, caller: &Address) -> Result<PendingRotation, ContractError> {
    let config = load_config(env)?;
    require_signer(env, &config, caller)?;

    let rotation: PendingRotation = env
        .storage()
        .instance()
        .get(&DataKey::PendingSignerRotation)
        .ok_or(ContractError::RotationNotFound)?;

    env.storage()
        .instance()
        .remove(&DataKey::PendingSignerRotation);
    Ok(rotation)
}
