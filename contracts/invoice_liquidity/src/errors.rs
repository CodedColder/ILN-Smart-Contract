use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ContractError {
    InvoiceNotFound = 1,
    AlreadyFunded = 2,
    AlreadyPaid = 3,
    NotFunded = 4,
    Unauthorized = 5,
    InvalidAmount = 6,
    InvalidDiscountRate = 7,
    InvalidDueDate = 8,
    InvoiceDefaulted = 9,
    NothingToClaim = 10,
    NotYetDefaulted = 11,
    OverfundingRejected = 12,
    InvoiceExpired = 13,
    BatchTooLarge = 14,
    AlreadyCancelled = 15,
    AlreadyInitialized = 16,
    // ── Issue #36: appeal_default ──────────────────────────────────
    /// Payer attempted to appeal an invoice that is already in Appealed state.
    AlreadyAppealed = 17,
    /// Appeal window has closed; appeal can no longer be submitted.
    AppealWindowClosed = 18,
    /// Action requires the invoice to be in Defaulted state.
    NotDefaulted = 19,
    // ── Issue #34: LP priority queue ──────────────────────────────
    /// LP has already joined the fund queue for this invoice.
    AlreadyInQueue = 20,
    /// fund_invoice rejected because a different LP was selected by the priority queue.
    NotApprovedFunder = 21,
    /// Invoice is in Appealed state and cannot be acted upon yet.
    InvoiceAppealed = 22,
    AlreadyDisputed = 23,
    NotDisputed = 24,
    InvoiceDisputed = 25,
    ContractPaused = 26,
    DueDateTooSoon = 27,
    DueDateTooFar = 28,
    SelfInvoice = 29,
    OverpaymentRejected = 30,
    /// Issue #28: payer's reputation is below the configured minimum threshold.
    PayerReputationTooLow = 31,
    ArithmeticOverflow = 32,
    /// Token charges a fee during `transfer`, causing the received amount to differ
    /// from the amount sent and breaking ILN accounting.
    FeeOnTransferToken = 33,
    /// Issue #92: oracle returned unverified for the invoice payer when
    /// require_oracle_verification was set to true.
    PayerUnverified = 34,
    /// Issue #93: oracle data is older than max_oracle_age_ledgers and must
    /// be rejected to prevent stale-data attacks.
    OracleDataStale = 35,
    /// Invoice amount is below the configurable minimum threshold.
    AmountTooSmall = 36,
    /// Reentrant call detected — the function was called while already executing (Issue #535).
    Reentrancy = 37,
    /// Rate-limited function called before the cooldown period elapsed (Issue #541).
    RateLimited = 38,
    /// resolve_fund_queue called before the minimum queue maturity delay has
    /// elapsed since the first LP joined the queue.  Prevents MEV/front-running
    /// attacks where an attacker races to resolve the queue immediately after a
    /// high-reputation LP joins (Issue #MEV-1).
    QueueNotMature = 39,
    /// Cross-contract dependency reported an incompatible interface version
    /// (or the version query failed) during configuration.
    IncompatibleInterfaceVersion = 40,
    /// Issue #circuit-breaker: every oracle in the priority chain for this
    /// feed type + token is circuit-tripped (or absent after excluding a
    /// tripped one) — oracle-gated funding must be rejected rather than
    /// silently proceeding as if no oracle were configured.
    OracleCircuitOpen = 41,
    /// Issue #price-deviation: no price source is registered for the
    /// requested feed type, or every registered source failed to respond.
    NoPriceSource = 42,
    /// Issue #price-deviation: every registered price source's report
    /// deviated beyond the configured threshold from every other — no
    /// source survived to produce a validated price.
    AllPriceSourcesRejected = 43,
    /// Issue #655: invoice `amount` exceeds the governance-configured
    /// staged-rollout per-invoice cap.
    MaxInvoiceAmountExceeded = 44,
    /// Issue #655: funding this amount would push the token's cumulative
    /// funded volume past the governance-configured staged-rollout cap.
    GlobalVolumeCapExceeded = 45,
    /// Issue #817: requested TWAP window is outside the governance-bounded
    /// `[MIN_TWAP_WINDOW_LEDGERS, MAX_TWAP_WINDOW_LEDGERS]` range.
    InvalidTwapWindow = 46,
    // ── Issue #124 / #641 / #640 / #639: multisig admin ────────────
    // These variants were lost in a merge; they are re-added after the
    // current highest code (46) because 44-46 now belong to other errors.
    /// Signer list/threshold combination is invalid (empty signer set,
    /// duplicate signer, or threshold outside `1..=signers.len()`).
    InvalidMultisigConfig = 47,
    /// Caller is not a configured multisig signer.
    NotAuthorizedSigner = 48,
    /// This signer has already approved the given proposal.
    AlreadySigned = 49,
    /// No multisig proposal exists with the given id.
    ProposalNotFound = 50,
    /// The proposal has already been executed and cannot be executed again.
    ProposalAlreadyExecuted = 51,
    /// The proposal's approval window (`MULTISIG_WINDOW_LEDGERS`) has
    /// elapsed; it can no longer be signed or executed.
    ProposalExpired = 52,
    /// The proposal has not yet collected enough signer approvals to be executed.
    ThresholdNotReached = 53,
    /// No multisig admin scheme has been bootstrapped yet.
    MultisigNotConfigured = 54,
    /// `initialize_multisig_admin` was called again after the multisig
    /// admin scheme was already bootstrapped; reconfiguration must go
    /// through the `UpdateMultisig` proposal flow instead.
    MultisigAlreadyConfigured = 55,
    /// Issue #641: a Pending, non-expired proposal already exists for this
    /// exact admin action — reject the duplicate.
    DuplicateProposal = 56,
    /// Issue #640: no pending signer rotation exists to finalize or cancel.
    RotationNotFound = 57,
    /// Issue #640: the rotation's timelock has not yet elapsed.
    RotationTimelockNotExpired = 58,
    /// Issue #640: a signer rotation is already pending; only one may be
    /// in flight at a time.
    RotationAlreadyPending = 59,
}
