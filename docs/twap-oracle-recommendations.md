# TWAP and Manipulation-Resistant Oracle Recommendations

**Date:** August 27, 2026  
**Branch:** docs/sandwich-attack-oracle-audit  
**Related Documents:** [oracle-sandwich-audit-findings.md](./oracle-sandwich-audit-findings.md), [threat-model.md](./threat-model.md#d3-price-oracle-sandwich-attacks-issue-39)

## Executive Summary

Based on the oracle sandwich attack audit, this document provides concrete recommendations for implementing Time-Weighted Average Price (TWAP) and other manipulation-resistant mechanisms for price oracles in the ILN ecosystem.

## Current State Analysis

### 1. Price Oracle Usage in ILN
- **Function:** USD volume normalization in `get_contract_stats()`
- **Location:** `get_price_from_oracle()` in `invoice.rs`
- **Current Implementation:** Mock/test oracles only (`MockPriceOracle`)
- **Risk Level:** MEDIUM-HIGH if using on-chain DEX spot prices

### 2. Oracle Registry Architecture
- **Feed Types:** `Price`, `Identity`, `Credit`
- **Governance Control:** `register_oracle()` admin-only function
- **Resolution Priority:** Per-token override > Feed-type default > Legacy config

## Recommendations

### 1. Governance Policy Recommendations

#### 1.1 Mandatory Requirements for Price Oracle Proposals
```markdown
**Governance Policy Resolution:**
1. Any `OracleFeedType::Price` proposal MUST demonstrate manipulation resistance
2. DEX-based price oracles MUST implement TWAP with minimum 30-minute window
3. Spot price oracles (single-block) are PROHIBITED for financial operations
4. Multi-source aggregation REQUIRED for high-value integrations
```

#### 1.2 Oracle Provider Approval Checklist Addition
Add to `oracle-provider-vetting.md` checklist:
- [ ] **TWAP Implementation:** Minimum 30-minute window for DEX-based prices
- [ ] **Data Source Diversity:** ≥3 independent price sources
- [ ] **Manipulation Detection:** Automated monitoring and alerting
- [ ] **Historical Consistency:** Protection against flash loan attacks

### 2. Technical Implementation Recommendations

#### 2.1 TWAP Oracle Interface Standard
Define a standard TWAP interface for Stellar/Soroban oracle contracts:

```rust
// Recommended TWAP interface for price oracles
#[contract]
pub trait TwapOracleInterface {
    /// Get TWAP price for token over specified window
    /// @param token: Token address
    /// @param window_seconds: TWAP window in seconds (minimum 1800 = 30 minutes)
    /// @returns: Price in basis points (e.g., 20_000 = $20.00 per token unit)
    fn get_price_twap(env: Env, token: Address, window_seconds: u64) -> i128;
    
    /// Get available TWAP windows supported by this oracle
    fn get_supported_windows(env: Env) -> Vec<u64>;
    
    /// Get price with recommended default window (e.g., 1 hour)
    fn get_price(env: Env, token: Address) -> i128 {
        // Default to 1-hour TWAP for backward compatibility
        self.get_price_twap(env, token, 3600)
    }
}
```

#### 2.2 Enhanced Oracle Registry with Validation
Extend `oracle_registry.rs` to validate oracle capabilities:

```rust
// In oracle_registry.rs - enhance register_oracle function
pub fn register_oracle(
    env: &Env,
    feed_type: OracleFeedType,
    oracle: Address,
    required_capabilities: Option<OracleCapabilities>,
) -> Result<(), ContractError> {
    require_admin(env)?;
    
    // Verify interface version
    let version = verify_oracle_interface_version(env, &oracle)?;
    
    // For Price feeds, optionally verify TWAP capability
    if feed_type == OracleFeedType::Price {
        if let Some(caps) = required_capabilities {
            if caps.requires_twap {
                verify_twap_capability(env, &oracle, caps.min_twap_window)?;
            }
        }
    }
    
    // ... existing registration logic
}

struct OracleCapabilities {
    requires_twap: bool,
    min_twap_window: u64,  // Minimum TWAP window in seconds
    multi_source: bool,    // Requires multiple data sources
}
```

#### 2.3 Price Manipulation Detection System
Implement off-chain monitoring system:

```python
# Example monitoring system architecture
class OracleManipulationDetector:
    def __init__(self, oracle_address, token_address):
        self.oracle = oracle_address
        self.token = token_address
    
    def detect_manipulation(self, price_history, window_hours=24):
        """Detect potential price manipulation"""
        metrics = {
            'volatility': self.calculate_volatility(price_history),
            'deviation': self.calculate_deviation_from_median(price_history),
            'flash_loan_pattern': self.detect_flash_loan_pattern(price_history),
            'volume_spike': self.detect_volume_spikes(price_history),
        }
        
        risk_score = self.calculate_risk_score(metrics)
        return risk_score > THRESHOLD
    
    def alert_governance(self, manipulation_detected):
        """Trigger governance alerts if manipulation detected"""
        if manipulation_detected:
            # 1. Emit on-chain event
            # 2. Notify governance multisig
            # 3. Post to governance forum
            # 4. Consider temporary oracle suspension
```

### 3. Implementation Roadmap

#### Phase 1: Immediate (Pre-Mainnet)
1. **Update Documentation:** Complete threat model and vetting criteria updates
2. **Governance Policy:** Formalize oracle approval requirements in governance docs
3. **Monitoring Setup:** Basic price anomaly detection for testnet

#### Phase 2: Short-Term (1-3 Months)
1. **TWAP Interface Standard:** Publish and promote standard TWAP interface
2. **Reference Implementation:** Create open-source TWAP oracle contract
3. **Enhanced Registry:** Add capability validation to oracle registry

#### Phase 3: Medium-Term (3-6 Months)
1. **Multi-Oracle Fallback:** Implement fallback oracle system
2. **Automated Governance:** Smart alerts for manipulation detection
3. **Insurance Mechanism:** Oracle failure insurance fund

### 4. Specific TWAP Implementation Options

#### 4.1 DEX-Based TWAP Oracle
For oracles using Stellar DEX or AMM pools:

```rust
// Simplified DEX TWAP implementation
pub struct DexTwapOracle {
    pool_address: Address,
    price_samples: Vec<(u64, i128)>,  // (timestamp, price)
    max_samples: usize,                // e.g., 180 samples for 30-min window at 10s intervals
}

impl DexTwapOracle {
    fn update_price_sample(&mut self, env: &Env) {
        let current_price = self.get_pool_spot_price(env);
        let current_time = env.ledger().timestamp();
        
        self.price_samples.push((current_time, current_price));
        
        // Maintain sliding window
        if self.price_samples.len() > self.max_samples {
            self.price_samples.remove(0);
        }
    }
    
    fn get_twap_price(&self, window_seconds: u64) -> i128 {
        let window_end = self.price_samples.last().unwrap().0;
        let window_start = window_end.saturating_sub(window_seconds);
        
        let mut total_weighted_price = 0;
        let mut total_weight = 0;
        
        for i in 0..self.price_samples.len() - 1 {
            let (time1, price1) = self.price_samples[i];
            let (time2, price2) = self.price_samples[i + 1];
            
            if time2 < window_start {
                continue; // Sample before window
            }
            
            let sample_start = time1.max(window_start);
            let sample_end = time2.min(window_end);
            
            if sample_end > sample_start {
                let weight = sample_end - sample_start;
                let avg_price = (price1 + price2) / 2;
                
                total_weighted_price += avg_price * (weight as i128);
                total_weight += weight;
            }
        }
        
        if total_weight > 0 {
            total_weighted_price / (total_weight as i128)
        } else {
            // Fallback to latest price
            self.price_samples.last().unwrap().1
        }
    }
}
```

#### 4.2 Off-Chain Signed Feed with TWAP
For oracles like Chainlink or Pyth:

```rust
// Off-chain feed with on-chain TWAP verification
pub struct SignedFeedTwapOracle {
    feed_address: Address,
    historical_prices: PersistentMap<u64, i128>,  // timestamp -> price
}

impl SignedFeedTwapOracle {
    fn verify_and_store_price(&self, env: &Env, signed_price: SignedPrice) -> Result<(), OracleError> {
        // 1. Verify signature from trusted feed
        self.verify_signature(&signed_price)?;
        
        // 2. Verify price is not stale
        let current_time = env.ledger().timestamp();
        if current_time - signed_price.timestamp > MAX_STALENESS {
            return Err(OracleError::StalePrice);
        }
        
        // 3. Store in historical price map
        self.historical_prices.set(signed_price.timestamp, signed_price.price);
        
        // 4. Trim old prices
        self.trim_old_prices(current_time);
        
        Ok(())
    }
    
    fn get_twap_price(&self, env: &Env, window_seconds: u64) -> i128 {
        let current_time = env.ledger().timestamp();
        let window_start = current_time.saturating_sub(window_seconds);
        
        let mut total_price = 0;
        let mut count = 0;
        
        for (timestamp, price) in self.historical_prices.iter() {
            if timestamp >= window_start && timestamp <= current_time {
                total_price += price;
                count += 1;
            }
        }
        
        if count > 0 {
            total_price / (count as i128)
        } else {
            // Fallback to latest verified price
            self.get_latest_price()
        }
    }
}
```

### 5. Integration Guidelines for ILN

#### 5.1 Contract Integration Pattern
```rust
// Recommended integration pattern for ILN contracts
pub struct PriceOracleClient {
    oracle_address: Address,
    use_twap: bool,
    twap_window: u64,  // Default: 3600 seconds (1 hour)
}

impl PriceOracleClient {
    fn get_price_safe(&self, env: &Env, token: Address) -> Result<i128, OracleError> {
        if self.use_twap {
            // Use TWAP for manipulation resistance
            let twap_client = TwapOracleClient::new(env, &self.oracle_address);
            match twap_client.try_get_price_twap(&token, &self.twap_window) {
                Ok(price) => Ok(price),
                Err(_) => {
                    // Fallback to spot price if TWAP not available
                    let spot_client = OracleClient::new(env, &self.oracle_address);
                    spot_client.get_price(&token)
                }
            }
        } else {
            // Direct spot price (only for non-critical uses)
            let client = OracleClient::new(env, &self.oracle_address);
            client.get_price(&token)
        }
    }
}
```

#### 5.2 Configuration Recommendations
```toml
# Recommended oracle configuration for production
[oracle.price]
use_twap = true
twap_window_seconds = 3600  # 1 hour minimum
min_data_sources = 3
max_staleness_seconds = 300  # 5 minutes
fallback_oracle_enabled = true

[oracle.monitoring]
manipulation_detection = true
alert_threshold_volatility = 0.15  # 15% price change in 5 minutes
alert_threshold_deviation = 0.10   # 10% deviation from median
```

### 6. Emergency Procedures

#### 6.1 Oracle Manipulation Detected
1. **Immediate:** Emit emergency event and pause price-dependent operations
2. **Short-term:** Switch to fallback oracle or use last known good price
3. **Medium-term:** Governance vote to replace compromised oracle
4. **Long-term:** Investigate root cause and enhance protections

#### 6.2 Oracle Failure
1. **Circuit Breaker:** Automatic pause if price deviation > threshold
2. **Manual Override:** Governance multisig can set emergency price
3. **Compensation:** Consider insurance for losses from oracle failure

### 7. Conclusion

Implementing TWAP and manipulation-resistant mechanisms is **CRITICAL** for any price oracle integration in ILN. While current usage is limited to statistical reporting, future protocol expansions could introduce price-dependent financial operations.

**Priority Actions:**
1. **Immediate:** Formalize governance policy prohibiting unprotected spot price oracles
2. **Short-term:** Develop and audit reference TWAP oracle implementation
3. **Ongoing:** Implement monitoring and alerting for price manipulation

By implementing these recommendations, ILN can maintain robust defenses against oracle manipulation attacks while enabling secure price oracle integrations for future protocol features.

## 8. TWAP Migration Plan

### 8.1 Contract Upgrades
- The new `invoice_liquidity` contract introduces TWAP support built directly into the registry (`oracle_registry.rs`).
- Upgrade the contract WASM. TWAP will be disabled by default for all feeds (`DataKey::TwapEnabled` defaults to false), ensuring the spot price fallback continues to operate seamlessly post-upgrade.

### 8.2 Observation Accumulation
- Before enabling TWAP for a feed, `get_twap_price` will not be invoked.
- However, samples must be recorded *before* enabling TWAP so that `get_twap_price` has data to read.
- Have a script periodically invoke `record_twap_sample` (or have the indexer/automation do so) for the feed and tokens of interest.

### 8.3 Enabling TWAP
- Once the buffer of observations is sufficiently populated (i.e. at least `MIN_TWAP_OBSERVATIONS` samples, default 2), an admin can call `set_twap_enabled(env, feed_type, true)`.
- At that point, `get_twap_aware_price` will read the TWAP. If sufficient observations are not present, it will return an `InsufficientTwapObservations` error.
- Therefore, DO NOT enable TWAP on a feed until the buffer has been properly warmed up.

### 8.4 Backwards Compatibility
- The `get_twap_aware_price` will automatically fall back to the spot price `get_verified_price` if `is_twap_enabled` is false. This guarantees backward compatibility during the transition period.