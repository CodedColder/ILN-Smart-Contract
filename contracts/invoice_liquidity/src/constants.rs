pub const MAX_DISCOUNT_RATE: u32 = 5000;
pub const DEFAULT_PAYER_SCORE: u32 = 50;
pub const DEFAULT_LP_SCORE: u32 = 50;
pub const TOP_PAYERS_CAPACITY: u32 = 50;
pub const CONTRACT_VERSION: &str = "1.0.0";
/// Issue #539: Current storage schema version. Bump this whenever the
/// persistent/instance storage layout changes so that `migrate()` can
/// detect and apply incremental upgrades.
pub const CURRENT_STORAGE_VERSION: u32 = 3;

// ----------------------------------------------------------------
// Rate Limiting Defaults (Issue #541)
// ----------------------------------------------------------------

/// Default minimum delay between sensitive admin operations in ledgers.
/// At ~5 seconds per ledger, 120 ledgers ≈ 10 minutes.
pub const DEFAULT_RATE_LIMIT_LEDGERS: u64 = 120;

/// Rate limit cooldown for `set_admin` — 1 hour (720 ledgers at 5s).
pub const ADMIN_CHANGE_COOLDOWN_LEDGERS: u64 = 720;

/// Rate limit cooldown for `upgrade` — 2 hours (1440 ledgers at 5s).
pub const UPGRADE_COOLDOWN_LEDGERS: u64 = 1440;

/// Rate limit cooldown for economic parameters — 30 minutes (360 ledgers).
pub const ECONOMIC_PARAM_COOLDOWN_LEDGERS: u64 = 360;

// ----------------------------------------------------------------
// Reputation Decay Bounds (Issue #601)
// ----------------------------------------------------------------

/// Maximum number of decay periods `get_payer_score` will iterate before
/// short-circuiting the score to zero. See invoice.rs for full rationale.
pub const MAX_REPUTATION_DECAY_PERIODS: u64 = 1000;

/// Minimum number of ledgers that must elapse between the first LP joining the
/// fund queue and `resolve_fund_queue` being callable.  At ~5 s per ledger,
/// 120 ledgers ≈ 10 minutes, giving other LPs a fair window to join.
///
/// Prevents MEV / front-running: an attacker who observes a high-reputation LP
/// joining the queue can no longer immediately resolve the queue in the same
/// block to lock-out competing LPs (Issue #MEV-1).
pub const QUEUE_DELAY_LEDGERS: u32 = 120;

/// Issue #1022: hard cap on LPs per invoice fund queue. Bounds the persistent
/// `FundQueue` entry and the O(n) scans in `join_fund_queue` /
/// `resolve_fund_queue` regardless of how long the queue sits open.
pub const MAX_FUND_QUEUE_LEN: u32 = 50;

/// Issue #1020: ledgers an approved LP has, after `resolve_fund_queue`, to
/// fund before anyone may call `recover_fund_queue` to evict them (~1 hour).
pub const QUEUE_RESOLUTION_TTL_LEDGERS: u32 = 720;

/// Issue #1020: when the invoice is due within this many seconds, the
/// recovery grace period shortens from `QUEUE_RESOLUTION_TTL_LEDGERS` to
/// `QUEUE_DELAY_LEDGERS` so a stalled winner cannot run out the clock.
pub const NEAR_EXPIRY_WINDOW_SECS: u64 = 60 * 60 * 24;
