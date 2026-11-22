# Security Policy

ILN spans Soroban smart contracts, a TypeScript SDK, a CLI, an indexer, and a notifications service. ILN is in experimental/testnet phase — do not use current deployments as mainnet-secure infrastructure until the mainnet checklist, audits, and maintainer sign-off are complete.

For the full policy — including component-specific vulnerability classes, detailed severity definitions, response timelines, reporting instructions, safe harbor, and maintainer handling procedures — see [docs/security.md](docs/security.md).

## Responsible Disclosure

Please do not disclose suspected vulnerabilities publicly before maintainers have had time to investigate and remediate them. "Publicly" includes issue and pull-request comments, Discord, Telegram, and social media. If you have already opened a public report, keep the body empty, edit it to a neutral title, and ask a maintainer to convert or hide it.

### Reporting channels

1. **GitHub Security Advisory — preferred.** Open a private draft advisory against this repository at
   <https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/security/advisories/new>.
   You can also reach it from the repository's **Security** tab → **Report a vulnerability**, or from **New issue** → **Report a security vulnerability**.
   A private advisory is visible only to you and the repository's maintainers, keeps the discussion and any proof-of-concept out of the public issue tracker, and produces a publishable CVE-backed advisory at the end.
   Private vulnerability reporting is **enabled** on this repository (see [Contact Status](#contact-status)). You must be signed in to GitHub. If the page nevertheless returns `404`, the setting has been switched off again — open a detail-free issue as described below and a maintainer will re-enable it ([owner steps](#enabling-private-vulnerability-reporting-owner-only)).
2. **Security email — currently unavailable.** No ILN-owned security mailbox is reachable today; see [Contact Status](#contact-status). Do not send a report to any address marked broken there — it will bounce silently and your report will be lost.

If neither channel works for you, open an ordinary issue whose **title and body contain no detail about the finding at all** (for example: "Security report — please advise on a private channel") and ask a maintainer to point you at a working one. Never describe the vulnerability, the affected function, or the affected contract in anything public.

### What to include

- Affected component, and the commit, tag, branch, or deployed contract ID.
- Steps to reproduce, with expected versus actual behavior.
- Impact: which assets, users, or permissions are affected, and whether you believe it is actively exploitable.
- Proof-of-concept code, transaction XDR, logs, screenshots, or traces where available.

Do not include live secrets, private keys, or personally identifiable information unless maintainers explicitly request a secure transfer method.

### Embargo and coordinated disclosure

- Maintain an embargo on all details from the moment you report until maintainers publish a fix, mitigation, or advisory, whichever comes first.
- The default embargo is **90 days** from the date of the first report. If a fix is not available after that, contact the maintainers before publishing anything; do not treat 90 days as unilateral permission to disclose.
- Actively exploited or mainnet fund-at-risk findings follow a shorter, mutually agreed timeline — maintainers will propose it in the acknowledgement.
- Maintainers will credit the reporter in the published advisory unless asked otherwise, and will not pursue legal action against good-faith research conducted under this policy (see [Safe Harbor](docs/security.md#safe-harbor)).

## Contact Status

The channel table below records what a contributor has actually checked, so that nobody wastes a report on an address that cannot deliver it.

| Channel | Status | Verification note |
|---------|--------|-------------------|
| `https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/security/advisories/new` | **Working — preferred** | Private vulnerability reporting was disabled until 2026-11-21, which is why researchers signed in with non-collaborator accounts received `404` (issues #1155, #1156). A repository admin enabled it on 2026-11-21 (`PUT /repos/{owner}/{repo}/private-vulnerability-reporting` → `204`), and `GET` on the same endpoint now returns `{"enabled":true}`. An anonymous request is still redirected to sign-in, so test while logged in. |
| `security@invoice-liquidity-network.local` | **Broken — do not use** | Published in the previous revision of this file. `.local` is reserved for multicast DNS and is never resolved by public DNS, so no mail server can accept mail for it. |
| `security@invoiceliquidity.network` | **Broken — do not use** | Reported unreachable by a researcher. `invoiceliquidity.network` has no DNS records at all (`NXDOMAIN`, and therefore no `MX`), so mail to it cannot be routed. |
| `security@drips.network` | **Unconfirmed — owner must confirm or disown** | Previously published in [docs/upgrade-guide.md](docs/upgrade-guide.md), next to a link to the `drips-network` organization, which does not host this repository (`404`). The domain does resolve with mail exchangers, but no maintainer has said that ILN's security team reads that mailbox, so it must not be used for disclosures until they do. This revision points that document at this policy instead. |

An owner should add one verified, ILN-owned security mailbox to this table (and to [docs/security.md](docs/security.md)) once mail delivery for it has been tested end to end. This document deliberately does not invent an address.

## Response Commitments

Reporters can expect, at minimum:

| Stage | Timeline |
|-------|----------|
| Acknowledgment | Within 48 hours of the report reaching a maintainer |
| Initial severity assessment | Within 5 business days |
| Critical fix | Begin mitigation immediately; target patch within 7 days |
| High fix | Target patch within 14 days |
| Medium fix | Target patch within 30 days |

The 48-hour acknowledgment clock is measured from the moment a report is delivered to a working channel, which is why [Contact Status](#contact-status) above matters: a bounced email never starts the clock.

## Severity Levels

| Severity | Description |
|----------|-------------|
| Critical | Direct loss or theft of user funds, permanent protocol insolvency, or unauthenticated upgrade/admin takeover |
| High | Material fund risk, broad data integrity failure, secret exposure, or reliable service compromise |
| Medium | Limited financial or operational impact, denial of service with recovery path, or scoped data exposure |
| Low | Defense-in-depth issue, documentation security gap, low-impact information exposure |
| Informational | No immediate exploit path but useful for hardening |

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

----

## Enabling Private Vulnerability Reporting (Owner Only)

Contributors cannot enable this; only an owner or a maintainer with repository-admin rights can. While it is disabled, a signed-in non-collaborator opening the advisory URL in [Contact Status](#contact-status) receives `404`, so reporters have no private channel at all. It is currently **enabled**; these steps are for re-enabling it or verifying it after a settings change.

Fastest path — one API call with an admin token, then verify:

```sh
gh api -X PUT repos/Invoice-Liquidity-Network/ILN-Smart-Contract/private-vulnerability-reporting   # 204 No Content
gh api repos/Invoice-Liquidity-Network/ILN-Smart-Contract/private-vulnerability-reporting           # {"enabled":true}
```

Equivalent UI path and related settings:

1. On GitHub, as a repository admin: **Settings → Advanced security → Code security and analysis** (the same group is reachable from the repository **Security** tab → the settings badge) → enable **Secret scanning alerts** if you want them, and tick **Private vulnerability reporting**.
2. API equivalents exist only for the *other* switches. `PATCH /repos/{owner}/{repo}` accepts `security_and_analysis` with the keys `advanced_security`, `secret_scanning`, `secret_scanning_push_protection`, and `dependabot_security_updates`, each with status `enable`/`disable`/`auto`; Dependabot security updates also have a dedicated endpoint:

   ```sh
   gh api -X PATCH repos/Invoice-Liquidity-Network/ILN-Smart-Contract --input - <<'JSON'
   {
     "security_and_analysis": {
       "advanced_security": { "status": "enable" },
       "secret_scanning": { "status": "enable" },
       "dependabot_security_updates": { "status": "enable" }
     }
   }
   JSON

   gh api -X POST repos/Invoice-Liquidity-Network/ILN-Smart-Contract/vulnerability-alerts
   ```

   Two gotchas worth knowing before you trust a scripted "fix": the endpoint silently ignores keys it does not recognize (`secret_scanning_alerts` is a GraphQL name, not a REST one, and `has_vulnerability_alerts` in `PATCH -f has_vulnerability_alerts=true` is not a documented parameter of this endpoint at all — both return HTTP 200 while changing nothing), and `advanced_security` answers HTTP 400 "Invalid value for Advanced Security status" on forks or where GHAS is not licensed, in which case use `"auto"` to inherit the organization setting.
3. Private vulnerability reporting is *not* part of `security_and_analysis`; it has its own endpoint (`PUT`/`DELETE`/`GET /repos/{owner}/{repo}/private-vulnerability-reporting`), shown at the top of this section. Verify it took effect with an account that is **not** a collaborator, signed in to github.com, by opening
   <https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/security/advisories/new>:
   disabled renders `404`, enabled renders the "Report a vulnerability" form. Do not test this with `curl` from an unauthenticated shell — GitHub redirects anonymous requests for that page to sign-in, which answers `200` either way and therefore proves nothing.
4. If the checkbox is greyed out, an organization owner has to allow private vulnerability reporting at the organization level first (**org Settings → Code security and analysis**), and the repository must be public.
5. Once reporting works, update the [Contact Status](#contact-status) table here and mirror the same wording into [docs/security.md](docs/security.md).
