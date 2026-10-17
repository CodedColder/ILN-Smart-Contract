# Contract Error Codes

This reference documents all error codes returned by the ILN contracts, grouped by crate.

## `invoice_liquidity` — `ContractError`

Source of truth: [`contracts/invoice_liquidity/src/errors.rs`](../contracts/invoice_liquidity/src/errors.rs)

> **Note:** codes 33 is shared by two variants (`FeeOnTransferToken` and `PayerUnverified`) due to a historical renumbering. Both map to the same numeric value at runtime.

| Code | Variant | Description | Common cause | Recommended remediation |
|------|---------|-------------|--------------|--------------------------|
| 1 | `InvoiceNotFound` | The requested invoice ID does not exist in contract storage. | Caller supplied an invalid or deleted invoice ID. | Check the invoice ID before calling, or load it from a prior successful `submit_invoice` response. |
| 2 | `AlreadyFunded` | The invoice is already funded and cannot be funded again. | A second LP attempted to fund an invoice that already reached the funded state. | Read the invoice status first and stop funding once the invoice is funded. |
| 3 | `AlreadyPaid` | The invoice has already been paid. | A payer or LP retried a payment flow after settlement completed. | Treat the invoice as terminal and skip any additional payment, funding, or collection attempts. |
| 4 | `NotFunded` | The invoice has not been funded yet. | A caller tried to settle, claim, or resolve a flow that requires an active funded invoice. | Fund the invoice first, or wait until the correct state transition has occurred. |
| 5 | `Unauthorized` | The caller does not have the required role or authorization for the action. | Wrong account signed the transaction, or the contract has not been configured with the expected admin/role mapping. | Verify the signing account and required role, then retry with the correct address or permissions. |
| 6 | `InvalidAmount` | The provided amount is not acceptable to the contract. | Zero, negative, or otherwise malformed payment/funding amount. | Send a positive amount that matches the invoice rules and token decimals. |
| 7 | `InvalidDiscountRate` | The discount rate is outside the allowed range. | Admin or caller supplied a rate above the contract maximum or in the wrong units. | Use the documented basis-point format and keep the value within the configured bounds. |
| 8 | `InvalidDueDate` | The due date is not valid for invoice creation or update. | Due date is in the past, malformed, or violates contract invariants. | Provide a future due date that satisfies the contract's validation rules. |
| 9 | `InvoiceDefaulted` | The invoice has already defaulted. | Caller tried to fund, pay, cancel, or otherwise act on an invoice that is already in default. | Use the default/appeal flows instead of settlement or funding flows. |
| 10 | `NothingToClaim` | There is no yield or claimable amount available. | LP tried to claim before yield accrued or before funds became claimable. | Wait until the invoice has generated claimable yield, then retry the claim. |
| 11 | `NotYetDefaulted` | The invoice has not reached the default threshold yet. | A default-claim or default-handling function was called too early. | Wait until the invoice is actually defaulted before using the default recovery flow. |
| 12 | `OverfundingRejected` | The funding attempt would exceed the invoice's remaining amount. | LP sent more than the unpaid principal or attempted to top up beyond the cap. | Fund only the remaining unpaid amount, or read the remaining balance first. |
| 13 | `InvoiceExpired` | The invoice has expired and cannot proceed through normal settlement. | Caller tried to fund or pay after the invoice passed its allowed lifecycle window. | Create a fresh invoice or use the appropriate default/closure flow if supported. |
| 14 | `BatchTooLarge` | The submitted batch exceeds the contract's maximum batch size. | Bulk action included too many invoices in one call. | Split the request into smaller batches and retry. |
| 15 | `AlreadyCancelled` | The invoice was already cancelled. | A caller retried a cancel flow or attempted another action after cancellation. | Treat the invoice as terminal and stop sending state-changing actions for it. |
| 16 | `AlreadyInitialized` | The contract was initialized more than once. | A deployment or setup script ran initialization again after state already existed. | Run initialization only once per deployment and guard scripts against duplicate setup. |
| 17 | `AlreadyAppealed` | An appeal already exists for this invoice. | The payer submitted a second appeal for the same defaulted invoice. | Check whether an appeal is already open before creating another one. |
| 18 | `AppealWindowClosed` | The appeal deadline has passed. | The appeal was submitted after the configured appeal window elapsed. | Submit the appeal before the deadline, or update the contract configuration if the window needs to change. |
| 19 | `NotDefaulted` | The invoice is not currently in the defaulted state required by this action. | A caller attempted to appeal or resolve a default-specific flow before default existed. | Wait until the invoice is defaulted, then retry the default-specific action. |
| 20 | `AlreadyInQueue` | The LP has already joined the funding queue for this invoice. | Duplicate queue enrollment request from the same LP. | Skip re-joining if the LP is already queued, or remove the existing queue entry first. |
| 21 | `NotApprovedFunder` | The LP is not the funder approved by the priority queue. | A different LP attempted to fund before queue resolution selected them. | Wait for queue resolution and fund only when the contract assigns that LP as the approved funder. |
| 22 | `InvoiceAppealed` | The invoice is currently in the appealed state. | Another action was attempted while appeal review is still in progress. | Wait for the appeal to resolve before retrying settlement or closure flows. |
| 23 | `AlreadyDisputed` | The invoice is already disputed. | A caller attempted to open a second dispute on the same invoice. | Check dispute status before filing and avoid re-opening an active dispute. |
| 24 | `NotDisputed` | The invoice is not in a disputed state. | A dispute-resolution function was called before a dispute existed. | Open a dispute first, or call the correct function for the current invoice state. |
| 25 | `InvoiceDisputed` | The invoice is under dispute and cannot proceed through normal settlement. | A user attempted to fund, pay, or finalize an invoice while a dispute is active. | Resolve or dismiss the dispute before retrying normal invoice actions. |
| 26 | `ContractPaused` | The contract is currently paused. | An admin paused the protocol for maintenance, incident response, or governance action. | Wait until the contract is unpaused, or ask the admin/governance process to resume it. |
| 27 | `DueDateTooSoon` | The due date is earlier than the minimum allowed horizon. | Invoice due date was set too close to the current ledger time. | Choose a later due date that satisfies the contract's minimum lead time. |
| 28 | `DueDateTooFar` | The due date is later than the maximum allowed horizon. | Invoice due date was set too far in the future. | Reduce the due date to fall within the contract's configured maximum range. |
| 29 | `SelfInvoice` | The payer and invoice creator are the same address. | A caller attempted to create an invoice against themselves. | Use distinct payer and submitter addresses, or fix the invoice data before resubmitting. |
| 30 | `OverpaymentRejected` | The payment amount exceeds the remaining amount due. | Payer attempted to pay more than the invoice balance. | Pay exactly the remaining amount or query the outstanding balance first. |
| 31 | `PayerReputationTooLow` | The payer's reputation is below the configured minimum threshold. | Reputation gate is enabled and the payer score does not meet the contract requirement. | Improve the payer's reputation score, or adjust the minimum threshold through the approved governance/admin path. |
| 32 | `ArithmeticOverflow` | A checked arithmetic operation overflowed. | Large amounts, counters, or computed values exceeded `u64`/`i128` limits during processing. | Re-check inputs for unreasonable values and investigate the caller data or contract math path. |
| 33 | `FeeOnTransferToken` | The token charges a transfer fee, so the received amount differs from the amount sent. | An unsupported fee-on-transfer asset was added or used for settlement. | Use a standard token that transfers the full amount, or remove the fee-on-transfer asset from configuration. |
| 33 | `PayerUnverified` | The oracle did not verify the payer when verification was required. | Oracle verification is enabled, but the payer is not present or not verified in the oracle response. | Use a verified payer account, or disable payer verification if that policy is not required. |
| 34 | `OracleDataStale` | The oracle response is older than the configured freshness window. | The payer-verification oracle data has exceeded `max_oracle_age_ledgers`. | Refresh oracle data and retry, or increase the freshness window only if that tradeoff is acceptable. |
| 35 | `InvoiceNftAlreadyExists` | An NFT has already been minted for this invoice. | Attempted to mint a duplicate NFT for an invoice that already has one. | Check `invoice_nft_exists` before minting. |
| 36 | `InvoiceNftNotFound` | No NFT exists for the requested invoice. | Querying or transferring an NFT that was never minted (or was burned). | Verify the invoice was funded and the NFT was minted before querying. |
| 37 | `InvoiceNftNotOwned` | The caller is not the current owner of this NFT. | A non-owner attempted to transfer or burn an NFT. | Verify ownership via `invoice_nft_owner` before attempting transfer or burn. |
| 38 | `AmountTooSmall` | The invoice amount is below the configurable minimum threshold. | The submitted invoice amount is too low to be economically viable. | Increase the invoice amount to meet the minimum required by the contract configuration. |
| 68 | `TokenDecimalsUnavailable` | The token does not implement `decimals()`, so its precision cannot be verified. | `add_token` called with a non-SEP-41 / non-SAC contract; or (Issue #1069) `correct_token_decimals` called on a token that can no longer answer `decimals()`. | List only SACs or SEP-41 tokens that report `decimals()`. See [token-safety.md](token-safety.md). |
| 69 | `InvalidTokenDecimals` | Declared decimals exceed 18 or differ from the token's own `decimals()`. | `add_token(token, 6)` for a classic-asset SAC, which reports 7; or (Issue #1069) `correct_token_decimals(token, 6)` for a bootstrap token whose real decimals are 7 — call `audit_token_decimals` first to read the real value. | Pass exactly the value the token reports. |
| 70 | `TokenDecimalsNotConfigured` | The token is approved but has no registered decimals. | Legacy deployment upgraded without running `migrate`. | Run `migrate()` after `upgrade()`, or re-register the token with `add_token`. |
| 71 | `AccountFrozen` | The account's trustline / balance for this token is deauthorized or missing. | Issuer froze the LP, payer, freelancer, admin or contract; or the account has no trustline. | Ask the issuer to re-authorize. Escrowed payouts can then be claimed with `claim_pending_payout`. |
| 72 | `TokenAuthorizationUnavailable` | The token has no SAC `authorized()` interface, so freezes cannot be detected. | Listing, or transacting in, a custom token without the SAC authorization interface. | Use a SAC, or a token that implements `authorized(id)`. |
| 73 | `InsufficientContractBalance` | The contract's unreserved balance cannot cover a payout. | Issuer clawback from the contract's balance; escrowed funds are reserved. | Restore the shortfall to the contract and retry. See the clawback runbook in [token-safety.md](token-safety.md). |
| 75 | `QueueStateArchived` | `resolve_fund_queue` found a non-empty fund queue but its `FundQueueOpenedAt` record is missing. | The opening-ledger record was archived (TTL lapsed) or otherwise lost before the queue could be resolved — see [storage-layout.md § Queued-entry retention guarantees](storage-layout.md#queued-entry-retention-guarantees-issues-1011-1012-1013-1014-1015-1016-1018). | Check `get_fund_queue_ttl_margins(invoice_id)` on other open queues to catch drift early; this specific queue cannot be resolved automatically — treat it as needing manual/admin remediation (e.g. a fresh `join_fund_queue` cannot restore the lost timestamp, so the MEV-mitigation delay cannot be safely reconstructed for this invoice). |
| 76 | `TooManyPriceSources` | `add_price_source` was called for a feed that already holds `MAX_PRICE_SOURCES_PER_FEED` (20) price sources (Issue #1064). | Governance tried to register more than the per-feed cap of price sources (a fresh registration of an already-registered source is a no-op, not this error). | Reuse an existing source, remove one first via `remove_price_source` to free a slot, or set the correct single oracle via `register_oracle`. The cap bounds the number of `PriceOutlierRejected` events a `get_verified_price` read can emit — see [oracle-design.md](oracle-design.md). |

---

## `insurance_pool` — `InsuranceError`

Source of truth: [`contracts/insurance_pool/src/lib.rs`](../contracts/insurance_pool/src/lib.rs)

| Code | Variant | Description | Common cause | Recommended remediation |
|------|---------|-------------|--------------|--------------------------|
| 1 | `NotInitialized` | Contract has not been initialised with an admin. | `initialize()` was never called, or was called and failed. | Call `initialize()` with a valid admin address and positive coverage cap. |
| 2 | `AlreadyClaimed` | A claim has already been processed for this invoice. | Duplicate claim attempt for the same invoice ID. | Check `is_claimed()` before filing; each invoice can only be claimed once. |
| 3 | `InvalidAmount` | Premium / coverage amount must be positive. | Zero or negative amount passed to `deposit_premium` or `initialize`. | Send a positive stroop amount. |
| 4 | `PoolEmpty` | Pool has no balance available to pay a claim. | All premiums have been paid out or the pool was never funded. | LPs must deposit premiums before claims can be paid. |
| 5 | `AlreadyInitialized` | Contract is already initialised. | `initialize()` called more than once. | Initialisation is one-shot; redeploy if a fresh pool is needed. |
| 6 | `NoPendingProposal` | No pending proposal exists for the requested admin action. | `execute_coverage_change` / `execute_admin_transfer` / `cancel_*` called with no queued proposal. | Queue a proposal first via `propose_coverage_change` or `propose_admin_transfer`. |
| 7 | `TimelockNotExpired` | The proposal's timelock has not yet expired. | Attempted to execute a timelocked action before the 3-day delay elapsed. | Wait until `env.ledger().timestamp() >= eta` and retry. |
| 8 | `ArithmeticOverflow` | A checked arithmetic operation overflowed during premium accumulation. | Depositing an amount that would overflow `i128` on the running balance or per-LP premium counter. | Ensure deposit amounts are within sane bounds; this should only occur with extreme/malicious inputs. |
| 9 | `BalanceCapExceeded` | Premium deposit would push the pool balance above the configured cap. | The admin has set a `BalanceCap` and the incoming deposit would exceed it. | Reduce the deposit amount or ask the admin to raise the cap via `set_balance_cap`. |
| 10 | `SolvencyCircuitOpen` | A new claim payout has been paused because the solvency circuit breaker is open. | The pool's reserve ratio fell below `MinReserveRatioBps` (Issue #826) and the sticky breaker tripped. | Governance must restore the reserve and call `reset_solvency_circuit`; enrollments and premium deposits continue unaffected. |
| 11 | `InvalidReserveRatio` | The minimum reserve ratio (bps) must be within `0..=10_000`. | `set_min_reserve_ratio_bps` / `set_backstop_funding_bps` called with a bps value above 10_000. | Use a value in the valid basis-point range. |
| 12 | `EvidenceRequired` | A claim gated by the review window was attempted before on-chain evidence was submitted. | `ReviewWindowSeconds > 0` and `submit_claim_evidence` was never called for the invoice (Issue #828). | Submit evidence via `submit_claim_evidence` before claiming, or disable the review gate. |
| 13 | `ReviewWindowNotElapsed` | A claim gated by the review window was attempted before the window elapsed. | Evidence was submitted but `env.ledger().timestamp()` is still within `submitted_at + ReviewWindowSeconds` (Issue #828). | Retry the claim after the review window elapses. |
| 14 | `InvalidBackstopAmount` | A backstop top-up was requested with a non-positive amount. | `top_up_backstop` called with `amount <= 0` (Issue #827). | Pass a positive stroop amount. |

---

## `iln_governance` — `GovernanceError`

Source of truth: [`contracts/iln_governance/src/lib.rs`](../contracts/iln_governance/src/lib.rs)

| Code | Variant | Description | Common cause | Recommended remediation |
|------|---------|-------------|--------------|--------------------------|
| 1 | `AlreadyInitialized` | The contract was initialized more than once. | Deployment script ran initialization twice. | Run initialization only once per deployment. |
| 2 | `ProposalNotFound` | The specified proposal ID does not exist. | Invalid proposal ID or the proposal was never created. | Verify the proposal ID via `get_proposal`. |
| 3 | `VotingEnded` | Voting period for this proposal has ended. | Attempted to cast a vote after the voting window closed. | Vote within the configured voting period. |
| 4 | `ProposalNotActive` | The proposal is not in the `Active` state required by this action. | Action requires an active proposal (e.g. voting) but the proposal is in a different state. | Check proposal status before acting. |
| 5 | `NoVotingPower` | The voter has no governance token balance at the snapshot block. | Voter held no tokens when the proposal was created. | Acquire governance tokens before the proposal's snapshot block. |
| 6 | `AlreadyVoted` | The address has already voted on this proposal. | Double-vote attempt on the same proposal. | Each address may vote once per proposal. |
| 7 | `VotingOngoing` | The proposal's voting period is still in progress. | Attempted to finalize or execute a proposal before voting ended. | Wait for the voting period to end. |
| 8 | `QuorumNotReached` | The proposal did not meet the minimum participation threshold. | Insufficient total votes relative to governance token supply. | Encourage more token holders to vote. |
| 9 | `ProposalRejected` | The proposal was rejected (more votes against than for). | More weight voted against the proposal. | Revise the proposal and resubmit. |
| 10 | `AlreadyResolved` | The proposal has already been resolved (passed/rejected/executed/vetoed). | Action on a proposal that is already in a terminal state. | No further action is possible on a resolved proposal. |
| 11 | `CannotDelegateToSelf` | Delegating to self is not allowed. | `delegate_votes` called with `to == caller`. | Delegate to a different address. |
| 12 | `DelegationCyclePrevented` | Delegation would create a cycle. | Delegating A → B → A (directly or transitively). | Break the delegation chain before delegating. |
| 13 | `TimelockNotExpired` | Execution timelock has not yet expired. | Attempted to execute a passed proposal before the governance timelock delay elapsed. | Wait for the timelock delay, then call `execute_proposal`. |
| 14 | `Unauthorized` | Caller does not have the required role. | Wrong account signed the transaction. | Verify the signing account matches the required role (admin, proposer, etc.). |
| 15 | `InvalidQuorumBps` | Invalid quorum basis points (must be 1..=10,000). | Admin set quorum outside the valid range. | Set quorum between 1 and 10,000 basis points. |
| 16 | `NotAdmin` | Caller is not the admin. | Non-admin attempted an admin-only action (e.g. `veto_proposal`). | Use the admin account. |
| 17 | `NotVetoable` | Proposal cannot be vetoed in its current status. | Admin attempted to veto a proposal that is not in `Active` status. | Veto only active proposals. |
| 18 | `VetoPowerDisabled` | Admin veto power has been disabled by governance. | Admin veto was disabled via a governance proposal. | Re-enable veto via governance before using it. |
| 19 | `InsufficientProposerBalance` | Proposer does not hold the minimum required token balance. | Proposer's token balance is below `MinProposalBalance`. | Acquire enough governance tokens to meet the proposal threshold. |
| 28 | `NotInitialized` | Contract called before `initialize()`. | Calling an admin-gated function on a contract instance that was never initialized. | Call `initialize()` first. |
| 29 | `InsufficientHoldingPeriod` | Voter has no balance checkpoint predating the proposal by `MIN_VOTE_HOLD_LEDGERS` (10 ledgers). | First vote (or delegation) from a newly funded address; possible flash-loan attempt. | Call `checkpoint_balance` and wait ~10 ledgers (or vote on a later proposal). |

---

## Keeping This Doc Current

When you add, remove, or renumber variants in any contract's error enum:

1. Update the relevant table above.
2. Update any client-side error mapping in SDKs or examples.
3. Keep the README link below pointing here so the reference remains easy to find.
