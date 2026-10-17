use soroban_sdk::{contracttype, Address, BytesN, Env, Symbol};
use soroban_sdk::testutils::storage::Persistent as _;

use crate::config::Config;
use crate::invoice::{AppealRecord, Invoice, InvoiceCore, InvoiceMetadata, ReputationScore};
use crate::multisig::AdminAction;

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataKey {
    // Instance Storage
    Admin,
    Config,
    FeeRate,
    MaxDiscountRate,
    DistributionContract,
    Paused,
    /// Minimum payer reputation required to fund an invoice (Issue #28). Default 0.
    MinPayerReputation,
    NextInvoiceId,
    /// Issue #655: governance-configurable cap on a single invoice's `amount`,
    /// for a staged mainnet rollout. `0` = uncapped (default).
    MaxInvoiceAmount,

    // Persistent Storage
    Invoice(u64),         // DEPRECATED: kept for backwards compatibility
    InvoiceCore(u64),     // NEW: hot-path core data (accessed >95% of time)
    InvoiceMetadata(u64), // NEW: cold-path metadata (accessed <5% of time)
    InvoiceCount,
    Token,
    PayerScore(Address),
    InvoiceFunders(u64),
    ApprovedToken(Address),
    TokenList,
    /// Decimal precision for each allowlisted token (e.g. 6 for USDC, 7 for XLM).
    TokenDecimals(Address),
    /// Detailed reputation profile per address (Issue #26).
    Reputation(Address),
    Appeal(u64),
    PreDefaultPayerScore(u64),
    LpScore(Address),
    FundQueue(u64),
    QueueResolution(u64),
    /// Ledger sequence when the first LP joined the fund queue for an invoice.
    /// Used to enforce a minimum maturity delay before `resolve_fund_queue` may
    /// be called, preventing MEV / front-running (Issue #MEV-1).
    FundQueueOpenedAt(u64),
    /// Issue #645: ring-buffer slot for the admin action audit log, indexed
    /// by `seq % ADMIN_ACTION_LOG_CAPACITY`.
    AdminActionLog(u32),
    /// Issue #645: monotonically increasing sequence counter for the admin
    /// action audit log (instance storage).
    AdminActionCount,

    // Stats (Persistent)
    TotalInvoices,
    TotalFunded,
    TotalPaid,
    TotalVolumeUsdc,
    TotalVolumeEurc,
    TotalVolumeXlm,
    TokenVolume(Address),
    /// Issue #655: governance-configurable cap on cumulative funded volume
    /// (`TokenVolume`) for a given token, for a staged mainnet rollout — can
    /// be raised over time as confidence in the deployment grows. `0` =
    /// uncapped (default).
    TokenVolumeCap(Address),
    /// Referral counts keyed by fixed-size code
    ReferralCount(BytesN<32>),
    Dispute(u64),
    SubmitterInvoices(Address),
    LpInvoices(Address),
    /// Fixed-size min-heap of the top payers by reputation score (Issue #77).
    TopPayersHeap,
    /// NFT Metadata storage (Issue #423)
    InvoiceNft(u64),
    /// NFT Owner tracking (Issue #423)
    InvoiceNftOwner(u64),
    /// Issue #533: Fee tier configuration — ordered list of (min_amount, fee_rate_bps).
    FeeTiers,
    /// Issue #539: Storage version tracking for migration safety.
    StorageVersion,
    /// Reentrancy guard lock (Issue #535)
    ReentrancyLock,
    /// Last ledger sequence when each rate-limited function was called (Issue #541).
    /// Keyed by a Symbol representing the function name.
    RateLimit(Symbol),
    /// Issue #532: governance-controlled oracle registry default, keyed by feed type.
    OracleRegistry(crate::oracle_registry::OracleFeedType),
    /// Issue #532: per-token oracle override, takes priority over OracleRegistry.
    TokenOracle(crate::oracle_registry::OracleFeedType, Address),
    /// Issue #532: last recorded oracle health snapshot for a feed type + token.
    OracleHealth(crate::oracle_registry::OracleFeedType, Address),
    /// Issue #529: deployed insurance pool contract address, consulted by
    /// claim_default() to compensate enrolled LPs on a confirmed default.
    InsurancePool,
    /// Cached insurance pool interface version verified at configuration time.
    InsurancePoolInterfaceVersion,
    /// Cached oracle interface version verified at register_oracle time,
    /// keyed by feed type.
    OracleInterfaceVersion(crate::oracle_registry::OracleFeedType),
    /// Issue #circuit-breaker: whether the oracle circuit breaker for a
    /// feed type + token resolution channel is tripped (sticky — cleared
    /// only via governance-gated `reset_oracle_circuit`, never auto-cleared
    /// by a fresh query, to avoid flapping).
    OracleCircuitTripped(crate::oracle_registry::OracleFeedType, Address),
    /// Issue #price-deviation: list of registered price-reporting oracle
    /// sources for a feed type, consulted together for cross-source
    /// deviation checking. Distinct from OracleRegistry/TokenOracle (the
    /// single-oracle model used for boolean payer verification, where
    /// deviation checking doesn't apply).
    PriceSources(crate::oracle_registry::OracleFeedType),
    /// Issue #price-deviation: governance-configurable maximum allowed
    /// deviation (basis points) between a price source's reported price
    /// and the cross-source median before it's rejected as an outlier.
    MaxPriceDeviationBps,
    /// Issue #816: per-feed TWAP opt-in flag. `true` routes `Price`
    /// reads through the windowed TWAP average; `false` (default) keeps
    /// the existing raw spot behavior. Stored per feed type since not
    /// every feed has enough liquidity/data points for a meaningful window.
    TwapEnabled(crate::oracle_registry::OracleFeedType),
    /// Issue #817: governance-configurable TWAP window in ledgers, bounded
    /// by `MIN/MAX_TWAP_WINDOW_LEDGERS`. Distinct from the
    /// `max_oracle_age_ledgers` staleness bound.
    TwapWindowLedgers,
    MinTwapObservations,
    /// Issue #815/#816: chronological TWAP price samples per feed + token,
    /// backing the opt-in windowed average.
    TwapSamples(crate::oracle_registry::OracleFeedType, Address),

    // ── Issue #124 / #641: multisig admin ───────────────────────────
    /// The multisig admin signer set + approval threshold, once bootstrapped
    /// via `initialize_multisig_admin`.
    MultisigAdmin,
    /// A pending/executed/expired multisig proposal, keyed by its id.
    MultisigProposal(u64),
    /// Monotonically increasing multisig proposal id counter.
    NextProposalId,
    /// Issue #641: the id of the currently in-flight (Pending,
    /// non-expired) proposal for a given `AdminAction`, if any — used to
    /// reject a second concurrent proposal for the same logical action
    /// instead of allowing duplicates to race each other.
    PendingActionProposal(AdminAction),
    /// Issue #640: the currently scheduled signer rotation (old signer ->
    /// new signer + timelock expiry), if any. Only one rotation may be
    /// pending at a time.
    PendingSignerRotation,
    /// Issue #775: ledger timestamp of the most recent `pause()` (single-admin
    /// or multisig path). Absent until the contract has been paused at least
    /// once. Read via `get_protocol_status()` for the public status view;
    /// never cleared on `unpause()` so operators keep a "last halted at"
    /// reference.
    LastPauseTimestamp,
    /// Issue #1093: governance-configurable minimum number of price
    /// sources that must respond *and* survive outlier rejection before
    /// `get_verified_price` returns a value. Absent = 1 (legacy behavior).
    MinPriceQuorum,
    /// Issue #1072: payout owed to `(recipient, token)` that could not be
    /// delivered because the recipient's trustline was frozen. Claimable via
    /// `claim_pending_payout` once re-authorized.
    PendingPayout(Address, Address),
    /// Issue #1071/#1072: sum of all `PendingPayout` entries for a token —
    /// reserved balance that new payouts may not spend.
    PendingPayoutTotal(Address),
    /// Issue #1031: governance-configurable minimum score margin above the
    /// current queue leader that a new joiner must have.  `0` (default) means
    /// the pre-existing behaviour is preserved: any score ≥ best_score is
    /// allowed to join.  A non-zero margin requires a joining LP to score
    /// `best_score + margin` or higher, raising the economic barrier to
    /// equal-score manipulation.  Stored as a `u32` in instance storage.
    QueueTieScoreMargin,
}

// ----------------------------------------------------------------
// Config Helpers
// ----------------------------------------------------------------

pub fn get_admin(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::Admin)
}

pub fn set_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&DataKey::Admin, admin);
}

pub fn get_config(env: &Env) -> Option<Config> {
    env.storage().instance().get(&DataKey::Config)
}

pub fn set_config(env: &Env, config: &Config) {
    env.storage().instance().set(&DataKey::Config, config);
}

/// Issue #529: the configured insurance pool contract address, if any.
pub fn get_insurance_pool(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::InsurancePool)
}

pub fn set_insurance_pool(env: &Env, pool: &Address) {
    env.storage().instance().set(&DataKey::InsurancePool, pool);
}

pub fn set_insurance_pool_interface_version(env: &Env, version: u32) {
    env.storage()
        .instance()
        .set(&DataKey::InsurancePoolInterfaceVersion, &version);
}

pub fn get_insurance_pool_interface_version(env: &Env) -> Option<u32> {
    env.storage()
        .instance()
        .get(&DataKey::InsurancePoolInterfaceVersion)
}

pub fn set_oracle_interface_version(
    env: &Env,
    feed_type: crate::oracle_registry::OracleFeedType,
    version: u32,
) {
    env.storage()
        .instance()
        .set(&DataKey::OracleInterfaceVersion(feed_type), &version);
}

pub fn get_oracle_interface_version(
    env: &Env,
    feed_type: crate::oracle_registry::OracleFeedType,
) -> Option<u32> {
    env.storage()
        .instance()
        .get(&DataKey::OracleInterfaceVersion(feed_type))
}

pub fn is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
}

pub fn set_paused(env: &Env, paused: bool) {
    env.storage().instance().set(&DataKey::Paused, &paused);
}

// ----------------------------------------------------------------
// Invoice Helpers
// ----------------------------------------------------------------

/// Save invoice by splitting into hot and cold data for optimized storage.
///
/// This function saves the invoice in two separate storage entries:
/// - InvoiceCore: Contains frequently-accessed fields (>95% of operations)
/// - InvoiceMetadata: Contains rarely-accessed fields (<5% of operations)
///
/// This optimization reduces gas costs by:
/// 1. Minimizing serialization/deserialization of cold data on hot paths
/// 2. Reducing the size of data moved in/out of storage
///
/// **Gas savings:** ~10-15% on fund_invoice and mark_paid operations
pub fn save_invoice(env: &Env, invoice: &Invoice) {
    let id = invoice.id;

    // Save hot-path core data
    let core_key = DataKey::InvoiceCore(id);
    let core = invoice.to_core();
    env.storage().persistent().set(&core_key, &core);
    env.storage()
        .persistent()
        .extend_ttl(&core_key, 1_000_000, 2_000_000);

    // Save cold-path metadata
    let metadata_key = DataKey::InvoiceMetadata(id);
    let metadata = invoice.to_metadata();
    env.storage().persistent().set(&metadata_key, &metadata);
    env.storage()
        .persistent()
        .extend_ttl(&metadata_key, 1_000_000, 2_000_000);
}

/// Load invoice by combining hot and cold data from storage.
///
/// This function reconstructs the full Invoice by loading:
/// - InvoiceCore: Hot-path data (always present)
/// - InvoiceMetadata: Cold-path data (always present after split)
///
/// Falls back to old Invoice key format for backwards compatibility
/// with data written before the split was implemented.
pub fn load_invoice(env: &Env, id: u64) -> Invoice {
    // Try new split format first (preferred)
    if let Some(core) = env
        .storage()
        .persistent()
        .get::<DataKey, crate::invoice::InvoiceCore>(&DataKey::InvoiceCore(id))
    {
        if let Some(metadata) = env
            .storage()
            .persistent()
            .get::<DataKey, crate::invoice::InvoiceMetadata>(&DataKey::InvoiceMetadata(id))
        {
            return core.with_metadata(metadata);
        }
    }

    // Fall back to old format for backwards compatibility
    env.storage()
        .persistent()
        .get(&DataKey::Invoice(id))
        .expect("invoice not found")
}

pub fn invoice_exists(env: &Env, id: u64) -> bool {
    // Check new split format first, then fall back to old format
    env.storage().persistent().has(&DataKey::InvoiceCore(id))
        || env.storage().persistent().has(&DataKey::Invoice(id))
}

/// Load only the hot-path core data of an invoice.
///
/// This function is optimized for hot paths (fund_invoice, mark_paid)
/// that don't need metadata. Avoids deserializing cold data.
///
/// Returns None if invoice not found, panics with "invoice core not found"
/// if the core is missing (data corruption).
pub fn load_invoice_core(env: &Env, id: u64) -> crate::invoice::InvoiceCore {
    // Try new split format first
    if let Some(core) = env
        .storage()
        .persistent()
        .get::<DataKey, crate::invoice::InvoiceCore>(&DataKey::InvoiceCore(id))
    {
        return core;
    }

    // Fall back to old format and extract core
    if let Some(invoice) = env
        .storage()
        .persistent()
        .get::<DataKey, Invoice>(&DataKey::Invoice(id))
    {
        return invoice.to_core();
    }

    panic!("invoice core not found");
}

/// Try to load only the hot-path core data of an invoice.
///
/// This function is optimized for hot paths and returns None if not found.
pub fn try_load_invoice_core(env: &Env, id: u64) -> Option<crate::invoice::InvoiceCore> {
    // Try new split format first
    if let Some(core) = env
        .storage()
        .persistent()
        .get::<DataKey, crate::invoice::InvoiceCore>(&DataKey::InvoiceCore(id))
    {
        return Some(core);
    }

    // Fall back to old format and extract core
    if let Some(invoice) = env
        .storage()
        .persistent()
        .get::<DataKey, Invoice>(&DataKey::Invoice(id))
    {
        return Some(invoice.to_core());
    }

    None
}

pub fn read_next_invoice_id(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::NextInvoiceId)
        .unwrap_or(1)
}

pub fn write_next_invoice_id(env: &Env, id: u64) {
    env.storage().instance().set(&DataKey::NextInvoiceId, &id);
}

pub fn next_invoice_id(env: &Env) -> Result<u64, crate::errors::ContractError> {
    let current_id = read_next_invoice_id(env);
    let next_id = current_id
        .checked_add(1)
        .ok_or(crate::errors::ContractError::ArithmeticOverflow)?;

    write_next_invoice_id(env, next_id);

    Ok(current_id)
}

// ----------------------------------------------------------------
// Funder List Helpers
// ----------------------------------------------------------------

pub fn get_invoice_funders(env: &Env, id: u64) -> soroban_sdk::Vec<(Address, i128)> {
    env.storage()
        .persistent()
        .get(&DataKey::InvoiceFunders(id))
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

pub fn save_invoice_funders(env: &Env, id: u64, funders: &soroban_sdk::Vec<(Address, i128)>) {
    env.storage()
        .persistent()
        .set(&DataKey::InvoiceFunders(id), funders);
}

// ----------------------------------------------------------------
// Reputation Helpers
// ----------------------------------------------------------------

pub fn get_lp_score(env: &Env, lp: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::LpScore(lp.clone()))
        .unwrap_or(50)
}

pub fn set_lp_score(env: &Env, lp: &Address, score: u32) {
    let score = score.min(100);
    env.storage()
        .persistent()
        .set(&DataKey::LpScore(lp.clone()), &score);
}

// ----------------------------------------------------------------
// LP Queue Helpers
//
// Issue #1014: centralized. These keys (`FundQueue`, `FundQueueOpenedAt`,
// `QueueResolution`) and their TTL-extension logic live in exactly one
// place — `invoice::save_fund_queue`, `invoice::try_set_fund_queue_opened_at`,
// `invoice::save_queue_resolution`, and the private `invoice::extend_queue_key_ttl`
// they all call — because that is the module `lib.rs` actually imports from
// for `join_fund_queue` / `resolve_fund_queue`.
//
// This file used to carry a second, parallel copy of the same six functions
// (added for Issue #1091) that called `extend_ttl` while the `invoice.rs`
// copies did not, so the TTL-extension fix lived in code nothing ever called
// — `lib.rs` only ever imported the `invoice.rs` versions. Issue #1011 moved
// the `extend_ttl` logic into the copies that are actually on the call path,
// which made this module's copies fully redundant; they have been removed
// so there is only one implementation to maintain and no risk of a future
// change landing in the unused half again. See `docs/storage-layout.md` §
// "Queued-entry retention guarantees" for the full before/after history.
// ----------------------------------------------------------------

// ----------------------------------------------------------------
// Appeal Helpers
// ----------------------------------------------------------------

pub fn get_appeal(env: &Env, invoice_id: u64) -> Option<AppealRecord> {
    env.storage().persistent().get(&DataKey::Appeal(invoice_id))
}

pub fn save_appeal(env: &Env, invoice_id: u64, record: &AppealRecord) {
    env.storage()
        .persistent()
        .set(&DataKey::Appeal(invoice_id), record);
}

pub fn save_pre_default_payer_score(env: &Env, invoice_id: u64, score: u32) {
    env.storage()
        .persistent()
        .set(&DataKey::PreDefaultPayerScore(invoice_id), &score);
}

pub fn get_pre_default_payer_score(env: &Env, invoice_id: u64) -> Option<u32> {
    env.storage()
        .persistent()
        .get(&DataKey::PreDefaultPayerScore(invoice_id))
}

// ----------------------------------------------------------------
// Contract Stats Helpers
// ----------------------------------------------------------------

/// In-memory stats accumulator for optimizing batch updates.
/// Use this struct to accumulate multiple stat changes and commit
/// them with a single storage operation for better gas efficiency.
#[derive(Clone, Debug)]
pub struct StatsAccumulator {
    pub invoices_delta: i64,
    pub funded_delta: i64,
    pub paid_delta: i64,
}

impl StatsAccumulator {
    pub fn new() -> Self {
        StatsAccumulator {
            invoices_delta: 0,
            funded_delta: 0,
            paid_delta: 0,
        }
    }

    pub fn add_invoice(&mut self) {
        self.invoices_delta = self.invoices_delta.saturating_add(1);
    }

    pub fn add_funded(&mut self) {
        self.funded_delta = self.funded_delta.saturating_add(1);
    }

    pub fn add_paid(&mut self) {
        self.paid_delta = self.paid_delta.saturating_add(1);
    }

    /// Commit accumulated deltas to persistent storage.
    /// This is more efficient than calling increment_* functions multiple times.
    pub fn commit(self, env: &Env) {
        if self.invoices_delta > 0 {
            let current: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::TotalInvoices)
                .unwrap_or(0);
            env.storage().persistent().set(
                &DataKey::TotalInvoices,
                &current.saturating_add(self.invoices_delta as u64),
            );
        }
        if self.funded_delta > 0 {
            let current: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::TotalFunded)
                .unwrap_or(0);
            env.storage().persistent().set(
                &DataKey::TotalFunded,
                &current.saturating_add(self.funded_delta as u64),
            );
        }
        if self.paid_delta > 0 {
            let current: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::TotalPaid)
                .unwrap_or(0);
            env.storage().persistent().set(
                &DataKey::TotalPaid,
                &current.saturating_add(self.paid_delta as u64),
            );
        }
    }
}

pub fn increment_total_invoices(env: &Env) {
    let current: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::TotalInvoices)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&DataKey::TotalInvoices, &current.saturating_add(1));
}

pub fn increment_total_funded(env: &Env) {
    let current: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::TotalFunded)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&DataKey::TotalFunded, &current.saturating_add(1));
}

pub fn increment_total_paid(env: &Env) {
    let current: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::TotalPaid)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&DataKey::TotalPaid, &current.saturating_add(1));
}

// add_volume moved to invoice.rs where the configured token addresses are available

/// Get current total invoices count.
pub fn get_total_invoices(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::TotalInvoices)
        .unwrap_or(0)
}

/// Get current total funded count.
pub fn get_total_funded(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::TotalFunded)
        .unwrap_or(0)
}

/// Get current total paid count.
pub fn get_total_paid(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::TotalPaid)
        .unwrap_or(0)
}

// ----------------------------------------------------------------
// Issue #1082: Pre-upgrade TTL field validation
// ----------------------------------------------------------------

/// Guard that rejects a contract upgrade when any live persistent storage
/// entry carrying critical invoice or fund-queue data has a remaining TTL
/// below [`crate::constants::TTL_MIN_LEDGERS_BEFORE_UPGRADE`].
///
/// ## Why this guard exists
///
/// Soroban persistent storage entries are individually TTL-managed.  If an
/// upgrade were allowed to proceed while one or more of these entries had
/// already drifted close to expiry (e.g. because an earlier code path
/// forgot to call `extend_ttl`, or because the admin deliberately omitted
/// the renewal step to silently archive data), the new WASM would start
/// with live state it believes it can read, but those entries would expire
/// mid-flight and cause silent data loss.
///
/// ## What is checked
///
/// The most-recent `TTL_UPGRADE_SAMPLE_SIZE` invoices (by ID, counting down
/// from `NextInvoiceId`) are sampled.  For each ID the following keys are
/// checked *if they exist* in persistent storage:
///
/// * `InvoiceCore(id)`
/// * `InvoiceMetadata(id)`
/// * `FundQueue(id)`
/// * `FundQueueOpenedAt(id)`
/// * `QueueResolution(id)`
///
/// If *any* existing entry has `get_ttl(key) < TTL_MIN_LEDGERS_BEFORE_UPGRADE`
/// the function returns `Err(ContractError::UpgradeRemovesTtlFields)` and the
/// upgrade is rejected **fail-closed** — the WASM swap is never executed.
///
/// ## Remediation
///
/// Before retrying the upgrade the admin should call `save_invoice` for the
/// affected invoices (which calls `extend_ttl`) and/or the relevant queue
/// helpers in `invoice.rs` (`invoice::save_fund_queue`,
/// `invoice::save_queue_resolution`, `invoice::try_set_fund_queue_opened_at`
/// — the module `lib.rs` actually calls; see Issue #1014) to restore TTLs.
pub fn validate_ttl_fields_pre_upgrade(
    env: &Env,
) -> Result<(), crate::errors::ContractError> {
    use crate::constants::{TTL_MIN_LEDGERS_BEFORE_UPGRADE, TTL_UPGRADE_SAMPLE_SIZE};

    // Determine the upper bound of invoice IDs written so far.
    let next_id = read_next_invoice_id(env);
    if next_id <= 1 {
        // No invoices have been written yet — nothing to check.
        return Ok(());
    }

    // The last written ID is (next_id - 1).  We sample at most
    // TTL_UPGRADE_SAMPLE_SIZE IDs counting downward from the most recent.
    let last_id = next_id - 1;
    let sample_start = last_id.saturating_sub(TTL_UPGRADE_SAMPLE_SIZE.saturating_sub(1));

    // Iterate sample_start ..= last_id
    let mut id = last_id;
    loop {
        // --- InvoiceCore(id) ---
        let core_key = DataKey::InvoiceCore(id);
        if env.storage().persistent().has(&core_key) {
            let ttl = env.storage().persistent().get_ttl(&core_key);
            if ttl < TTL_MIN_LEDGERS_BEFORE_UPGRADE {
                return Err(crate::errors::ContractError::UpgradeRemovesTtlFields);
            }
        }

        // --- InvoiceMetadata(id) ---
        let meta_key = DataKey::InvoiceMetadata(id);
        if env.storage().persistent().has(&meta_key) {
            let ttl = env.storage().persistent().get_ttl(&meta_key);
            if ttl < TTL_MIN_LEDGERS_BEFORE_UPGRADE {
                return Err(crate::errors::ContractError::UpgradeRemovesTtlFields);
            }
        }

        // --- FundQueue(id) ---
        let fq_key = DataKey::FundQueue(id);
        if env.storage().persistent().has(&fq_key) {
            let ttl = env.storage().persistent().get_ttl(&fq_key);
            if ttl < TTL_MIN_LEDGERS_BEFORE_UPGRADE {
                return Err(crate::errors::ContractError::UpgradeRemovesTtlFields);
            }
        }

        // --- FundQueueOpenedAt(id) ---
        let fqoa_key = DataKey::FundQueueOpenedAt(id);
        if env.storage().persistent().has(&fqoa_key) {
            let ttl = env.storage().persistent().get_ttl(&fqoa_key);
            if ttl < TTL_MIN_LEDGERS_BEFORE_UPGRADE {
                return Err(crate::errors::ContractError::UpgradeRemovesTtlFields);
            }
        }

        // --- QueueResolution(id) ---
        let qr_key = DataKey::QueueResolution(id);
        if env.storage().persistent().has(&qr_key) {
            let ttl = env.storage().persistent().get_ttl(&qr_key);
            if ttl < TTL_MIN_LEDGERS_BEFORE_UPGRADE {
                return Err(crate::errors::ContractError::UpgradeRemovesTtlFields);
            }
        }

        if id == sample_start {
            break;
        }
        id -= 1;
    }

    Ok(())
}

// ----------------------------------------------------------------
// Issue #1083: Rollback rehearsal — layout completeness check
// ----------------------------------------------------------------

/// Dry-run validator that checks whether rolling back the WASM to a version
/// that only understands the monolithic `Invoice` storage key would leave any
/// invoice data silently inaccessible.
///
/// ## Background
///
/// The v2 storage layout stores each invoice as two separate persistent
/// entries:
///
/// * `InvoiceCore(id)` — hot-path fields (status, amounts, addresses)
/// * `InvoiceMetadata(id)` — cold-path fields (funder, funded_at,
///   referral_code, submitter_reputation)
///
/// A v1 (pre-split) WASM only reads the monolithic `Invoice(id)` key.
/// If the contract is rolled back to v1, any invoice written exclusively in
/// the split format (no `Invoice(id)` key) would be silently invisible — the
/// old code would report `InvoiceNotFound` for every such ID.
///
/// ## What this function checks
///
/// The most-recent `ROLLBACK_SAMPLE_SIZE` invoice IDs (counting down from
/// `NextInvoiceId - 1`) are inspected.  For each ID the function checks:
///
/// ```text
/// has_legacy   = Invoice(id) key is present
/// has_core     = InvoiceCore(id) key is present
/// has_metadata = InvoiceMetadata(id) key is present
/// ```
///
/// The *rollback-safe invariant* is:
///
/// ```text
/// has_legacy  OR  (has_core AND has_metadata)
/// ```
///
/// An invoice violates the invariant — and is counted as **orphaned** — when:
/// * `has_core` is true but `has_metadata` is false (core without metadata)
/// * `has_metadata` is true but `has_core` is false (metadata without core)
///
/// **Neither** of the split halves alone is sufficient for old WASM to read
/// the invoice; the old code only understands `Invoice(id)`.
///
/// ## Return value
///
/// Returns `Ok(RollbackRehearsalReport)` always — including when the check
/// finds orphaned entries — so the caller can inspect the report and decide
/// whether to proceed.  Returns `Err(RollbackUnsafe)` only if at least one
/// orphaned entry is found, enabling a fail-closed pattern in callers that
/// need to gate a rollback on safety.
///
/// ## Normal operation
///
/// `save_invoice()` always writes **both** split halves atomically, so
/// orphaned entries should never occur under normal contract behaviour.
/// This function is a safety net for detecting storage corruption, partial
/// writes from an interrupted migration, or a deliberate adversarial scenario.
pub fn check_rollback_safety(
    env: &Env,
) -> Result<crate::events::RollbackRehearsalReport, crate::errors::ContractError> {
    use crate::constants::ROLLBACK_SAMPLE_SIZE;
    use crate::events::RollbackRehearsalReport;

    let next_id = read_next_invoice_id(env);

    // No invoices have been written — trivially safe.
    if next_id <= 1 {
        return Ok(RollbackRehearsalReport {
            invoices_checked: 0,
            split_format_count: 0,
            legacy_format_count: 0,
            orphaned_count: 0,
            rollback_safe: true,
        });
    }

    let last_id = next_id - 1;
    let sample_start = last_id.saturating_sub(ROLLBACK_SAMPLE_SIZE.saturating_sub(1));

    let mut invoices_checked: u32 = 0;
    let mut split_format_count: u32 = 0;
    let mut legacy_format_count: u32 = 0;
    let mut orphaned_count: u32 = 0;

    let mut id = last_id;
    loop {
        let has_legacy = env.storage().persistent().has(&DataKey::Invoice(id));
        let has_core = env.storage().persistent().has(&DataKey::InvoiceCore(id));
        let has_metadata = env.storage().persistent().has(&DataKey::InvoiceMetadata(id));

        // Skip IDs that have no storage at all (gaps are possible if IDs were
        // reserved but never committed, though normal operation is gapless).
        if !has_legacy && !has_core && !has_metadata {
            if id == sample_start {
                break;
            }
            id -= 1;
            continue;
        }

        invoices_checked = invoices_checked.saturating_add(1);

        if has_legacy {
            // Old monolithic format — always safe to roll back.
            legacy_format_count = legacy_format_count.saturating_add(1);
        } else if has_core && has_metadata {
            // Both split halves present — safe (v2 format, fully intact).
            split_format_count = split_format_count.saturating_add(1);
        } else {
            // Orphaned: exactly one of the two split halves is present.
            // Rolling back to v1 WASM would make this invoice invisible.
            orphaned_count = orphaned_count.saturating_add(1);
        }

        if id == sample_start {
            break;
        }
        id -= 1;
    }

    let rollback_safe = orphaned_count == 0;

    let report = RollbackRehearsalReport {
        invoices_checked,
        split_format_count,
        legacy_format_count,
        orphaned_count,
        rollback_safe,
    };

    if rollback_safe {
        Ok(report)
    } else {
        Err(crate::errors::ContractError::RollbackUnsafe)
    }
}

// ----------------------------------------------------------------
// Issue #1031: Queue Tie-Score Margin helpers
// ----------------------------------------------------------------

/// Return the current queue tie-score margin (default 0 = disabled).
///
/// When > 0, `join_fund_queue` requires a joining LP to have a score of
/// at least `current_best_score + margin`. This raises the economic barrier
/// to equal-score manipulation from zero additional invoices to `margin`
/// additional funded invoices.
pub fn get_queue_tie_score_margin(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::QueueTieScoreMargin)
        .unwrap_or(0)
}

/// Persist a new queue tie-score margin.  The margin is clamped to [0, 50]
/// so it cannot block legitimate high-reputation LPs from joining (the score
/// range is 50–100, so a margin > 50 would prevent most LPs from ever
/// joining a non-empty queue).
pub fn set_queue_tie_score_margin(env: &Env, margin: u32) {
    // Max useful margin: 50 (half the achievable score range above default).
    let margin = margin.min(50);
    env.storage()
        .instance()
        .set(&DataKey::QueueTieScoreMargin, &margin);
}
