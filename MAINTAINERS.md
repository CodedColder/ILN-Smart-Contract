# Maintainers & Emergency Contacts

**Status:** Confirmed for mainnet-launch readiness (Issue #904).  
**CODEOWNERS:** [`.github/CODEOWNERS`](.github/CODEOWNERS) — GitHub team reviews are the source of truth for PR routing.

ILN uses **GitHub Organization teams** under `@Keengfk` rather than personal logins in CODEOWNERS, so ownership survives individual rotation. Area leads below are the accountable roles for checklist sign-off; escalate via the team mention and the security channel.

## Ownership areas

| Area | CODEOWNERS team | Primary paths | Emergency contact method |
|------|-----------------|---------------|--------------------------|
| Contracts | `@Keengfk/contracts-team` | `contracts/` | GitHub team mention + private Security Advisory if funds at risk |
| Security | `@Keengfk/security-lead` | `SECURITY.md`, security docs | `security@invoice-liquidity-network.local` / GitHub Security Advisory ([SECURITY.md](SECURITY.md)) |
| Infrastructure / DevOps | `@Keengfk/devops` | `scripts/`, `.github/workflows/` | GitHub team mention; pager/on-call per [incident-response-runbook](docs/incident-response-runbook.md) |
| Documentation | `@Keengfk/docs-lead` | `docs/` | GitHub team mention |
| Community / default | `@Keengfk/maintainers` | everything else (`*`) | GitHub team mention; public support via [support-channels.md](docs/support-channels.md) |

Mainnet admin/multisig signer keys are mapped in [`.github/mainnet-admin-signers.json`](.github/mainnet-admin-signers.json) and checked by [Admin Signer Check CI](.github/workflows/admin-signer-check.yml) against CODEOWNERS. That file is currently empty until mainnet signers are appointed — that is expected pre-launch and does **not** mean CODEOWNERS is placeholder.

## Checklist sign-off mapping

Matches the Maintainer Sign-off table in [`docs/mainnet-launch-checklist.md`](docs/mainnet-launch-checklist.md):

| Sign-off seat | Owning team |
|---------------|-------------|
| Contracts | `@Keengfk/contracts-team` |
| Security | `@Keengfk/security-lead` |
| Infrastructure | `@Keengfk/devops` |
| Documentation | `@Keengfk/docs-lead` |
| Community | `@Keengfk/maintainers` |

## Rotation policy

1. Update `.github/CODEOWNERS` team membership in the GitHub org (not personal redirects in-tree).
2. If a signer key changes, update `mainnet-admin-signers.json` in the same PR.
3. Keep this file's team ↔ area table in sync when a new ownership area is added.
