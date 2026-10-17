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
    /// Issue #1091: the fund queue is at `MAX_FUND_QUEUE_LEN` and the
    /// joining LP's score does not strictly exceed the lowest queued score.
    QueueFull = 47,
    /// Issue #1090: `fund_invoice` called while LPs are queued for this
    /// invoice but `resolve_fund_queue` has not yet selected a winner.
    /// Funding must wait for resolution so the queue cannot be bypassed.
    QueueUnresolved = 48,
    /// Issue #1093: fewer price sources responded (or survived outlier
    /// rejection) than the governance-configured minimum quorum.
    PriceQuorumNotMet = 49,
    // ── Variants referenced by merged code but previously undefined ──
    // (restored so the crate compiles; codes appended, existing codes unchanged)
    /// Multisig admin has already been initialized.
    MultisigAlreadyConfigured = 50,
    /// Signer set / threshold combination is invalid.
    InvalidMultisigConfig = 51,
    /// A multisig operation was attempted before multisig was configured.
    MultisigNotConfigured = 52,
    /// Caller is not in the multisig signer set.
    NotAuthorizedSigner = 53,
    /// An identical pending proposal already exists.
    DuplicateProposal = 54,
    /// No proposal exists with the given id.
    ProposalNotFound = 55,
    /// Proposal has already been executed.
    ProposalAlreadyExecuted = 56,
    /// Proposal is past its expiry and can no longer be signed or executed.
    ProposalExpired = 57,
    /// Signer has already approved this proposal.
    AlreadySigned = 58,
    /// Proposal does not yet have enough approvals to execute.
    ThresholdNotReached = 59,
    /// A signer rotation is already scheduled (Issue #640).
    RotationAlreadyPending = 60,
    /// No signer rotation is scheduled (Issue #640).
    RotationNotFound = 61,
    /// Signer rotation timelock has not yet elapsed (Issue #640).
    RotationTimelockNotExpired = 62,
    /// TWAP read requested with fewer observations than required.
    InsufficientTwapObservations = 63,
    /// Fund-queue index invariant violated (unreachable under normal execution).
    QueueIndexOutOfBounds = 64,
    /// Funder-list index invariant violated (unreachable under normal execution).
    FunderIndexOutOfBounds = 65,
    /// `decay_rate_bps` outside its governance bound.
    InvalidDecayRate = 66,
    /// `high_rep_threshold` outside its governance bound.
    InvalidRepThreshold = 67,
    // ── Issues #1070–#1073: token configuration and SAC safety ─────
    /// Issue #1070: the token does not answer `decimals()`, so its
    /// precision cannot be verified and it cannot be allowlisted.
    TokenDecimalsUnavailable = 68,
    /// Issue #1070: declared decimals exceed `MAX_TOKEN_DECIMALS` or differ
    /// from the token's own `decimals()`.
    InvalidTokenDecimals = 69,
    /// Issue #1070: the token is allowlisted but has no registered decimals
    /// (legacy state); re-register it or run `migrate`.
    TokenDecimalsNotConfigured = 70,
    /// Issue #1072: the account's trustline / balance for this token is
    /// deauthorized (frozen) or missing.
    AccountFrozen = 71,
    /// Issue #1073: the token does not expose the SAC `authorized()`
    /// interface, so issuer freezes cannot be detected.
    TokenAuthorizationUnavailable = 72,
    /// Issue #1071: the contract's unreserved balance cannot cover a payout
    /// (e.g. after an issuer clawback).
    InsufficientContractBalance = 73,
    /// Issue #1031: an LP tried to join the fund queue while the queue
    /// already has at least one entry, and the LP's score does not exceed
    /// the current best score by the governance-configured tie-score margin.
    /// The margin is set via `set_queue_tie_score_margin` (admin-only) and
    /// raises the economic barrier to equal-score manipulation: a fresh LP
    /// at the default score cannot silently tie with an established LP
    /// without first building `margin` additional funded invoices.
    ScoreBelowTieMargin = 74,
    /// Issue #1013: `resolve_fund_queue` found a non-empty fund queue but no
    /// `FundQueueOpenedAt` record for it. That record is only ever absent for
    /// a non-empty queue if it was archived (TTL lapsed) or otherwise lost —
    /// never because no LP had joined. Previously this case silently skipped
    /// the MEV-mitigation maturity delay instead of rejecting the call; it is
    /// now a hard failure so corrupted/archived queue state fails closed.
    QueueStateArchived = 75,
    /// Issue #1064: `add_price_source` rejected because the feed already has
    /// `MAX_PRICE_SOURCES_PER_FEED` sources registered. Bounding the list
    /// bounds the number of `PriceOutlierRejected` events a single
    /// `get_verified_price` invocation can emit.
    TooManyPriceSources = 76,
}
