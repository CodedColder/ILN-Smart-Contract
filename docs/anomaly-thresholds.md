# Anomaly Detection Thresholds and Baselines

**Status:** Initial thresholds set from testnet observation; review cadence defined below.

This document defines the concrete, justified thresholds used by the anomaly detection service, insurance pool solvency monitoring, and the shared alert router. Poorly-tuned thresholds cause either alert fatigue (too sensitive) or missed incidents (too loose).

---

## 1. Event Volume Anomaly Detection

Source: `indexer/src/services/anomalyDetectionService.ts`

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `trailingWindowHours` | 24 | Captures a full day of activity patterns including peak/off-peak hours. Shorter windows miss diurnal patterns; longer windows dilute recent signals. |
| `alertThresholdStdDevs` | 2.0 | A 2σ threshold flags events outside ~95% of normal variation. On testnet with low baseline volume, this prevents false positives from single-digit count fluctuations. |
| `minimumSampleSize` | 6 | Requires at least 6 hourly buckets (6 hours) of data before evaluating. Below this, the mean/stddev are too unreliable to be actionable. |

### Monitored event types

| Event Type | Typical testnet volume (per hour) | Spike threshold (2σ) | Drop threshold |
|------------|-----------------------------------|----------------------|----------------|
| InvoiceSubmitted | 2–15 | > 45 | < 1 (if baseline > 0) |
| InvoiceFunded | 1–10 | > 30 | < 1 (if baseline > 0) |
| InvoicePaid | 1–8 | > 25 | < 1 (if baseline > 0) |
| InvoiceDisputed | 0–2 | > 8 | N/A (low baseline) |
| InvoiceCancelled | 0–3 | > 10 | N/A (if baseline > 0) |
| InvoiceExpired | 0–2 | > 8 | N/A (if baseline > 0) |

> **Note:** These are initial estimates from testnet observation. The actual thresholds are computed dynamically from the trailing window data, not hardcoded. The table above provides rough expectations for tuning the `alertThresholdStdDevs` parameter.

---

## 2. Insurance Pool Solvency

Source: `indexer/src/services/solvencyMonitor.ts`

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Alert severity for tripped circuit | `critical` | Solvency breaches directly affect user funds — no claim can be processed while the circuit is open. |
| Alert severity for resumed | `info` | Recovery is positive but not actionable; operators should see it but not be paged. |
| Cooldown between alerts | 5 minutes | Prevents alert storms when solvency oscillates near the threshold. |

---

## 3. Canary Latency Thresholds

Source: `scripts/synthetic-canary.ts`

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `LATENCY_THRESHOLD_MS` | 10,000 (10s) | Stellar transaction confirmation typically takes 5–8s on mainnet. 10s allows headroom for normal variance while catching RPC degradation. |
| `INDEXER_REFLECT_WINDOW_MS` | 60,000 (60s) | The indexer processes events within seconds under normal load, but 60s accounts for ingestion lag, reorg handling, and network partitions. |

---

## 4. Alert Router Cooldowns

Source: `indexer/src/services/alertRouter.ts`

| Category | Cooldown | Rationale |
|----------|----------|-----------|
| `solvency_circuit_tripped` | 5 min | High-severity; must page on-call but not spam. |
| `solvency_circuit_resumed` | 5 min | Info-level; single notification is sufficient. |
| `reorg_detected` | 10 min | Reorgs are rare but may cluster during chain instability. |
| `admin_action_anomaly` | 5 min | Each anomalous admin action is independent and should alert. |
| `oracle_health_degraded` | 15 min | Oracle health may fluctuate; longer cooldown prevents noise. |
| `canary_failure` | 5 min | Canary failures should alert immediately but not spam. |

---

## 5. Review Cadence

| Cadence | Action |
|---------|--------|
| **Weekly (first month)** | Review alert volume and false-positive rate. Adjust `alertThresholdStdDevs` if noise is excessive. |
| **Monthly** | Compare actual event volumes against baselines. Update the expected volume table in §1. |
| **Quarterly** | Re-evaluate all thresholds against production traffic patterns. Document changes via ADR. |
| **Post-incident** | Any threshold that caused a missed incident or false alarm must be reviewed and adjusted in the post-incident report. |

---

## 6. How to Tune

1. **Too many false positives:** Increase `alertThresholdStdDevs` (e.g. 2.0 → 2.5) or increase `minimumSampleSize`.
2. **Missing real incidents:** Decrease `alertThresholdStdDevs` (e.g. 2.0 → 1.5) or shorten `trailingWindowHours`.
3. **Alert storms:** Increase the cooldown in `AlertRouterConfig.cooldownMs` for the specific category.
4. **Canary too sensitive:** Increase `LATENCY_THRESHOLD_MS` if mainnet confirmation times are consistently higher than expected.

All changes must be documented with rationale and committed alongside the code change.
