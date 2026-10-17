# Public Support Channels

This document describes where to get help with the Invoice Liquidity Network protocol
**and which channels are actually live** ahead of mainnet.

Last audited: 2026-09-24.

---

## Channel status (pre-mainnet)

| Channel | Status | Use for |
|---------|--------|---------|
| **GitHub Issues** (bug / feature / component templates) | **Live** | Bugs, feature requests, tracked work |
| **GitHub Security Advisories** + `security@invoice-liquidity-network.local` | **Live** (private) | Vulnerabilities only — see [SECURITY.md](../SECURITY.md) |
| **GitHub Discussions** | **Not enabled yet** on this repository | Planned for open-ended integration Q&A after launch readiness; until then use Issues |
| Discord / other chat | **Out of scope pre-launch** | Not stood up; do not treat as an official channel |

**Scoped decision:** For mainnet launch readiness we treat **GitHub Issues + private security reporting** as the complete public support surface. Standing up Discord (or enabling Discussions) is optional post-launch community work, not a launch blocker once Issues templates and this doc are accurate.

---

## Bug Reports

Report bugs by opening a GitHub issue:

- [General bug report](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/new?template=bug_report.md)
- [SDK bug report](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/new?template=sdk-bug.md)
- [CLI bug report](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/new?template=cli-bug.md)
- [Indexer bug report](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/new?template=indexer-bug.md)

Choose the template that matches the component you are having trouble with. Include
steps to reproduce, expected behaviour, and actual behaviour.

---

## Integration Questions

GitHub Discussions are **not enabled** on this repository yet. Until they are:

- Open a **GitHub Issue** using the
  [feature request template](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/new?template=feature_request.md)
  for new functionality, or a bug template if something is broken
- Tag maintainers via CODEOWNERS on a draft PR if you are proposing an integration change

When Discussions are enabled, this section will be updated and the checklist row will stay in sync.

---

## Feature Requests

Suggest new features or improvements via the
[feature request template](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/new?template=feature_request.md).

---

## Incidents & status

Protocol incidents are handled under
[`incident-response-runbook.md`](incident-response-runbook.md). There is no separate
public status Discord; user-facing notices go through GitHub (issues/advisories)
and release notes.

---

## Response Times

| Channel | Expected Response |
|---------|-------------------|
| Bug reports (GitHub Issues) | Acknowledged within 3 business days |
| Integration / feature Issues | Best-effort, typically within 1 week |
| Security (private) | Per [SECURITY.md](../SECURITY.md) (ack within 48 hours) |

These are targets, not guarantees. Response times may vary based on maintainer
availability and issue complexity.

---

## Security Vulnerabilities

**Do not** report security vulnerabilities through public GitHub issues.
See [SECURITY.md](../SECURITY.md) for responsible disclosure instructions,
including the private reporting email and GitHub Security Advisory process.
