# Oracle Provider Vetting

This document defines the criteria the community should use to evaluate a
proposed oracle provider **before voting to register it** via governance, and
provides a template proposers must fill out when submitting a
`RegisterOracle` proposal.

It complements, but is distinct from, [Oracle Integration](oracle-integration.md)
and [Oracle Design](oracle-design.md):

| Document | Answers |
|---|---|
| [Oracle Design](oracle-design.md) | How does ILN consume oracle data? What's the trust model and failure behaviour? |
| [Oracle Integration](oracle-integration.md) | How does a provider technically implement and deploy a compatible oracle contract? |
| **This document** | *Should* a given implementation be trusted with real funds? What must a governance proposal disclose before token holders vote? |

Passing the technical interface check (`interface_version() == ORACLE_INTERFACE_VERSION`,
enforced on-chain by `register_oracle`/`register_token_oracle`) proves a
contract is *wired up correctly*. It proves nothing about whether the data it
serves is trustworthy, timely, or resistant to manipulation. That judgment is
what this document is for.

---

## 1. Scope

This vetting process applies to any oracle contract proposed for registration
in the registry described in [ADR-010](adr/ADR-010-oracle-registry.md) —
i.e. any `RegisterOracle(feed_type, oracle)` governance proposal, for any
`OracleFeedType` (`Price`, `Identity`, `Credit`).

It is a **social/process layer, not an on-chain gate**: nothing in the
contracts enforces that this checklist was completed. The mechanism that
makes it binding is `create_proposal`'s `description_hash` — the vetting
write-up is published off-chain, hashed, and that hash is submitted with the
proposal (see [§5](#5-governance-proposal-template)), so voters have a
verifiable, immutable reference to what was disclosed at proposal time.

`RemoveOracle` proposals (or an admin veto of a bad `RegisterOracle`
proposal, see [governance.md §8](governance.md#8-admin-veto-power)) are the
remediation path if a registered provider is later found to fail these
criteria — removal itself does not require a new vetting write-up, since it
only reduces trust surface.

---

## 2. Vetting Criteria

Each criterion below should be addressed explicitly in the proposal. "Unknown"
or "not applicable" is an acceptable answer where genuinely true (e.g. a
brand-new provider has no incident history), but the proposal must say so
rather than omit the section.

### 2.1 Track record

- How long has this provider operated in production, and on which networks?
- What other protocols consume this provider's data, and what value do those
  integrations secure (a rough proxy for how battle-tested the feed is)?
- Has the provider's own contract code been independently audited? By whom,
  when, and are the reports public?
- Is the team/organization publicly identified, or pseudonymous/anonymous?
  Anonymity is not automatically disqualifying, but it removes an entire
  category of accountability and should be weighed accordingly.

### 2.2 Decentralization of the provider's own data sources

A registered oracle is only as trustworthy as what feeds it. Evaluate:

- How many **independent** upstream data sources or node/reporter operators
  feed this oracle? A single centralized API wrapped in a thin on-chain
  contract is a single point of failure regardless of how solid the contract
  code is.
- What aggregation method is used (median, mean, trimmed mean, stake-weighted,
  etc.) and what is the minimum reporter count / quorum before a value is
  accepted?
- What deviation/outlier-rejection logic exists to resist a single bad or
  malicious reporter skewing the result?
- Who can call the oracle's own update/write path, and how is that access
  controlled (single admin key, multisig, permissionless reporter set with
  staking/slashing)? This mirrors the `update_verification` access-control
  expectation already called out in
  [Oracle Integration §Security Checklist](oracle-integration.md#security-checklist) —
  vetting should confirm the provider actually meets it, not just assume it.

### 2.3 Update frequency / freshness SLA

- What is the provider's documented data-refresh cadence, and does the
  provider commit to an SLA (vs. best-effort)?
- Is that cadence comfortably tighter than the staleness threshold ILN will
  configure for this feed (`max_oracle_age_ledgers` per
  [ADR-010](adr/ADR-010-oracle-registry.md); the legacy `Identity` fallback
  additionally has the fixed 7‑day `ORACLE_STALENESS_THRESHOLD_SECS` described
  in [Oracle Integration](oracle-integration.md#staleness-policy))? A provider
  whose SLA is close to or looser than ILN's staleness threshold will cause
  routine `OracleDataStale` rejections under normal operation, not just during
  incidents.
- What monitoring/alerting does the provider run on their own freshness, and
  is that status publicly visible (a status page, on-chain heartbeat, etc.)?
- What is the provider's process and typical latency for correcting a
  reported error once identified?

### 2.4 Past incident history

- Any publicly known outages, stale-data incidents, price manipulation
  events, or depegs attributable to this provider's feeds?
- For each known incident: was it disclosed proactively, was a postmortem
  published, and what changed afterward to prevent recurrence?
- Any history of disputes with integrating protocols or their governance
  communities?
- A provider with zero incident history should be treated as *unproven*, not
  automatically *safe* — track record (§2.1) and source decentralization
  (§2.2) matter more for a newer provider precisely because there isn't yet
  an incident record to evaluate.

### 2.5 Technical & operational compliance

- Confirms `interface_version()` reports `ORACLE_INTERFACE_VERSION` (see
  `contracts/invoice_liquidity/src/oracle_interface.rs`) — checked
  automatically on-chain, but the vetting write-up should still name the
  version and confirm which interface methods were manually reviewed, since
  a version match alone doesn't prove the *implementation* behind those
  methods is correct.
- Is the oracle contract itself upgradeable? If so, by whom, and is there a
  timelock or multisig on that upgrade path? An upgradeable oracle contract
  means the trust decision governance is voting on today can change later
  without a new ILN-side proposal.
- How is the oracle contract's own admin/operator key custodied (hardware
  wallet, multisig, HSM)? A single hot key operating a widely-relied-upon
  oracle is a material risk regardless of the data source quality.

### 2.6 Sandwich Attack Resistance (Price Feeds Only)

**Applies only to `OracleFeedType::Price` feed proposals.**

Price oracles that derive from on-chain DEX/AMM liquidity pools are vulnerable
to sandwich attacks where an adversary can manipulate the reported price by
front-running oracle queries. For ILN's use case (USD volume normalization in
contract statistics), evaluate:

- **Data Source Type:** Does the oracle use:
  - Off-chain signed price feeds (Chainlink, Pyth, etc.) — **LOW risk**
  - On-chain DEX spot prices without protection — **HIGH risk**
  - On-chain DEX prices with TWAP protection — **MEDIUM-LOW risk**
  - Multi-source aggregation — **REDUCED risk**
- **TWAP Implementation:** If using on-chain prices, does the oracle implement
  Time-Weighted Average Price (TWAP) over a sufficient window (e.g., ≥30
  minutes)? A single-block spot price is trivially manipulable; TWAP requires
  sustained manipulation across many blocks, raising attack cost.
- **Manipulation Detection:** Does the provider monitor for price manipulation
  attempts (sudden large deviations, wash trading patterns) and have procedures
  to pause or revert suspicious updates?
- **Historical Consistency:** For DEX-based oracles, what safeguards exist
  against flash loan attacks that could temporarily distort prices within a
  single transaction?

**Governance Policy:** ILN governance should reject any `Price` feed proposal
that uses on-chain DEX spot prices without TWAP protection or equivalent
manipulation resistance. See [threat-model.md §D3](../threat-model.md#d3-price-oracle-sandwich-attacks-issue-39)
for the full risk analysis.

### 2.7 Economic & operational disclosure

- Any fees charged for querying or maintaining the feed, and who bears them.
- Any jurisdictional or regulatory exposure worth flagging for voters
  (informational only — this document does not make legal determinations).

---

## 3. Vetting Checklist

Proposers should be able to check every applicable box before submitting a
`RegisterOracle` proposal; reviewers/voters should expect an explicit answer
(including "N/A" with justification) for any unchecked item.

- [ ] Track record documented: time in production, networks, other integrators
- [ ] Independent audit(s) of the provider's own contracts identified, with links
- [ ] Team/organization identity disclosed (or anonymity explicitly noted)
- [ ] Data source count and aggregation/quorum method documented
- [ ] Oracle's own update-path access control documented (who can write data)
- [ ] Documented update-frequency SLA compared against ILN's configured staleness threshold for this feed type
- [ ] Provider's own freshness monitoring/status visibility documented
- [ ] Known incident history disclosed, with postmortems linked where they exist
- [ ] `interface_version()` value confirmed and interface methods manually reviewed
- [ ] Oracle contract upgradeability and upgrade-authority documented
- [ ] Oracle contract admin/operator key custody documented
- [ ] **For Price feeds only:** Sandwich attack resistance documented (data source type, TWAP if DEX-based)
- [ ] Fee structure disclosed, if any

---

## 4. Governance Proposal Template

Publish the filled-out template below (forum post, GitHub issue, or
equivalent) **before** calling `create_proposal`. Hash the published document
(SHA-256) and pass that hash as `create_proposal`'s `description_hash`
argument — the same pattern already used for `veto_proposal`'s `reason_hash`
(see [governance.md §8](governance.md#8-admin-veto-power)). This gives voters
an immutable, on-chain-referenced link between what was disclosed and what
they're voting on.

```markdown
## Oracle Provider Proposal: <Provider Name>

**Feed type:** Price | Identity | Credit
**Oracle contract address:** <C... address, per target network>
**Interface version reported:** <value returned by interface_version()>
**Proposed on-chain action:** ProposalAction::RegisterOracle(<FeedType>, <oracle_address>)

### 1. Track record
<Time in production, networks, other integrators, audits with links,
team/organization identity>

### 2. Decentralization of data sources
<Number of independent sources/reporters, aggregation method, quorum,
outlier handling, who can write updates and how that access is controlled>

### 3. Update frequency / freshness SLA
<Documented refresh cadence and SLA, comparison against the max_oracle_age
this proposal intends to configure, provider's own freshness monitoring>
Note: the contract does not only bound *how old* a response may be — a
provider that stops advancing is also bounded. `fund_invoice` rejects a
`timestamp` strictly older than the last recorded for the same oracle
(`OracleTimestampRegression`, Issue #1052) and rejects one that repeats an
identical value more than `MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3) times in a row
(`OracleDuplicateUpdate`, Issue #1053). A provider whose cadence can leave the
`timestamp` unchanged across several consecutive queries must explain why, or
it will be treated as stalled.

### 4. Past incident history
<Known outages/incidents with dates, postmortems linked, or "None known" —
do not omit this section>

### 5. Technical & operational notes
<Contract upgradeability and upgrade authority, admin/operator key custody,
any fees>

### 6. Sandwich Attack Resistance (Price feeds only)
<For Price feed proposals only: data source type, TWAP implementation if DEX-based,
manipulation detection mechanisms, historical consistency safeguards>

### 7. Links
- Audit report(s):
- Provider documentation:
- Contract source / verification (e.g. Stellar Expert):
- Prior incident postmortems (if any):
```

### Submitting

1. Publish the filled template at a stable URL.
2. Compute `description_hash = SHA-256(document_bytes)`.
3. Call `create_proposal(proposer, ProposalAction::RegisterOracle(feed_type, oracle), description_hash, proposed_value)`
   (see [governance.md §5](governance.md#5-worked-example--end-to-end-proposal)
   for the general proposal lifecycle; `proposed_value` is unused by
   `RegisterOracle` and may be `0`).
4. Link the published document in the on-chain proposal's off-chain
   discussion thread so voters can verify the hash matches before voting.

---

## 5. Related documents

- [Oracle Design](oracle-design.md) — trust model and failure-mode behaviour
- [Oracle Integration](oracle-integration.md) — technical deployment/registration steps
- [ADR-010: Governance-Controlled Oracle Registry](adr/ADR-010-oracle-registry.md) — registry architecture and rationale
- [Governance](governance.md) — proposal lifecycle, voting, quorum, and admin veto
