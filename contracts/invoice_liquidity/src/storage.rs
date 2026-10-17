use soroban_sdk::{contracttype, Address, BytesN, Env, Symbol};

use crate::config::Config;
use crate::invoice::{
    AppealRecord, Invoice, InvoiceCore, InvoiceMetadata, LpFundRequest, ReputationScore,
};
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
    /// Issue #645: monotonically increasing count of admin actions ever
    /// recorded in the admin action audit log (never decreases, even as
    /// older ring-buffer entries are overwritten).
    AdminActionCount,
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
// ----------------------------------------------------------------

/// Extend `key`'s TTL if (and only if) it exists. `extend_ttl` on a missing
/// key traps, so every queue-entry bump goes through this guard.
fn bump_queue_entry(env: &Env, key: &DataKey) {
    if env.storage().persistent().has(key) {
        env.storage().persistent().extend_ttl(
            key,
            crate::constants::QUEUE_TTL_THRESHOLD_LEDGERS,
            crate::constants::QUEUE_TTL_EXTEND_TO_LEDGERS,
        );
    }
}

/// Issues #1009 / #1010: keep every record that belongs to an invoice's fund
/// queue alive together. The queue, its `FundQueueOpenedAt` marker and the
/// `QueueResolution` record are read at different times (join, resolve,
/// and a deferred `fund_invoice`), so they must not age out independently:
/// an expired `QueueResolution` silently re-opens funding to every LP, and an
/// expired queue/marker silently drops the MEV maturity delay.
pub fn extend_fund_queue_ttl(env: &Env, invoice_id: u64) {
    bump_queue_entry(env, &DataKey::FundQueue(invoice_id));
    bump_queue_entry(env, &DataKey::FundQueueOpenedAt(invoice_id));
    bump_queue_entry(env, &DataKey::QueueResolution(invoice_id));
}

pub fn get_fund_queue(env: &Env, invoice_id: u64) -> soroban_sdk::Vec<LpFundRequest> {
    let key = DataKey::FundQueue(invoice_id);
    let queue = env.storage().persistent().get(&key);
    if queue.is_some() {
        extend_fund_queue_ttl(env, invoice_id);
    }
    queue.unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

pub fn save_fund_queue(env: &Env, invoice_id: u64, queue: &soroban_sdk::Vec<LpFundRequest>) {
    env.storage()
        .persistent()
        .set(&DataKey::FundQueue(invoice_id), queue);
    extend_fund_queue_ttl(env, invoice_id);
}

/// Return the resolved (approved) funder for an invoice, if any.
///
/// Reading the record extends it (and its sibling queue entries): the
/// resolution is consulted by `join_fund_queue`, `resolve_fund_queue` and
/// `fund_invoice`, the last of which may run long after resolution.
pub fn get_queue_resolution(env: &Env, invoice_id: u64) -> Option<Address> {
    let resolution = env
        .storage()
        .persistent()
        .get(&DataKey::QueueResolution(invoice_id));
    if resolution.is_some() {
        extend_fund_queue_ttl(env, invoice_id);
    }
    resolution
}

pub fn save_queue_resolution(env: &Env, invoice_id: u64, approved_lp: &Address) {
    env.storage()
        .persistent()
        .set(&DataKey::QueueResolution(invoice_id), approved_lp);
    extend_fund_queue_ttl(env, invoice_id);
}

/// Record the ledger sequence when the first LP joined the fund queue.
/// Must only be called once per invoice (when the queue transitions from empty
/// to non-empty).  Subsequent joins do not overwrite this timestamp.
pub fn try_set_fund_queue_opened_at(env: &Env, invoice_id: u64) {
    let key = DataKey::FundQueueOpenedAt(invoice_id);
    if !env.storage().persistent().has(&key) {
        env.storage()
            .persistent()
            .set(&key, &env.ledger().sequence());
    }
    extend_fund_queue_ttl(env, invoice_id);
}

/// Return the ledger sequence when the fund queue for `invoice_id` was first
/// opened (i.e. the first LP join), or `None` if the queue is still empty.
pub fn get_fund_queue_opened_at(env: &Env, invoice_id: u64) -> Option<u32> {
    let opened_at = env
        .storage()
        .persistent()
        .get(&DataKey::FundQueueOpenedAt(invoice_id));
    if opened_at.is_some() {
        extend_fund_queue_ttl(env, invoice_id);
    }
    opened_at
}

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
