// @ts-nocheck
import {
  Address,
  Contract,
  SorobanRpc,
  TransactionBuilder,
  BASE_FEE,
  nativeToScVal,
  scValToNative,
  xdr,
  Account,
  Transaction,
} from "@stellar/stellar-sdk";
import { ILNError } from "../errors.js";
import {
  ProposalAction,
  ProposalStatus,
  type Proposal,
  type ProposalFilter,
  type CreateProposalResult,
} from "../types/governance.js";
import { retry } from "../utils/retry.js";
import { decodeGovernanceProposal } from "../utils/xdrDecoder.js";

/**
 * iln_governance has its own `GovernanceError` enum (contracts/iln_governance/src/lib.rs)
 * whose numeric codes are NOT the same as invoice_liquidity's `ContractError`
 * codes in errors.ts (e.g. code 11 is CannotDelegateToSelf here, but
 * NotYetDefaulted there). The existing createProposal/castVote/
 * executeProposal/getProposal functions in this file already reuse
 * `ILNError.fromError`, which predates this — left as-is to avoid an
 * unrelated behavior change, but the delegation/veto/config methods added
 * below need to surface the *correct* governance-specific error (the issues
 * that added them explicitly call out distinguishing e.g. VetoPowerDisabled
 * from NotVetoable), so they use this dedicated mapper instead.
 */
export class GovernanceContractError extends Error {
  constructor(message: string, public readonly code?: number) {
    super(message);
    this.name = "GovernanceContractError";
  }

  static AlreadyInitialized = class AlreadyInitialized extends GovernanceContractError {
    constructor(msg = "Governance contract already initialized") { super(msg, 1); }
  };
  static ProposalNotFound = class ProposalNotFound extends GovernanceContractError {
    constructor(msg = "Proposal not found") { super(msg, 2); }
  };
  static VotingEnded = class VotingEnded extends GovernanceContractError {
    constructor(msg = "Voting period has ended") { super(msg, 3); }
  };
  static ProposalNotActive = class ProposalNotActive extends GovernanceContractError {
    constructor(msg = "Proposal is not active") { super(msg, 4); }
  };
  static NoVotingPower = class NoVotingPower extends GovernanceContractError {
    constructor(msg = "Caller has no voting power") { super(msg, 5); }
  };
  static AlreadyVoted = class AlreadyVoted extends GovernanceContractError {
    constructor(msg = "Already voted on this proposal") { super(msg, 6); }
  };
  static VotingOngoing = class VotingOngoing extends GovernanceContractError {
    constructor(msg = "Voting is still ongoing") { super(msg, 7); }
  };
  static QuorumNotReached = class QuorumNotReached extends GovernanceContractError {
    constructor(msg = "Quorum not reached") { super(msg, 8); }
  };
  static ProposalRejected = class ProposalRejected extends GovernanceContractError {
    constructor(msg = "Proposal was rejected") { super(msg, 9); }
  };
  static AlreadyResolved = class AlreadyResolved extends GovernanceContractError {
    constructor(msg = "Proposal already resolved") { super(msg, 10); }
  };
  static CannotDelegateToSelf = class CannotDelegateToSelf extends GovernanceContractError {
    constructor(msg = "Cannot delegate votes to yourself") { super(msg, 11); }
  };
  static DelegationCyclePrevented = class DelegationCyclePrevented extends GovernanceContractError {
    constructor(msg = "Delegation would create a cycle") { super(msg, 12); }
  };
  static TimelockNotExpired = class TimelockNotExpired extends GovernanceContractError {
    constructor(msg = "Timelock has not expired yet") { super(msg, 13); }
  };
  static Unauthorized = class Unauthorized extends GovernanceContractError {
    constructor(msg = "Unauthorized") { super(msg, 14); }
  };
  static InvalidQuorumBps = class InvalidQuorumBps extends GovernanceContractError {
    constructor(msg = "Quorum bps must be between 1 and 10,000") { super(msg, 15); }
  };
  static NotAdmin = class NotAdmin extends GovernanceContractError {
    constructor(msg = "Caller is not the admin") { super(msg, 16); }
  };
  static NotVetoable = class NotVetoable extends GovernanceContractError {
    constructor(msg = "Proposal cannot be vetoed in its current status") { super(msg, 17); }
  };
  static VetoPowerDisabled = class VetoPowerDisabled extends GovernanceContractError {
    constructor(msg = "Admin veto power has been permanently disabled") { super(msg, 18); }
  };
  static InsufficientProposerBalance = class InsufficientProposerBalance extends GovernanceContractError {
    constructor(msg = "Proposer does not hold the minimum required balance") { super(msg, 19); }
  };
  static ExecutionFailed = class ExecutionFailed extends GovernanceContractError {
    constructor(msg = "Proposal's cross-contract execution call failed; it remains Passed and can be retried") { super(msg, 20); }
  };
  static MaxDelegationDepthExceeded = class MaxDelegationDepthExceeded extends GovernanceContractError {
    constructor(msg = "Delegation chain exceeds the maximum depth cap") { super(msg, 21); }
  };
  static VetoMultisigNotConfigured = class VetoMultisigNotConfigured extends GovernanceContractError {
    constructor(msg = "Veto multisig has not been configured yet") { super(msg, 22); }
  };
  static NotVetoSigner = class NotVetoSigner extends GovernanceContractError {
    constructor(msg = "Caller is not a configured veto signer") { super(msg, 23); }
  };
  static VetoAlreadyApproved = class VetoAlreadyApproved extends GovernanceContractError {
    constructor(msg = "This signer already approved the veto for this proposal") { super(msg, 24); }
  };
  static InvalidVetoMultisigConfig = class InvalidVetoMultisigConfig extends GovernanceContractError {
    constructor(msg = "Veto signer list/threshold combination is invalid") { super(msg, 25); }
  };
  static ExecutionDelayOutOfBounds = class ExecutionDelayOutOfBounds extends GovernanceContractError {
    constructor(msg = "Execution delay violates production minimum or maximum bounds") { super(msg, 35); }
  };
  static InvalidVetoTransition = class InvalidVetoTransition extends GovernanceContractError {
    constructor(msg = "Invalid transition in veto transition state machine") { super(msg, 36); }
  };
  static VetoPowerAlreadyDisabled = class VetoPowerAlreadyDisabled extends GovernanceContractError {
    constructor(msg = "Admin veto power has already been permanently disabled") { super(msg, 37); }
  };
  static ConsentRequired = class ConsentRequired extends GovernanceContractError {
    constructor(msg = "Consent reason hash is required and cannot be empty") { super(msg, 38); }
  };

  static fromError(error: unknown): Error {
    const match = String(error).match(/Error\(Contract, (\d+)\)/);
    if (!match) return error instanceof Error ? error : new Error(String(error));
    switch (parseInt(match[1] || "", 10)) {
      case 1: return new GovernanceContractError.AlreadyInitialized();
      case 2: return new GovernanceContractError.ProposalNotFound();
      case 3: return new GovernanceContractError.VotingEnded();
      case 4: return new GovernanceContractError.ProposalNotActive();
      case 5: return new GovernanceContractError.NoVotingPower();
      case 6: return new GovernanceContractError.AlreadyVoted();
      case 7: return new GovernanceContractError.VotingOngoing();
      case 8: return new GovernanceContractError.QuorumNotReached();
      case 9: return new GovernanceContractError.ProposalRejected();
      case 10: return new GovernanceContractError.AlreadyResolved();
      case 11: return new GovernanceContractError.CannotDelegateToSelf();
      case 12: return new GovernanceContractError.DelegationCyclePrevented();
      case 13: return new GovernanceContractError.TimelockNotExpired();
      case 14: return new GovernanceContractError.Unauthorized();
      case 15: return new GovernanceContractError.InvalidQuorumBps();
      case 16: return new GovernanceContractError.NotAdmin();
      case 17: return new GovernanceContractError.NotVetoable();
      case 18: return new GovernanceContractError.VetoPowerDisabled();
      case 19: return new GovernanceContractError.InsufficientProposerBalance();
      case 20: return new GovernanceContractError.ExecutionFailed();
      case 21: return new GovernanceContractError.MaxDelegationDepthExceeded();
      case 22: return new GovernanceContractError.VetoMultisigNotConfigured();
      case 23: return new GovernanceContractError.NotVetoSigner();
      case 24: return new GovernanceContractError.VetoAlreadyApproved();
      case 25: return new GovernanceContractError.InvalidVetoMultisigConfig();
      case 35: return new GovernanceContractError.ExecutionDelayOutOfBounds();
      case 36: return new GovernanceContractError.InvalidVetoTransition();
      case 37: return new GovernanceContractError.VetoPowerAlreadyDisabled();
      case 38: return new GovernanceContractError.ConsentRequired();
      default: return new GovernanceContractError(`iln_governance error: ${String(error)}`);
    }
  }
}

/**
 * Build, simulate, sign and submit a governance transaction, polling until the
 * network confirms it. Shared by the write methods below.
 */
async function sendGovernanceCall(
  server: SorobanRpc.Server,
  sourceAccount: Account,
  networkPassphrase: string,
  op: ReturnType<Contract["call"]>,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  // Defaults to ILNError.fromError to preserve existing behavior for
  // createProposal/castVote/executeProposal. New callers below pass
  // GovernanceContractError.fromError for correctly-typed governance errors.
  mapError: (error: unknown) => Error = ILNError.fromError
): Promise<{ txHash: string; returnValue: unknown }> {
  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw mapError(sim.error);
  }

  const assembledTx = SorobanRpc.assembleTransaction(tx, sim).build();
  const signedTx = await signTransaction(assembledTx);
  const sendResult = await retry(() => server.sendTransaction(signedTx));
  if (sendResult.errorResult) {
    throw new Error(`Transaction failed: ${sendResult.errorResult}`);
  }

  let status = await retry(() => server.getTransaction(sendResult.hash));
  let retries = 0;
  while (status.status === SorobanRpc.Api.GetTransactionStatus.NOT_FOUND && retries < 15) {
    await new Promise(r => setTimeout(r, 2000));
    status = await retry(() => server.getTransaction(sendResult.hash));
    retries++;
  }
  if (status.status === SorobanRpc.Api.GetTransactionStatus.FAILED) {
    throw new Error("Transaction failed during execution");
  }

  const returnValue =
    status.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS && status.returnValue
      ? scValToNative(status.returnValue)
      : undefined;

  return { txHash: sendResult.hash, returnValue };
}

/** Normalise a raw contract proposal record into a {@link Proposal}. */
function parseProposal(raw: Record<string, unknown>): Proposal {
  const statusTag =
    ((raw as any)["status"] as unknown)?.tag ?? String((raw as any)["status"]);
  return {
    id: BigInt(String(raw["id"])),
    action: Number(raw["action"]) as ProposalAction,
    proposedValue: BigInt(String(raw["proposed_value"] ?? 0)),
    descriptionHash: (raw as any)["description_hash"]
      ? Buffer.from((raw as any)["description_hash"] as string).toString("hex")
      : "",
    proposer: String((raw as any)["proposer"]),
    votesFor: BigInt(String(raw["votes_for"] ?? 0)),
    votesAgainst: BigInt(String(raw["votes_against"] ?? 0)),
    status: (ProposalStatus as unknown)[statusTag] ?? (statusTag as ProposalStatus),
    votingEndsAt: Number(raw["voting_ends_at"] ?? 0),
  };
}

/**
 * Create a new governance proposal.
 *
 * @param server Soroban RPC server
 * @param contractAddress Governance contract address
 * @param action The parameter-changing action to propose
 * @param proposedValue The proposed new value for the action's parameter
 * @param descriptionHash Hex-encoded 32-byte hash of the off-chain description
 * @param sourceAccount The proposer's account
 * @param signTransaction A function to sign the transaction
 * @param networkPassphrase The network passphrase
 * @returns The new proposalId and txHash
 * @throws {ILNError} When simulation or execution fails
 */
export async function createProposal(
  server: SorobanRpc.Server,
  contractAddress: string,
  action: ProposalAction,
  proposedValue: bigint,
  descriptionHash: string,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<CreateProposalResult> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "create_proposal",
    nativeToScVal(sourceAccount.accountId(), { type: "address" }),
    nativeToScVal(action, { type: "u32" }),
    nativeToScVal(proposedValue, { type: "i128" }),
    nativeToScVal(Buffer.from(descriptionHash, "hex"), { type: "bytes" })
  );

  const { txHash, returnValue } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction
  );

  return {
    proposalId: returnValue !== undefined ? BigInt(String(returnValue)) : 0n,
    txHash,
  };
}

/**
 * Cast a vote on an active proposal.
 *
 * @param support `true` to vote for, `false` to vote against.
 */
export async function castVote(
  server: SorobanRpc.Server,
  contractAddress: string,
  proposalId: bigint,
  support: boolean,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "cast_vote",
    nativeToScVal(sourceAccount.accountId(), { type: "address" }),
    nativeToScVal(proposalId, { type: "u64" }),
    nativeToScVal(support, { type: "bool" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction
  );
  return { txHash };
}

/**
 * Execute a proposal that has passed its vote.
 *
 * Issue #622: the contract no longer accepts a caller-supplied `total_supply`
 * — it queries the real governance token's on-chain supply itself, so the
 * only argument here is the proposal id. (The previous version of this call
 * also mismatched the contract's actual `execute_proposal(proposal_id,
 * total_supply)` signature by sending an address in place of `proposal_id`
 * and omitting `total_supply` entirely — that mismatch is fixed as part of
 * this change too.)
 */
export async function executeProposal(
  server: SorobanRpc.Server,
  contractAddress: string,
  proposalId: bigint,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "execute_proposal",
    nativeToScVal(proposalId, { type: "u64" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction
  );
  return { txHash };
}

/**
 * Fetch a single proposal by ID (read-only; no signer required).
 */
export async function getProposal(
  server: SorobanRpc.Server,
  contractAddress: string,
  id: bigint,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<Proposal> {
  const contract = new Contract(contractAddress);
  const op = contract.call("get_proposal", nativeToScVal(id, { type: "u64" }));

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw ILNError.fromError(sim.error);
  }
  if (!sim.result?.retval) {
    throw new ILNError(`Proposal ${id} not found`);
  }

  const raw = scValToNative(sim.result.retval) as Record<string, unknown>;
  return decodeGovernanceProposal(raw);
}

/**
 * Check if an address has voted on a proposal (read-only; no signer required).
 * @param voter The address to check
 * @param proposalId The proposal ID
 * @returns true if the address has voted, false otherwise
 */
export async function hasVoted(
  server: SorobanRpc.Server,
  contractAddress: string,
  voter: string,
  proposalId: bigint,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<boolean> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "has_voted",
    nativeToScVal(voter, { type: "address" }),
    nativeToScVal(proposalId, { type: "u64" })
  );

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  if (!sim.result?.retval) {
    return false;
  }

  return scValToNative(sim.result.retval) as boolean;
}

/**
 * List proposals, optionally filtered by status and/or proposer (read-only).
 * @param filter Optional filters (status sent on-chain, proposer applied client-side)
 * @param cursor Optional proposal ID cursor (reads proposals before this ID; defaults to latest)
 * @param pageSize Results per page (defaults to 20, max 20)
 */
export async function listProposals(
  server: SorobanRpc.Server,
  contractAddress: string,
  sourceAccount: Account,
  networkPassphrase: string,
  filter?: ProposalFilter,
  cursor?: bigint | number,
  pageSize: number = 20
): Promise<Proposal[]> {
  const contract = new Contract(contractAddress);
  const statusScVal =
    filter?.status !== undefined
      ? nativeToScVal({ tag: filter.status, values: [] }, { type: "instance" })
      : nativeToScVal(undefined);
  const cursorScVal =
    cursor !== undefined
      ? nativeToScVal(BigInt(cursor), { type: "u64" })
      : nativeToScVal(undefined);
  const op = contract.call(
    "list_proposals",
    statusScVal,
    cursorScVal,
    nativeToScVal(pageSize, { type: "u32" })
  );

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw ILNError.fromError(sim.error);
  }
  if (!sim.result?.retval) {
    return [];
  }

  const rawArr = scValToNative(sim.result.retval) as Record<string, unknown>[];
  let proposals = rawArr.map(raw => decodeGovernanceProposal(raw as Record<string, unknown>));

  if (filter?.status !== undefined) {
    proposals = proposals.filter(p => p.status === filter.status);
  }
  if (filter?.proposer) {
    proposals = proposals.filter(p => p.proposer === filter.proposer);
  }
  return proposals;
}

// ---------------------------------------------------------------------------
// Delegation (#471)
// ---------------------------------------------------------------------------

/**
 * Delegate the caller's voting weight to `delegate`.
 *
 * Wraps `delegate_votes(delegator, delegate)`. Requires the delegator's
 * signature — `sourceAccount` must be the delegator's account. The contract
 * walks the forward delegation chain to detect cycles before storing the
 * new edge.
 *
 * @throws {GovernanceContractError.CannotDelegateToSelf} If delegator === delegate
 * @throws {GovernanceContractError.DelegationCyclePrevented} If delegating to `delegate` would close a cycle
 */
export async function delegateVotes(
  server: SorobanRpc.Server,
  contractAddress: string,
  delegatorAddress: string,
  delegateAddress: string,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "delegate_votes",
    nativeToScVal(delegatorAddress, { type: "address" }),
    nativeToScVal(delegateAddress, { type: "address" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

/**
 * Remove the caller's active delegation, if any.
 *
 * Wraps `undelegate_votes(delegator)`. Requires the delegator's signature —
 * `sourceAccount` must be the delegator's account. No-ops (does not error)
 * if the caller had no active delegation.
 */
export async function undelegateVotes(
  server: SorobanRpc.Server,
  contractAddress: string,
  delegatorAddress: string,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "undelegate_votes",
    nativeToScVal(delegatorAddress, { type: "address" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

// ---------------------------------------------------------------------------
// Multisig-gated veto (#472, migrated to multisig by #642)
// ---------------------------------------------------------------------------

/**
 * Configure (or reconfigure) the veto multisig signer set and approval
 * threshold.
 *
 * Wraps `configure_veto_multisig(signers, threshold)`. On the first call
 * (no veto multisig configured yet) the stored admin account must sign; on
 * subsequent calls the configured ILN contract must authorize instead (the
 * same governance-vote-gated pattern as {@link setMinQuorumBps}).
 *
 * @throws {GovernanceContractError.InvalidVetoMultisigConfig} If `signers` is empty, contains a duplicate, or `threshold` is outside `1..=signers.length`
 */
export async function configureVetoMultisig(
  server: SorobanRpc.Server,
  contractAddress: string,
  signers: string[],
  threshold: number,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "configure_veto_multisig",
    xdr.ScVal.scvVec(signers.map((s) => new Address(s).toScVal())),
    nativeToScVal(threshold, { type: "u32" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

/**
 * Approve vetoing a proposal, blocking it from proceeding once enough
 * signers agree.
 *
 * Wraps `veto_proposal(signer, proposal_id, reason_hash)`. Requires the
 * signature of `signerAddress`, which must be one of the configured
 * `VetoSigners` (see {@link configureVetoMultisig}) — a single admin key can
 * no longer veto unilaterally (Issue #642). Once `threshold` distinct
 * signers have called this for the same `proposalId`, the proposal
 * transitions to `Vetoed`; until then the call simply records an approval.
 * Only proposals in `Active` or `Passed` status can be vetoed.
 *
 * @param reasonHash Hex-encoded 32-byte hash of the off-chain veto rationale
 * @throws {GovernanceContractError.VetoMultisigNotConfigured} If configureVetoMultisig() has not been called yet
 * @throws {GovernanceContractError.NotVetoSigner} If `signerAddress` is not a configured veto signer
 * @throws {GovernanceContractError.VetoAlreadyApproved} If `signerAddress` already approved this proposal's veto
 * @throws {GovernanceContractError.VetoPowerDisabled} If disableVetoPower() was already called
 * @throws {GovernanceContractError.NotVetoable} If the proposal isn't Active or Passed
 * @throws {GovernanceContractError.ProposalNotFound} If the proposal id is unknown
 */
export async function vetoProposal(
  server: SorobanRpc.Server,
  contractAddress: string,
  signerAddress: string,
  proposalId: bigint,
  reasonHash: string,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "veto_proposal",
    new Address(signerAddress).toScVal(),
    nativeToScVal(proposalId, { type: "u64" }),
    nativeToScVal(Buffer.from(reasonHash, "hex"), { type: "bytes" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

/**
 * Permanently disable the admin veto power. **One-way switch** — cannot be
 * re-enabled once called.
 *
 * Wraps `disable_veto_power()`. Requires a signature from the configured ILN
 * contract address (same pattern as `set_min_quorum_bps`/
 * `set_min_proposal_balance` below) — `sourceAccount` must be authorized as
 * that address, not an arbitrary caller.
 */
export async function disableVetoPower(
  server: SorobanRpc.Server,
  contractAddress: string,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call("disable_veto_power");

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

// ---------------------------------------------------------------------------
// Execution delay / timelock (#473)
// ---------------------------------------------------------------------------

/**
 * Set the execution delay (timelock) applied before a passed proposal can
 * be executed.
 *
 * Wraps `set_execution_delay(admin, delay)`. Requires the admin's signature
 * — `sourceAccount` must be `adminAddress`'s account. On the *first* call
 * (no admin stored yet) the calling address becomes the stored admin for
 * future admin-gated calls (including `vetoProposal`); subsequent calls
 * require the same admin address.
 *
 * @param delay Timelock duration in seconds
 * @throws {GovernanceContractError.Unauthorized} If called by an address other than the already-stored admin
 */
export async function setExecutionDelay(
  server: SorobanRpc.Server,
  contractAddress: string,
  adminAddress: string,
  delay: number,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "set_execution_delay",
    nativeToScVal(adminAddress, { type: "address" }),
    nativeToScVal(delay, { type: "u32" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

/**
 * Fetch the currently configured execution delay (timelock), in seconds.
 * Read-only; no signer required. Defaults to 0 if never set.
 */
export async function getExecutionDelay(
  server: SorobanRpc.Server,
  contractAddress: string,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<number> {
  const contract = new Contract(contractAddress);
  const op = contract.call("get_execution_delay");

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  if (!sim.result?.retval) {
    return 0;
  }
  return Number(scValToNative(sim.result.retval));
}

// ---------------------------------------------------------------------------
// Quorum / proposal balance config (#474)
// ---------------------------------------------------------------------------

/**
 * Update the minimum quorum required for a proposal to pass.
 *
 * Wraps `set_min_quorum_bps(min_quorum_bps)`. Requires a signature from the
 * configured ILN contract address — `sourceAccount` must be authorized as
 * that address, not an arbitrary caller.
 *
 * @param quorumBps Basis points, must be in 1..=10_000 (e.g. 1000 = 10%)
 * @throws {GovernanceContractError.InvalidQuorumBps} If quorumBps is 0 or > 10,000
 */
export async function setMinQuorumBps(
  server: SorobanRpc.Server,
  contractAddress: string,
  quorumBps: number,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "set_min_quorum_bps",
    nativeToScVal(quorumBps, { type: "u32" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

/**
 * Update the minimum token balance required to create a proposal.
 *
 * Wraps `set_min_proposal_balance(min_balance)`. Requires a signature from
 * the configured ILN contract address — `sourceAccount` must be authorized
 * as that address, not an arbitrary caller.
 */
export async function setMinProposalBalance(
  server: SorobanRpc.Server,
  contractAddress: string,
  minBalance: bigint,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "set_min_proposal_balance",
    nativeToScVal(minBalance, { type: "i128" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

// ---------------------------------------------------------------------------
// Quadratic voting (#530)
// ---------------------------------------------------------------------------

/**
 * Enables or disables quadratic voting (`sqrt(balance + delegated)` weight
 * instead of linear). Defaults to disabled for backwards compatibility.
 *
 * Wraps `set_quadratic_voting_enabled(enabled)`. Requires a signature from
 * the configured ILN contract address — `sourceAccount` must be authorized
 * as that address, not an arbitrary caller.
 */
export async function setQuadraticVotingEnabled(
  server: SorobanRpc.Server,
  contractAddress: string,
  enabled: boolean,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "set_quadratic_voting_enabled",
    nativeToScVal(enabled, { type: "bool" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

/**
 * Returns whether quadratic voting is currently enabled (read-only; no
 * signer required).
 */
export async function isQuadraticVotingEnabled(
  server: SorobanRpc.Server,
  contractAddress: string,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<boolean> {
  const contract = new Contract(contractAddress);
  const op = contract.call("is_quadratic_voting_enabled");

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  if (!sim.result?.retval) {
    return false;
  }
  return scValToNative(sim.result.retval) as boolean;
}

/**
 * Fetch the weight actually applied to `voter`'s vote on `proposalId` (the
 * vote receipt) — the post-square-root value when quadratic voting was
 * enabled at the time of voting, otherwise the linear balance. Read-only;
 * no signer required. Returns `undefined` if the address hasn't voted (or
 * the receipt's temporary-storage TTL expired).
 */
export async function getAppliedVoteWeight(
  server: SorobanRpc.Server,
  contractAddress: string,
  proposalId: bigint,
  voter: string,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<bigint | undefined> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "get_applied_vote_weight",
    nativeToScVal(proposalId, { type: "u64" }),
    nativeToScVal(voter, { type: "address" })
  );

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  if (!sim.result?.retval) {
    return undefined;
  }
  const decoded = scValToNative(sim.result.retval);
  return decoded === null || decoded === undefined ? undefined : BigInt(String(decoded));
}

/**
 * Fetch the minimum and maximum execution delay bounds (Issue #985).
 */
export async function getProductionDelayBounds(
  server: SorobanRpc.Server,
  contractAddress: string,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<[number, number]> {
  const contract = new Contract(contractAddress);
  const op = contract.call("get_production_delay_bounds");

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  const decoded = scValToNative(sim.result.retval) as [number, number];
  return decoded;
}

/**
 * Validate whether a delay meets production requirements (Issue #985).
 */
export async function isValidProductionDelay(
  server: SorobanRpc.Server,
  contractAddress: string,
  delay: number,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<boolean> {
  const contract = new Contract(contractAddress);
  const op = contract.call("is_valid_production_delay", nativeToScVal(delay, { type: "u32" }));

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  return Boolean(scValToNative(sim.result.retval));
}

/**
 * Verify whether currentLedger has reached etaLedger across rollover (Issue #987).
 */
export async function isEtaReached(
  server: SorobanRpc.Server,
  contractAddress: string,
  currentLedger: number,
  etaLedger: number,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<boolean> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "is_eta_reached",
    nativeToScVal(currentLedger, { type: "u32" }),
    nativeToScVal(etaLedger, { type: "u32" })
  );

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  return Boolean(scValToNative(sim.result.retval));
}

/**
 * Get the current state in the veto transition state machine (Issue #989).
 */
export async function getVetoState(
  server: SorobanRpc.Server,
  contractAddress: string,
  proposalId: bigint,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<{ status: number; approvals_count: number; threshold: number; reason_hash: Buffer }> {
  const contract = new Contract(contractAddress);
  const op = contract.call("get_veto_state", nativeToScVal(proposalId, { type: "u64" }));

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  return scValToNative(sim.result.retval);
}

/**
 * Permanently disable admin veto power with an explicit reason hash (Issue #990).
 */
export async function disableVetoPowerWithReason(
  server: SorobanRpc.Server,
  contractAddress: string,
  reasonHash: Buffer,
  sourceAccount: Account,
  signTransaction: (tx: Transaction) => Promise<Transaction> | Transaction,
  networkPassphrase: string
): Promise<{ txHash: string }> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "disable_veto_power_with_reason",
    nativeToScVal(reasonHash, { type: "bytes" })
  );

  const { txHash } = await sendGovernanceCall(
    server,
    sourceAccount,
    networkPassphrase,
    op,
    signTransaction,
    GovernanceContractError.fromError
  );
  return { txHash };
}

/**
 * Fetch the recorded veto-disable authorization details, if disabled (Issue #990).
 */
export async function getVetoDisableRecord(
  server: SorobanRpc.Server,
  contractAddress: string,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<{ authorized_by: string; disabled_at_ledger: number; reason_hash: Buffer } | undefined> {
  const contract = new Contract(contractAddress);
  const op = contract.call("get_veto_disable_record");

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw GovernanceContractError.fromError(sim.error);
  }
  if (!sim.result?.retval) {
    return undefined;
  }
  const decoded = scValToNative(sim.result.retval);
  return decoded ?? undefined;
}

