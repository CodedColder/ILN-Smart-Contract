# Remediation Policy for Critical Findings

For any security finding graded as **Critical**, the ILN core team requires a mandatory third-party re-verification (re-audit) before the fix can be deployed to mainnet.

## Triggers
- Any finding explicitly categorized as **Critical** by an external auditor or internal tabletop exercise.
- Any finding involving direct risk of unrecoverable loss of funds or core protocol bricking.

## Re-Verification Process
1. Internal fix developed and reviewed by at least 2 core maintainers.
2. Fix deployed to testnet.
3. Fix diff submitted back to the original auditing firm (or an equivalent tier-1 security partner).
4. Written sign-off acquired from the auditing firm explicitly affirming the fix is complete and introduces no regressions.
