pub use crate::storage::DataKey as StorageKey;
use soroban_sdk::{contracttype, Address, BytesN, Env, Symbol};
use soroban_sdk::testutils::storage::Persistent as _;

/// A nullable BytesN<32> that works with #[contracttype] derive.
/// In Soroban SDK 21.x, `Option<BytesN<32>>` doesn't implement the required
/// ScVal conversion traits, so we use this wrapper enum instead.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ReferralCode {
    None,
    Present(BytesN<32>),
}

// ----------------------------------------------------------------
// FORMAL VERIFICATION INVARIANTS
//
// == State Machine Invariants ==
// INVARIANT SM1: InvoiceStatus transitions are strictly validated.
//   Valid paths:
//     Pending -> {Funded, PartiallyFunded, Cancelled, Expired, Disputed}
//     PartiallyFunded -> {Funded, Cancelled, Disputed}
//     Funded -> {Paid, Defaulted, Disputed}
//     Defaulted -> {Appealed}
//     Appealed -> {Defaulted} (via resolve_appeal — admin only)
//     Disputed -> {Cancelled, Funded, PartiallyFunded, Pending} (admin resolution)
//   Terminal states: {Paid, Expired, Cancelled}
// INVARIANT SM2: No invalid transition is ever silently ignored — each
//   returns a distinct ContractError variant.
//
// == Balance Invariants ==
// INVARIANT B1: amount_funded <= amount  (enforced by OverfundingRejected)
// INVARIANT B2: amount_paid <= amount    (enforced by OverpaymentRejected)
// INVARIANT B3: amount_funded == amount  iff status == Funded
// INVARIANT B4: amount_paid == amount    iff status == Paid
//
// == Authorization Invariants ==
// INVARIANT A1: cancel_invoice requires caller == invoice.freelancer
// INVARIANT A2: mark_paid requires caller == invoice.payer
// INVARIANT A3: claim_default requires caller in invoice funders list
// INVARIANT A4: Admin-only functions call require_admin() at entry
// INVARIANT A5: dispute_invoice requires caller == invoice.payer
// INVARIANT A6: appeal_default requires caller == invoice.payer
//
// == Storage Invariants ==
// INVARIANT S1: Each invoice occupies an independent DataKey::Invoice(id).
//   Operations on invoice i never read or write invoice j (i != j).
// INVARIANT S2: Submitter invoice index and LP invoice index are eventually
//   consistent — every invoice appears in at least one index.
//
// == Fund-Queue Invariants (Issue #1017) ==
// INVARIANT Q1: resolve_fund_queue only finalizes a *not-yet-resolved*
//   queue while invoice.status is Pending or PartiallyFunded — matching
//   join_fund_queue's own status gate. cancel_invoice / expire_invoice
//   never touch FundQueue / FundQueueOpenedAt / QueueResolution, so without
//   this check a queue left unresolved when the invoice was cancelled or
//   expired would still be resolvable afterward, announcing a reputation-
//   weighted "winner" for funding that can never happen.
// INVARIANT Q2: a queue already resolved (QueueResolution present) before
//   cleanup stays resolved — cancel_invoice / expire_invoice cannot erase a
//   decision that already finalized; resolve_fund_queue's cache check for
//   an existing QueueResolution runs before Q1's status gate.
// ----------------------------------------------------------------

// ----------------------------------------------------------------
// Status enum — tracks lifecycle of invoice
// ----------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum InvoiceStatus {
    Pending,         // submitted, waiting for a liquidity provider to fund it
    Funded,          // LP has funded it, freelancer has been paid out
    PartiallyFunded, // partially funded by one or more LPs
    Paid,            // payer has settled in full, LP has been released
    Defaulted,       // past due_date and still unpaid
    Appealed,        // payer has contested the default ruling (issue #36)
    Disputed,        // payer has disputed the invoice before settlement
    Expired,         // past due_date with no funding
    Cancelled,       // freelancer cancelled the invoice before funding
}

// ----------------------------------------------------------------
// Invoice Core struct — hot path data (accessed in >95% of operations)
// ================================================================
// Fields are ordered by access frequency and size for optimal layout:
// - id, status: identifiers, checked in every operation
// - amount/amount_funded/amount_paid: financial core
// - addresses: payer, freelancer, token
// - due_date, discount_rate: parameters
// ================================================================

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceCore {
    pub id: u64,
    pub freelancer: Address, // who submitted the invoice (receives liquidity)
    pub payer: Address,      // the client who owes the money
    pub token: Address,      // token used for this invoice lifecycle
    pub amount: i128,        // full invoice value in stroops (1 USDC = 10_000_000)
    pub due_date: u32,       // Unix timestamp — when the payer must settle by
    pub discount_rate: u32,  // basis points, e.g. 300 = 3.00%
    pub status: InvoiceStatus,
    pub amount_funded: i128, // cumulative amount funded so far
    pub amount_paid: i128,   // cumulative amount paid by the payer
}

// ----------------------------------------------------------------
// Invoice Metadata struct — cold path data (accessed <5% of operations)
// ================================================================
// Kept separate to avoid deserializing unnecessary data on hot paths.
// Only loaded when needed for appeals, disputes, or metadata queries.
// ================================================================

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceMetadata {
    pub funder: Option<Address>, // set when an LP funds the invoice (legacy for full funding)
    pub funded_at: Option<u32>,  // ledger timestamp when funding occurred
    pub referral_code: ReferralCode,
    pub submitter_reputation: u32, // snapshot of freelancer's reputation at submission time
}

// ================================================================
// Invoice — Unified type for backwards compatibility
// ================================================================
// The Invoice type combines both core and metadata.
// Internal operations use InvoiceCore for hot paths.
// External API continues to use Invoice for compatibility.

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Invoice {
    pub id: u64,
    pub freelancer: Address,
    pub payer: Address,
    pub token: Address,
    pub amount: i128,
    pub due_date: u32,
    pub discount_rate: u32,
    pub status: InvoiceStatus,
    pub funder: Option<Address>,
    pub funded_at: Option<u32>,
    pub amount_funded: i128,
    pub amount_paid: i128,
    pub referral_code: ReferralCode,
    pub submitter_reputation: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct InvoiceParams {
    pub freelancer: Address,
    pub payer: Address,
    pub amount: i128,
    pub due_date: u64,
    pub discount_rate: u32,
    pub token: Address,
    pub referral_code: ReferralCode,
}

#[contracttype]
#[derive(Clone, Debug, Default)]
pub struct PayerStats {
    pub total_invoices: u64,
    pub paid_on_time: u64,
    pub defaults: u64,
    pub total_volume: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ReputationScore {
    pub score: u32,
    pub last_activity_ledger: u32,
}

/// Detailed reputation profile for an address (Issue #26).
///
/// **Protocol source of truth** for ILN reputation counters used by funding
/// gates, defaults, and appeals. This is independent of the
/// `reputation_bonus` crate's own `ReputationScore` storage — the two never
/// sync (see `docs/adr/ADR-011-reputation-state-source-of-truth.md`).
///
/// The lightweight [`ReputationScore`] holds the decaying score used by the
/// payer/LP scoring path; this profile records the richer counters. Unknown
/// addresses resolve to a zeroed profile (see [`get_reputation`]) rather than
/// panicking.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ReputationProfile {
    pub address: Address,
    pub invoices_submitted: u32,
    pub invoices_paid: u32,
    pub invoices_defaulted: u32,
    pub score: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ContractStats {
    pub total_invoices: u64,
    pub total_funded: u64,
    pub total_paid: u64,
    pub total_volume_usdc: i128,
    pub total_volume_eurc: i128,
    pub total_volume_xlm: i128,
    pub token_volumes: soroban_sdk::Vec<(Address, i128)>,
    pub total_volume_usd_normalized: i128,
}

/// Issue #775: operationally-relevant protocol state in a single read, for a
/// public "is the protocol paused, and why" status view (contract-side) and
/// the indexer's public `/protocol-status` endpoint (off-chain).
///
/// This is deliberately distinct from the admin-only dashboard data — every
/// field here is safe to expose publicly and is what a user or an on-call
/// responder needs during an incident.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProtocolStatus {
    /// Whether all mutating entry points are currently halted (`pause()`).
    pub paused: bool,
    /// Ledger timestamp of the most recent `pause()`; `0` if never paused.
    pub last_pause_timestamp: u64,
    /// The address `require_admin` authorizes against today (may itself be a
    /// Stellar account-level multisig — the contract cannot tell).
    pub admin: Address,
    /// Whether the contract-level multisig admin has been bootstrapped
    /// (`initialize_multisig_admin`).
    pub multisig_configured: bool,
    /// Signatures required to execute a multisig admin action; `0` when the
    /// multisig is not configured.
    pub multisig_threshold: u32,
    /// Number of authorized multisig signers; `0` when not configured.
    pub multisig_signer_count: u32,
    /// Whether at least one oracle circuit breaker (feed type + token) is
    /// currently tripped, halting oracle-gated funding on that channel.
    pub oracle_circuit_tripped: bool,
    /// How many (feed type, token) oracle circuit breakers are tripped.
    pub oracle_circuits_tripped: u32,
}

// ----------------------------------------------------------------
// Issue #36: Appeal record stored per invoice
// ----------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AppealRecord {
    /// SHA-256 hash of off-chain evidence submitted by the payer.
    pub evidence_hash: BytesN<32>,
    /// Ledger timestamp when the appeal was filed.
    pub appealed_at: u32,
    /// Payer reputation score just before the default was applied,
    /// used to restore the score if the appeal is upheld.
    pub pre_default_score: u32,
}

// ----------------------------------------------------------------
// Dispute record stored per invoice
// ----------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DisputeRecord {
    /// SHA-256 hash of off-chain dispute evidence.
    pub reason_hash: BytesN<32>,
    /// Ledger sequence when the dispute was filed.
    pub disputed_at: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TopPayerEntry {
    pub address: Address,
    pub score: u32,
}

// ----------------------------------------------------------------
// Issue #34: Single entry in the LP priority queue
// ----------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug)]
pub struct LpFundRequest {
    pub lp: Address,
    /// LP reputation score snapshotted at request time (used for ordering).
    pub score: u32,
}

// ================================================================
// Invoice conversion functions — convert between unified and split formats
// ================================================================

impl Invoice {
    /// Extract the hot-path core data into InvoiceCore
    pub fn to_core(&self) -> InvoiceCore {
        InvoiceCore {
            id: self.id,
            freelancer: self.freelancer.clone(),
            payer: self.payer.clone(),
            token: self.token.clone(),
            amount: self.amount,
            due_date: self.due_date,
            discount_rate: self.discount_rate,
            status: self.status.clone(),
            amount_funded: self.amount_funded,
            amount_paid: self.amount_paid,
        }
    }

    /// Extract the cold-path metadata into InvoiceMetadata
    pub fn to_metadata(&self) -> InvoiceMetadata {
        InvoiceMetadata {
            funder: self.funder.clone(),
            funded_at: self.funded_at,
            referral_code: self.referral_code.clone(),
            submitter_reputation: self.submitter_reputation,
        }
    }
}

impl InvoiceCore {
    /// Combine core data with metadata to reconstruct full Invoice
    pub fn with_metadata(self, metadata: InvoiceMetadata) -> Invoice {
        Invoice {
            id: self.id,
            freelancer: self.freelancer,
            payer: self.payer,
            token: self.token,
            amount: self.amount,
            due_date: self.due_date,
            discount_rate: self.discount_rate,
            status: self.status,
            funder: metadata.funder,
            funded_at: metadata.funded_at,
            amount_funded: self.amount_funded,
            amount_paid: self.amount_paid,
            referral_code: metadata.referral_code,
            submitter_reputation: metadata.submitter_reputation,
        }
    }
}

// ----------------------------------------------------------------
// Storage helpers — core invoice CRUD
// ----------------------------------------------------------------

pub fn get_submitter_invoices(env: &Env, submitter: &Address) -> soroban_sdk::Vec<u64> {
    env.storage()
        .persistent()
        .get(&StorageKey::SubmitterInvoices(submitter.clone()))
        .unwrap_or(soroban_sdk::Vec::new(env))
}

pub fn add_invoice_to_submitter(env: &Env, submitter: &Address, invoice_id: u64) {
    let mut invoices = get_submitter_invoices(env, submitter);
    invoices.push_back(invoice_id);
    let key = StorageKey::SubmitterInvoices(submitter.clone());
    env.storage().persistent().set(&key, &invoices);
    env.storage()
        .persistent()
        .extend_ttl(&key, 1_000_000, 2_000_000);
}

pub fn remove_invoice_from_submitter(env: &Env, submitter: &Address, invoice_id: u64) {
    let invoices = get_submitter_invoices(env, submitter);
    let mut new_invoices = soroban_sdk::Vec::new(env);
    for id in invoices.iter() {
        if id != invoice_id {
            new_invoices.push_back(id);
        }
    }
    let key = StorageKey::SubmitterInvoices(submitter.clone());
    if new_invoices.is_empty() {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    } else {
        env.storage().persistent().set(&key, &new_invoices);
        env.storage()
            .persistent()
            .extend_ttl(&key, 1_000_000, 2_000_000);
    }
}

pub fn get_lp_invoices(env: &Env, lp: &Address) -> soroban_sdk::Vec<u64> {
    env.storage()
        .persistent()
        .get(&StorageKey::LpInvoices(lp.clone()))
        .unwrap_or(soroban_sdk::Vec::new(env))
}

pub fn add_invoice_to_lp(env: &Env, lp: &Address, invoice_id: u64) {
    let mut invoices = get_lp_invoices(env, lp);
    // Check if already present to avoid duplicates in case of partial funding
    let mut exists = false;
    for id in invoices.iter() {
        if id == invoice_id {
            exists = true;
            break;
        }
    }
    if !exists {
        invoices.push_back(invoice_id);
        let key = StorageKey::LpInvoices(lp.clone());
        env.storage().persistent().set(&key, &invoices);
        env.storage()
            .persistent()
            .extend_ttl(&key, 1_000_000, 2_000_000);
    }
}

pub fn remove_invoice_from_lp(env: &Env, lp: &Address, invoice_id: u64) {
    let invoices = get_lp_invoices(env, lp);
    let mut new_invoices = soroban_sdk::Vec::new(env);
    for id in invoices.iter() {
        if id != invoice_id {
            new_invoices.push_back(id);
        }
    }
    let key = StorageKey::LpInvoices(lp.clone());
    if new_invoices.is_empty() {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    } else {
        env.storage().persistent().set(&key, &new_invoices);
        env.storage()
            .persistent()
            .extend_ttl(&key, 1_000_000, 2_000_000);
    }
}

pub fn save_invoice(env: &Env, invoice: &Invoice) {
    let key = StorageKey::Invoice(invoice.id);
    env.storage().persistent().set(&key, invoice);
    env.storage()
        .persistent()
        .extend_ttl(&key, 1_000_000, 2_000_000);
}

pub fn load_invoice(env: &Env, id: u64) -> Invoice {
    env.storage()
        .persistent()
        .get(&StorageKey::Invoice(id))
        .expect("invoice not found")
}

pub fn invoice_exists(env: &Env, id: u64) -> bool {
    env.storage().persistent().has(&StorageKey::Invoice(id))
}

/// Load an invoice in a single storage read, returning `None` if it does not
/// exist (Issue #71). Prefer this over the `invoice_exists` + `load_invoice`
/// pair in hot paths, which reads the same key twice.
/// Try to load an invoice, returning None if not found.
///
/// This function loads invoices using the optimized split format:
/// - First tries the new split format (InvoiceCore + InvoiceMetadata)
/// - Falls back to old format for backwards compatibility
pub fn try_load_invoice(env: &Env, id: u64) -> Option<Invoice> {
    // Try new split format first (preferred)
    if let Some(core) = env
        .storage()
        .persistent()
        .get::<StorageKey, InvoiceCore>(&StorageKey::InvoiceCore(id))
    {
        if let Some(metadata) = env
            .storage()
            .persistent()
            .get::<StorageKey, InvoiceMetadata>(&StorageKey::InvoiceMetadata(id))
        {
            return Some(core.with_metadata(metadata));
        }
    }

    // Fall back to old format for backwards compatibility
    env.storage().persistent().get(&StorageKey::Invoice(id))
}

pub fn read_next_invoice_id(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&StorageKey::NextInvoiceId)
        .unwrap_or(1)
}

pub fn write_next_invoice_id(env: &Env, id: u64) {
    env.storage()
        .instance()
        .set(&StorageKey::NextInvoiceId, &id);
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
// Reputation Score
// ----------------------------------------------------------------

/// Get a payer's reputation score (0-100, default 50)
pub fn get_payer_score(env: &Env, payer: &Address) -> u32 {
    match env
        .storage()
        .persistent()
        .get::<StorageKey, ReputationScore>(&StorageKey::PayerScore(payer.clone()))
    {
        Some(mut rep) => {
            // Apply decay if enough ledgers have passed and config exists
            if let Some(decay_config) = crate::storage::get_config(env) {
                let current_ledger = env.ledger().sequence();
                let ledgers_since_activity =
                    current_ledger.saturating_sub(rep.last_activity_ledger);

                if u64::from(ledgers_since_activity) >= decay_config.decay_period_ledgers
                    && decay_config.decay_period_ledgers > 0
                    && decay_config.decay_rate_bps > 0
                {
                    // Calculate number of decay periods that have passed
                    let periods_passed = u64::from(ledgers_since_activity)
                        .saturating_div(decay_config.decay_period_ledgers);

                    // Apply decay: score = score * (1 - decay_rate/10000)^periods
                    // Issue #601: periods_passed is unbounded (governance-
                    // configurable decay_period_ledgers can be set to 1),
                    // so cap iteration and short-circuit to 0 beyond that.
                    let decayed_score: u64 =
                        if periods_passed > crate::constants::MAX_REPUTATION_DECAY_PERIODS {
                            0
                        } else {
                            let mut decayed_score = rep.score as u64;
                            for _ in 0..periods_passed {
                                let mut decay_amount = decayed_score
                                    .saturating_mul(decay_config.decay_rate_bps as u64)
                                    .saturating_div(10_000);
                                if decay_amount == 0 && decayed_score > 0 {
                                    decay_amount = 1;
                                }
                                decayed_score = decayed_score.saturating_sub(decay_amount);
                            }
                            decayed_score
                        };

                    let new_score = (decayed_score.min(100)) as u32;
                    if new_score != rep.score {
                        rep.score = new_score;
                        rep.last_activity_ledger = current_ledger;
                        env.storage()
                            .persistent()
                            .set(&StorageKey::PayerScore(payer.clone()), &rep);

                        // Sync with ReputationProfile and trigger event
                        let mut profile = get_reputation(env, payer);
                        profile.score = new_score;
                        set_reputation(env, &profile);
                    }
                }
            }

            rep.score
        }
        None => crate::constants::DEFAULT_PAYER_SCORE,
    }
}

fn payer_score_key(payer: &Address) -> StorageKey {
    StorageKey::PayerScore(payer.clone())
}

/// Update a payer's reputation score (capped at 100).
/// Uses lazy initialisation: default scores are not persisted until changed.
pub fn set_payer_score(env: &Env, payer: &Address, score: u32) {
    let score = score.min(100);
    let key = payer_score_key(payer);
    let old_score = get_payer_score(env, payer);

    if score == crate::constants::DEFAULT_PAYER_SCORE {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    } else {
        let rep = ReputationScore {
            score,
            last_activity_ledger: env.ledger().sequence(),
        };
        env.storage().persistent().set(&key, &rep);
    }

    // Sync with ReputationProfile so they are completely aligned
    let mut profile = get_reputation(env, payer);
    profile.score = score;
    set_reputation(env, &profile);

    if old_score != score {
        crate::top_payers::update_top_payers_on_score_change(env, payer, score);
    }
}

// ----------------------------------------------------------------
// Issue #26: Reputation profile (detailed model)
// ----------------------------------------------------------------

/// Read an address's detailed reputation profile. Unknown addresses return a
/// zeroed profile (no panic) so callers can branch on the counters directly.
pub fn get_reputation(env: &Env, address: &Address) -> ReputationProfile {
    env.storage()
        .persistent()
        .get(&StorageKey::Reputation(address.clone()))
        .unwrap_or(ReputationProfile {
            address: address.clone(),
            invoices_submitted: 0,
            invoices_paid: 0,
            invoices_defaulted: 0,
            score: 0,
        })
}

/// Persist an address's reputation profile.
/// Uses lazy initialisation: zero-value profiles are not stored.
pub fn set_reputation(env: &Env, profile: &ReputationProfile) {
    let key = StorageKey::Reputation(profile.address.clone());
    let old_profile = get_reputation(env, &profile.address);
    let old_score = old_profile.score;
    let new_score = profile.score;

    let is_empty = profile.invoices_submitted == 0
        && profile.invoices_paid == 0
        && profile.invoices_defaulted == 0
        && profile.score == 0;

    if is_empty {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    } else {
        env.storage().persistent().set(&key, profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, 1_000_000, 2_000_000);
    }

    if old_score != new_score
        || old_profile.invoices_submitted != profile.invoices_submitted
        || old_profile.invoices_paid != profile.invoices_paid
        || old_profile.invoices_defaulted != profile.invoices_defaulted
    {
        env.events().publish(
            (
                Symbol::new(env, "reputation_updated"),
                profile.address.clone(),
            ),
            crate::events::ReputationUpdated {
                address: profile.address.clone(),
                old_score,
                new_score,
                invoices_submitted: profile.invoices_submitted,
                invoices_paid: profile.invoices_paid,
                invoices_defaulted: profile.invoices_defaulted,
            },
        );
    }
}

pub fn increment_invoices_submitted(env: &Env, address: &Address) {
    let mut profile = get_reputation(env, address);
    profile.invoices_submitted = profile.invoices_submitted.saturating_add(1);
    set_reputation(env, &profile);
}

pub fn increment_invoices_paid(env: &Env, address: &Address) {
    let mut profile = get_reputation(env, address);
    profile.invoices_paid = profile.invoices_paid.saturating_add(1);
    set_reputation(env, &profile);
}

pub fn increment_invoices_defaulted(env: &Env, address: &Address) {
    let mut profile = get_reputation(env, address);
    profile.invoices_defaulted = profile.invoices_defaulted.saturating_add(1);
    set_reputation(env, &profile);
}

// ----------------------------------------------------------------
// Issue #28: Minimum payer reputation threshold
// ----------------------------------------------------------------

/// Minimum payer reputation required to fund an invoice. Defaults to 0
/// (allowing all payers) when unset.
pub fn get_min_payer_reputation(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&StorageKey::MinPayerReputation)
        .unwrap_or(0)
}

/// Set the minimum payer reputation threshold.
pub fn set_min_payer_reputation(env: &Env, value: u32) {
    env.storage()
        .instance()
        .set(&StorageKey::MinPayerReputation, &value);
}

// ----------------------------------------------------------------
// Issue #655: staged mainnet rollout caps
// ----------------------------------------------------------------
//
// Two governance-configurable caps, both `0` (uncapped) by default, that
// can be raised over time as confidence in a fresh mainnet deployment
// grows: a per-invoice size cap, and a per-token cumulative funded-volume
// cap checked against the same `TokenVolume` counter `add_volume` already
// maintains for stats — no new hot-path dependency (e.g. an oracle call)
// is introduced to enforce it.

/// Maximum `amount` allowed for a single invoice. Defaults to 0 (uncapped).
pub fn get_max_invoice_amount(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&StorageKey::MaxInvoiceAmount)
        .unwrap_or(0)
}

/// Set the maximum single-invoice amount.
pub fn set_max_invoice_amount(env: &Env, value: i128) {
    env.storage()
        .instance()
        .set(&StorageKey::MaxInvoiceAmount, &value);
}

/// Cumulative funded-volume cap for `token`. Defaults to 0 (uncapped).
pub fn get_token_volume_cap(env: &Env, token: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&StorageKey::TokenVolumeCap(token.clone()))
        .unwrap_or(0)
}

/// Set the cumulative funded-volume cap for `token`.
pub fn set_token_volume_cap(env: &Env, token: &Address, value: i128) {
    env.storage()
        .persistent()
        .set(&StorageKey::TokenVolumeCap(token.clone()), &value);
}

/// Current cumulative funded volume recorded for `token` (the same counter
/// `add_volume` increments on every `fund_invoice` call).
pub fn get_token_volume(env: &Env, token: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&StorageKey::TokenVolume(token.clone()))
        .unwrap_or(0)
}

// ----------------------------------------------------------------
// Funder list helpers
// ----------------------------------------------------------------

/// Get the list of funders and their contributions for an invoice
pub fn get_invoice_funders(env: &Env, id: u64) -> soroban_sdk::Vec<(Address, i128)> {
    env.storage()
        .persistent()
        .get(&StorageKey::InvoiceFunders(id))
        .unwrap_or(soroban_sdk::Vec::new(env))
}

/// Save the list of funders for an invoice.
/// Uses lazy initialisation: empty lists are not stored.
pub fn save_invoice_funders(env: &Env, id: u64, funders: &soroban_sdk::Vec<(Address, i128)>) {
    let key = StorageKey::InvoiceFunders(id);
    if funders.is_empty() {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    } else {
        env.storage().persistent().set(&key, funders);
    }
}

// ----------------------------------------------------------------
// Issue #36: Appeal helpers
// ----------------------------------------------------------------

pub fn get_appeal(env: &Env, invoice_id: u64) -> Option<AppealRecord> {
    env.storage()
        .persistent()
        .get(&StorageKey::Appeal(invoice_id))
}

pub fn save_appeal(env: &Env, invoice_id: u64, record: &AppealRecord) {
    env.storage()
        .persistent()
        .set(&StorageKey::Appeal(invoice_id), record);
}

/// Store the payer's score BEFORE the default penalty is applied.
/// Called inside claim_default() so appeal_default() can restore it later.
pub fn save_pre_default_payer_score(env: &Env, invoice_id: u64, score: u32) {
    env.storage()
        .persistent()
        .set(&StorageKey::PreDefaultPayerScore(invoice_id), &score);
}

pub fn get_pre_default_payer_score(env: &Env, invoice_id: u64) -> Option<u32> {
    env.storage()
        .persistent()
        .get(&StorageKey::PreDefaultPayerScore(invoice_id))
}

// Dispute helpers

pub fn get_dispute(env: &Env, invoice_id: u64) -> Option<DisputeRecord> {
    env.storage()
        .persistent()
        .get(&StorageKey::Dispute(invoice_id))
}

pub fn save_dispute(env: &Env, invoice_id: u64, record: &DisputeRecord) {
    env.storage()
        .persistent()
        .set(&StorageKey::Dispute(invoice_id), record);
}

// ----------------------------------------------------------------
// Issue #34: LP score + queue helpers
// ----------------------------------------------------------------

/// LP reputation score starts at 50 (same neutral baseline as payers).
/// Uses lazy initialisation: default scores are not persisted until changed.
pub fn get_lp_score(env: &Env, lp: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&StorageKey::LpScore(lp.clone()))
        .unwrap_or(crate::constants::DEFAULT_LP_SCORE)
}

/// Update an LP's reputation score (capped at 100).
/// Uses lazy initialisation: default scores are not persisted until changed.
pub fn set_lp_score(env: &Env, lp: &Address, score: u32) {
    let score = score.min(100);
    let key = StorageKey::LpScore(lp.clone());

    if score == crate::constants::DEFAULT_LP_SCORE {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    } else {
        env.storage().persistent().set(&key, &score);
    }
}

/// Return all queued LP requests for an invoice.
///
/// Issues #1009 / #1010: the queue helpers live in `storage.rs`, where reads
/// and writes extend the TTL of the queue, its opened-at marker and the
/// resolution record together. These wrappers delegate so there is a single
/// implementation.
pub fn get_fund_queue(env: &Env, invoice_id: u64) -> soroban_sdk::Vec<LpFundRequest> {
    crate::storage::get_fund_queue(env, invoice_id)
}

/// Persist the queue and extend its TTL (Issue #1025, #1091)
pub fn save_fund_queue(env: &Env, invoice_id: u64, queue: &soroban_sdk::Vec<LpFundRequest>) {
    crate::storage::save_fund_queue(env, invoice_id, queue)
}

/// Return the resolved (approved) funder for an invoice, if any
pub fn get_queue_resolution(env: &Env, invoice_id: u64) -> Option<Address> {
    crate::storage::get_queue_resolution(env, invoice_id)
}

/// Store the approved funder chosen by the priority queue and extend its TTL (Issue #1025, #1091)
pub fn save_queue_resolution(env: &Env, invoice_id: u64, approved_lp: &Address) {
    env.storage()
        .persistent()
        .set(&StorageKey::QueueResolution(invoice_id), approved_lp);
    env.storage()
        .persistent()
        .set(&StorageKey::QueueResolvedAt(invoice_id), &env.ledger().sequence());
}

/// Ledger sequence at which the queue winner was locked in, if resolved.
pub fn get_queue_resolved_at(env: &Env, invoice_id: u64) -> Option<u32> {
    env.storage()
        .persistent()
        .get(&StorageKey::QueueResolvedAt(invoice_id))
}

/// Remove every queue-related entry for `invoice_id`. Idempotent.
pub fn clear_fund_queue_state(env: &Env, invoice_id: u64) {
    let p = env.storage().persistent();
    p.remove(&StorageKey::FundQueue(invoice_id));
    p.remove(&StorageKey::QueueResolution(invoice_id));
    p.remove(&StorageKey::QueueResolvedAt(invoice_id));
    p.remove(&StorageKey::FundQueueOpenedAt(invoice_id));
}

/// Reopen the queue after evicting a stalled winner: drop the resolution and
/// restart the maturity window for the remaining LPs (or clear it if empty).
pub fn reopen_fund_queue(env: &Env, invoice_id: u64, remaining: &soroban_sdk::Vec<LpFundRequest>) {
    let p = env.storage().persistent();
    p.remove(&StorageKey::QueueResolution(invoice_id));
    p.remove(&StorageKey::QueueResolvedAt(invoice_id));
    if remaining.is_empty() {
        p.remove(&StorageKey::FundQueue(invoice_id));
        p.remove(&StorageKey::FundQueueOpenedAt(invoice_id));
    } else {
        p.set(&StorageKey::FundQueue(invoice_id), remaining);
        p.set(&StorageKey::FundQueueOpenedAt(invoice_id), &env.ledger().sequence());
    }
}

/// Record the ledger sequence when the first LP joined the fund queue.
/// Called once when the queue transitions from empty to non-empty.
/// Subsequent joins do not overwrite this value. Extends TTL (Issue #1025, #1091).
pub fn try_set_fund_queue_opened_at(env: &Env, invoice_id: u64) {
    crate::storage::try_set_fund_queue_opened_at(env, invoice_id)
}

/// Return the ledger sequence when the fund queue for `invoice_id` was first
/// opened (i.e. when the first LP joined), or `None` if the queue is still
/// empty.
pub fn get_fund_queue_opened_at(env: &Env, invoice_id: u64) -> Option<u32> {
    crate::storage::get_fund_queue_opened_at(env, invoice_id)
}

/// Issue #1015: per-key TTL headroom for the fund-queue records that gate
/// `resolve_fund_queue`'s MEV-mitigation delay (Issue #MEV-1), the
/// fail-closed archived-state check (Issue #1013), and the pre-upgrade TTL
/// guard (Issue #1082, `TTL_MIN_LEDGERS_BEFORE_UPGRADE`).
///
/// `margin` is `remaining_ttl - TTL_MIN_LEDGERS_BEFORE_UPGRADE`: positive
/// means the key has at least that much headroom above the threshold the
/// upgrade guard enforces; zero or negative means it has already drifted
/// into the zone an upgrade would reject (and, for `FundQueueOpenedAt`
/// specifically, the zone where an untouched key risks the archived-state
/// failure). `None` fields mean the corresponding key does not currently
/// exist (never written, or already evicted).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FundQueueTtlMargins {
    pub fund_queue_ttl: Option<u32>,
    pub fund_queue_margin: Option<i64>,
    pub fund_queue_opened_at_ttl: Option<u32>,
    pub fund_queue_opened_at_margin: Option<i64>,
    pub queue_resolution_ttl: Option<u32>,
    pub queue_resolution_margin: Option<i64>,
}

/// Issue #1015: measure the live TTL (and margin above
/// `TTL_MIN_LEDGERS_BEFORE_UPGRADE`) for each fund-queue-related persistent
/// key of `invoice_id`, so operators can monitor queue health and catch TTL
/// drift before it causes an upgrade rejection (Issue #1082) or a
/// `QueueStateArchived` failure (Issue #1013), rather than finding out only
/// when one of those checks actually fires.
pub fn get_fund_queue_ttl_margins(env: &Env, invoice_id: u64) -> FundQueueTtlMargins {
    let threshold = crate::constants::TTL_MIN_LEDGERS_BEFORE_UPGRADE as i64;

    let fq_key = StorageKey::FundQueue(invoice_id);
    let (fund_queue_ttl, fund_queue_margin) = if env.storage().persistent().has(&fq_key) {
        let ttl = env.storage().persistent().get_ttl(&fq_key);
        (Some(ttl), Some(ttl as i64 - threshold))
    } else {
        (None, None)
    };

    let fqoa_key = StorageKey::FundQueueOpenedAt(invoice_id);
    let (fund_queue_opened_at_ttl, fund_queue_opened_at_margin) =
        if env.storage().persistent().has(&fqoa_key) {
            let ttl = env.storage().persistent().get_ttl(&fqoa_key);
            (Some(ttl), Some(ttl as i64 - threshold))
        } else {
            (None, None)
        };

    let qr_key = StorageKey::QueueResolution(invoice_id);
    let (queue_resolution_ttl, queue_resolution_margin) =
        if env.storage().persistent().has(&qr_key) {
            let ttl = env.storage().persistent().get_ttl(&qr_key);
            (Some(ttl), Some(ttl as i64 - threshold))
        } else {
            (None, None)
        };

    FundQueueTtlMargins {
        fund_queue_ttl,
        fund_queue_margin,
        fund_queue_opened_at_ttl,
        fund_queue_opened_at_margin,
        queue_resolution_ttl,
        queue_resolution_margin,
    }
}
// Contract stats helpers
// ----------------------------------------------------------------

pub fn get_contract_stats(env: &Env) -> ContractStats {
    let token_list: soroban_sdk::Vec<Address> = env
        .storage()
        .persistent()
        .get(&StorageKey::TokenList)
        .unwrap_or(soroban_sdk::Vec::new(env));

    let mut token_volumes = soroban_sdk::Vec::new(env);
    let mut total_volume_usd_normalized: i128 = 0;

    for token in token_list.iter() {
        let volume: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TokenVolume(token.clone()))
            .unwrap_or(0);
        token_volumes.push_back((token.clone(), volume));
        if let Some(price_bps) = get_price_from_oracle(env, &token) {
            total_volume_usd_normalized = total_volume_usd_normalized
                .checked_add(volume.checked_mul(price_bps).unwrap_or(0) / 10_000)
                .unwrap_or(total_volume_usd_normalized);
        }
    }

    ContractStats {
        total_invoices: env
            .storage()
            .persistent()
            .get(&StorageKey::TotalInvoices)
            .unwrap_or(0),
        total_funded: env
            .storage()
            .persistent()
            .get(&StorageKey::TotalFunded)
            .unwrap_or(0),
        total_paid: env
            .storage()
            .persistent()
            .get(&StorageKey::TotalPaid)
            .unwrap_or(0),
        total_volume_usdc: env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVolumeUsdc)
            .unwrap_or(0),
        total_volume_eurc: env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVolumeEurc)
            .unwrap_or(0),
        total_volume_xlm: env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVolumeXlm)
            .unwrap_or(0),
        token_volumes,
        total_volume_usd_normalized,
    }
}

/// Price used to USD-normalize `token`'s volume in `get_contract_stats`.
///
/// Issue #860: this used to bypass the oracle registry entirely — a bare
/// (panicking) `invoke_contract` against the legacy `Config.price_oracle`,
/// with no circuit-breaker or health check, so stats would happily
/// normalize with data the registry had already observed as tripped/stale
/// (or trap the whole read if the oracle didn't respond). It now resolves
/// through the registry (per-token/feed `Price` entry first, legacy
/// `price_oracle` as fallback) and consults the shared health gate before
/// trusting anything:
///
/// - circuit tripped for `(Price, token)` → `None`
/// - last health snapshot stale → `None`
/// - oracle query fails / returns garbage → `None` (non-panicking
///   `try_invoke_contract` via `oracle_registry::query_price`)
///
/// `None` makes `get_contract_stats` skip that token's contribution instead
/// of trusting a bad price or aborting the read.
fn get_price_from_oracle(env: &Env, token: &Address) -> Option<i128> {
    use crate::oracle_registry::{
        query_price, require_healthy_price_feed, resolve_oracle, OracleFeedType,
    };

    let feed = OracleFeedType::Price;
    require_healthy_price_feed(env, feed, token).ok()?;
    let oracle = resolve_oracle(env, feed, token)
        .or_else(|| crate::storage::get_config(env).and_then(|config| config.price_oracle))?;
    query_price(env, &oracle, token)
}

pub fn add_volume(env: &Env, token: &Address, amount: i128) {
    // Track per-token volume in a mutable map.
    let current_per_token: i128 = env
        .storage()
        .persistent()
        .get(&StorageKey::TokenVolume(token.clone()))
        .unwrap_or(0);
    env.storage().persistent().set(
        &StorageKey::TokenVolume(token.clone()),
        &current_per_token.saturating_add(amount),
    );

    // Preserve legacy aggregate token counters for compatibility. Match by
    // the token's actual configured SAC address — never by TokenList
    // position (Issue #620: a hardcoded index silently misattributes volume
    // whenever the list is reordered or a token is removed) — and increment
    // at most one counter per call (the previous code re-checked XLM a
    // second time after the first check's early return, which could
    // double-count it if the two checks ever disagreed).
    if crate::is_xlm_token(env, token) {
        let current: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVolumeXlm)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&StorageKey::TotalVolumeXlm, &current.saturating_add(amount));
    } else if crate::is_usdc_token(env, token) {
        let current: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVolumeUsdc)
            .unwrap_or(0);
        env.storage().persistent().set(
            &StorageKey::TotalVolumeUsdc,
            &current.saturating_add(amount),
        );
    } else if crate::is_eurc_token(env, token) {
        let current: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVolumeEurc)
            .unwrap_or(0);
        env.storage().persistent().set(
            &StorageKey::TotalVolumeEurc,
            &current.saturating_add(amount),
        );
    }
}

pub fn increment_total_invoices(env: &Env) {
    let current: u64 = env
        .storage()
        .persistent()
        .get(&StorageKey::TotalInvoices)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&StorageKey::TotalInvoices, &current.saturating_add(1));
}

pub fn increment_total_funded(env: &Env) {
    let current: u64 = env
        .storage()
        .persistent()
        .get(&StorageKey::TotalFunded)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&StorageKey::TotalFunded, &current.saturating_add(1));
}

pub fn increment_total_paid(env: &Env) {
    let current: u64 = env
        .storage()
        .persistent()
        .get(&StorageKey::TotalPaid)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&StorageKey::TotalPaid, &current.saturating_add(1));
}

// (add_volume is implemented earlier using the configured token addresses)
pub fn is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&StorageKey::Paused)
        .unwrap_or(false)
}

pub fn set_paused(env: &Env, paused: bool) {
    env.storage().instance().set(&StorageKey::Paused, &paused);
    // Issue #775: record when the protocol was last halted so the public
    // status view can answer "paused since when". Only advanced on a
    // pause; `unpause()` leaves the last value in place as a reference.
    if paused {
        env.storage()
            .instance()
            .set(&StorageKey::LastPauseTimestamp, &env.ledger().timestamp());
    }
}

/// Ledger timestamp of the most recent `pause()`, or `0` if the contract
/// has never been paused (Issue #775).
pub fn get_last_pause_timestamp(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&StorageKey::LastPauseTimestamp)
        .unwrap_or(0)
}

// ----------------------------------------------------------------
// Issue #1031: Queue Tie-Score Margin helpers
// ----------------------------------------------------------------

/// Return the current queue tie-score margin (default 0 = disabled).
///
/// When non-zero, `join_fund_queue` requires the joining LP to have a score
/// of at least `current_best_score + margin`, rather than simply ≥ best_score.
/// This raises the economic barrier to equal-score manipulation: an attacker
/// at the default score cannot silently tie with an established LP without
/// first building `margin` additional funded-invoice history.
///
/// Score range: 50 (default) to 100 (max). A margin > 50 is clamped to 50 by
/// `set_queue_tie_score_margin` to avoid blocking legitimate high-rep LPs.
pub fn get_queue_tie_score_margin(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&StorageKey::QueueTieScoreMargin)
        .unwrap_or(0)
}

/// Persist a new queue tie-score margin. Clamped to [0, 50].
pub fn set_queue_tie_score_margin(env: &Env, margin: u32) {
    let margin = margin.min(50);
    env.storage()
        .instance()
        .set(&StorageKey::QueueTieScoreMargin, &margin);
}
