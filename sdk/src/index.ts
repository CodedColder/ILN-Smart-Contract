/**
 * @iln/sdk — Invoice Liquidity Network TypeScript SDK
 *
 * Public surface area re-exported from this entry point.
 */

export { fundInvoice, computeEffectiveYieldBps } from "./methods/fundInvoice.js";
export { getReputation } from "./methods/reputation.js";
export { getContractStats } from "./methods/stats.js";
export { getTopPayers } from "./methods/topPayers.js";
export {
  getAllowance,
  buildApproveTransaction,
  isAllowanceSufficient,
} from "./utils/allowance.js";
export {
  validateGAddress,
  validateContractId,
  validateAmount,
  validateDiscountRate,
  validateDueDate,
} from "./utils/validate.js";
export { KeypairSigner } from "./signers/KeypairSigner.js";
export { FreighterSigner, ILNErrorCode } from "./signers/FreighterSigner.js";
export { subscribe, parseContractEvent, matchesFilter, replay } from "./events/subscribe.js";
export { ILNClient, iln } from "./client.js";
export type { ISigner } from "./signers/ISigner.js";
export type { ILNClientConfig } from "./client.js";
export type { ReputationProfile } from "./methods/reputation.js";
export type { ContractStats } from "./methods/stats.js";
export type { TopPayerEntry } from "./methods/topPayers.js";
export type {
  FundOptions,
  FundResult,
  InvoiceView,
  AllowanceParams,
  AllowanceResult,
} from "./types.js";
export type { InvoiceNftMetadata } from "./utils/xdrDecoder.js";
export type {
  ILNEvent,
  ILNEventType,
  EventFilter,
  Unsubscribe,
} from "./events/types.js";

export { getInvoice, listInvoicesBySubmitter, listInvoicesByLP, getSubmitterInvoices, getPayerScore } from "./methods/queries.js";
export { getLpInvoices } from "./methods/lpInvoices.js";
export { getNftMetadata, getNftOwner } from "./methods/nft.js";
export { submitInvoice } from "./methods/submitInvoice.js";
export { transferLPPosition } from "./methods/transferLPPosition.js";
export { cancelInvoice } from "./methods/cancelInvoice.js";
export { markPaid } from "./methods/markPaid.js";
export {
  createProposal,
  castVote,
  executeProposal,
  getProposal,
  listProposals,
  hasVoted,
  delegateVotes,
  undelegateVotes,
  vetoProposal,
  disableVetoPower,
  setExecutionDelay,
  getExecutionDelay,
  getProductionDelayBounds,
  isValidProductionDelay,
  isEtaReached,
  getVetoState,
  disableVetoPowerWithReason,
  getVetoDisableRecord,
  setMinQuorumBps,
  setMinProposalBalance,
  GovernanceContractError,
} from "./methods/governance.js";
export {
  ProposalAction,
  ProposalStatus,
} from "./types/governance.js";
export type {
  Proposal,
  ProposalFilter,
  CreateProposalResult,
} from "./types/governance.js";

export { ILNError, ValidationError, AuthorizationError, InvoiceStateError, ContractExecutionError, NetworkError } from "./errors.js";
export { disputeInvoice, sha256Hex } from "./methods/disputeInvoice.js";
export type { DisputeInvoiceParams, DisputeInvoiceResult } from "./methods/disputeInvoice.js";
export { resolveDispute, DisputeRuling } from "./methods/resolveDispute.js";
export type { ResolveDisputeResult } from "./methods/resolveDispute.js";
export {
  getPoolBalance,
  getCoverage,
  isEnrolled,
  getPremiumsPaid,
  getInsurancePoolInfo,
  enrollInsurancePool,
  depositInsurancePremium,
  claimInsurance,
  isInsuranceEnrolled,
  getInsurancePremiums,
  initializeInsurancePool,
  getTokenAddress,
  calculatePremiumRate,
  getTieredCoverage,
  getPoolHealth,
  InsuranceContractError,
} from "./methods/insurance.js";
export type { InsurancePoolInfo } from "@invoice-liquidity/types";
export {
  getDistributionAccrual,
  accrueLp,
  accrueSettlement,
  claimTokens,
} from "./methods/distribution.js";
export {
  submitReputationInvoice,
  markReputationInvoicePaid,
  handleDefault,
  getReputationBonusConfig,
  getReputationBonusReputation,
  ReputationContractError,
} from "./methods/reputation.js";
export type {
  ReputationBonusInvoice,
  ReputationBonusConfig,
  ReputationBonusScore,
} from "./methods/reputation.js";
export { getReferralStats } from "./methods/referralStats.js";
export { appealInvoice, resolveAppeal } from "./methods/appeal.js";
export type { AppealInvoiceResult, ResolveAppealResult } from "./methods/appeal.js";
export { submitInvoicesBatch } from "./methods/submitInvoicesBatch.js";
export type { BatchInvoiceItem, SubmitInvoicesBatchResult } from "./methods/submitInvoicesBatch.js";
export { joinFundQueue, resolveFundQueue } from "./methods/fundQueue.js";
export type { JoinFundQueueResult, ResolveFundQueueResult } from "./methods/fundQueue.js";
export { pause, unpause } from "./methods/adminControls.js";
export type { PauseResult } from "./methods/adminControls.js";
export { transferInvoice } from "./methods/transferInvoice.js";
export type { TransferInvoiceResult } from "./methods/transferInvoice.js";
export { convertInvoiceToken } from "./methods/convertInvoiceToken.js";
export type { ConvertInvoiceTokenResult } from "./methods/convertInvoiceToken.js";
export { TokenRegistry, tokenRegistry } from "./utils/tokenRegistry.js";
export type { TokenInfo, NetworkName } from "./utils/tokenRegistry.js";
export {
  estimateFee,
  estimateSubmitFee,
  estimateFundFee,
} from "./utils/feeCalculator.js";
export type {
  FeeEstimate,
  FeeEstimateOptions,
  EstimateSubmitFeeParams,
  SorobanOperation,
} from "./utils/feeCalculator.js";
export { getTokenDecimals } from "./methods/getTokenDecimals.js";
export {
  buildBatchTransaction,
  submitBatchTransaction,
} from "./methods/batch.js";
export type { BatchContractCall, BatchTransactionOptions, BatchTransactionResult } from "./methods/batch.js";
export { setAdmin, upgrade, setDistributionContract, addToken, removeToken, updateFeeRate, updateMaxDiscount, setPriceOracle, setMaxOracleAge } from "./methods/admin.js";
export { getLpScore } from "./methods/getLpScore.js";
