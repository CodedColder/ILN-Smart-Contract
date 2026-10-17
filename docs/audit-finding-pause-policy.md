# Audit Finding: Contract Pause Authority Policy

## Overview
When a finding is disclosed, deciding whether to pause the protocol is critical. We must avoid both over-caution (disrupting legitimate users unnecessarily) and under-caution (leaving funds exposed).

## Decision Criteria
1. **Critical Vulnerability (Active/Imminent Exploit):** 
   - *Impact:* Immediate risk of fund loss.
   - *Action:* **PAUSE IMMEDIATELY**.
2. **High Vulnerability (Complex Exploit):** 
   - *Impact:* High risk, but requires extreme conditions or significant capital.
   - *Action:* **PAUSE** unless a narrower mitigation (e.g., blacklisting a specific malicious actor or freezing a specific sub-pool) is possible within 1 hour.
3. **Medium/Low Vulnerability:**
   - *Impact:* No direct risk of fund loss (e.g., UI glitch, minor DoS).
   - *Action:* **DO NOT PAUSE**. Deploy a standard patch via normal governance.

## Authority
Only the **Security Multisig (3-of-5)** holds the emergency `pause` authority specifically for audit finding response. Governance (DAO) handles standard unpausing and upgrades.

## Cross References
- [Incident Response Runbook](incident-response-runbook.md)
