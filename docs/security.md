# Security Policy

ILN spans Soroban smart contracts, a TypeScript SDK, a CLI, an indexer, and a notifications service. ILN is in experimental/testnet phase — do not use current deployments as mainnet-secure infrastructure until the mainnet checklist, audits, and maintainer sign-off are complete.

For the full policy — including component-specific vulnerability classes, detailed severity definitions, response timelines, reporting instructions, safe harbor, and maintainer handling procedures — see [docs/security.md](docs/security.md).

## Responsible Disclosure

Please do not disclose suspected vulnerabilities publicly before maintainers have had time to investigate and remediate them.

Report issues by either:

- Emailing `security@invoice-liquidity-network.local`
- Opening a private GitHub Security Advisory for this repository

## Severity Levels

| Severity | Description |
|----------|-------------|
| Critical | Direct loss or theft of user funds, permanent protocol insolvency, or unauthenticated upgrade/admin takeover |
| High | Material fund risk, broad data integrity failure, secret exposure, or reliable service compromise |
| Medium | Limited financial or operational impact, denial of service with recovery path, or scoped data exposure |
| Low | Defense-in-depth issue, documentation security gap, low-impact information exposure |
| Informational | No immediate exploit path but useful for hardening |

## Response Commitments

| Stage | Timeline |
|-------|----------|
| Acknowledgment | Within 48 hours |
| Initial severity assessment | Within 5 business days |
| Critical fix | Begin mitigation immediately; target patch within 7 days |
| High fix | Target patch within 14 days |
| Medium fix | Target patch within 30 days |

See [docs/security.md](docs/security.md) for full details including safe harbor, maintainer handling procedures, and the security checklist for releases.

## Supported Versions

| Version | Supported |
|---------|-----------|
| Experimental/testnet | Best-effort security fixes |
| Mainnet | Not yet launched |

## Bug Bounty Program (Post-Mainnet)

We operate a continuous bug bounty program to reward researchers for finding vulnerabilities in ILN Smart Contracts.

### Scope
- In-scope: All production Soroban contracts (`contracts/*`) deployed to mainnet.
- Out-of-scope: CLI utilities, off-chain indexers, demo scripts.

### Reward Tiers (USDC)
- **Critical:** Up to $50,000
- **High:** Up to $15,000
- **Medium:** Up to $5,000
- **Low:** Up to $1,000

Rewards are funded directly from the ILN DAO Treasury multi-sig.

### Safe Harbor
Good faith security research that follows this disclosure policy will be considered authorized. We will not pursue civil or criminal action against researchers who comply with these guidelines.

### Contact Rotation and Backup Coverage
To eliminate single points of failure in incident response:
1. The `security@` alias forwards to the active on-call engineer (rotating weekly).
2. If the on-call engineer does not acknowledge the report within 24 hours (SLA breach), the alias automatically escalates to the full Core Maintainers group.
3. Backup contacts: In the event of a total alias failure, direct messages to core team leads on the official Discord are authorized for initial triage (please do not disclose details publicly).
