# ADR-009: Quadratic Voting Weight Calculation

**Date:** 2026-07-27
**Status:** Accepted

## Context

`iln_governance`'s `cast_vote` weighted every vote linearly: weight equals the
voter's token balance plus any delegated balance (Issue #64). Under linear
weighting, a holder with 100x the tokens of another holder gets exactly 100x
the influence over every proposal, which lets a small number of large holders
("whales") dominate outcomes regardless of how many distinct token holders
disagree with them.

Quadratic voting is a common governance mechanism for reducing this
concentration of power: weight is calculated as the square root of the
underlying balance instead of the raw balance. A whale with 100x the tokens
of a small holder ends up with only 10x the vote weight (`sqrt(100) = 10`),
compressing the influence gap without removing it entirely (someone with more
tokens should still have more say, just not linearly more).

Constraints specific to this contract:

- `#![no_std]` Soroban contracts have no floating-point support, so the
  square root must be computed with integer arithmetic.
- The existing linear-weighting behavior (`cast_vote`, `VoteWeightSnapshot`,
  delegation via `DelegatedToMe`) has real proposals and tests depending on
  it (Issues #59, #61, #64). Switching to quadratic weighting unconditionally
  would silently change the meaning of every future vote and the effective
  quorum threshold — a governance token holder's expectations about their
  own voting power would change without an explicit protocol decision.

## Decision

Add a governance-controlled boolean flag, `QuadraticVotingEnabled`
(`is_quadratic_voting_enabled` / `set_quadratic_voting_enabled`), defaulting
to `false`. When enabled, `cast_vote` computes weight as
`isqrt(own_balance + delegated_weight)` instead of the raw sum, where `isqrt`
is a floor integer square root implemented via binary search over `i128`
(bounded by `checked_mul` to avoid overflow on the upper half of the search
space). When disabled, behavior is unchanged from the pre-#530 linear model.

The combined balance (own + delegated) is summed **before** taking the square
root, not summed after separately square-rooting each component. This matches
standard quadratic voting formulations (weight is a function of total voting
power available to the caster, not a sum of already-diminished pieces) and
avoids favoring vote-splitting across multiple delegate chains.

The actual weight applied to a vote (whichever mode was active at cast time)
is recorded per-voter-per-proposal in a new `AppliedVoteWeight` receipt
(temporary storage, same TTL policy as the existing `HasVoted` receipt), so
integrators can query exactly how much weight a specific vote carried without
re-deriving it from balances that may have since changed.

Toggling the flag requires the configured ILN contract address's
authorization — the same governance-controlled-parameter pattern already
used by `set_min_quorum_bps` and `set_min_proposal_balance` — so it is itself
subject to a governance vote, not a unilateral admin switch.

## Alternatives Considered

| Alternative | Why rejected |
|-------------|--------------|
| **Always-on quadratic voting (no toggle)** | Silently changes vote semantics for every existing/future proposal and breaks the documented linear-weighting behavior and its test suite. A governance mechanism change should itself be governable, not hardcoded. |
| **Fixed-point / rational sqrt approximation** | Adds precision complexity (rounding mode choices, extra storage for fractional remainders) with no real benefit — floor integer sqrt is deterministic, gas-cheap, and sufficient for vote-weight comparison purposes. |
| **sqrt(own_balance) + sqrt(delegated_weight) (square-root each component separately, then sum)** | Rewards splitting one's voting power across many small delegate chains (sqrt is concave, so splitting a total into pieces and summing the square roots yields a *larger* number than taking the square root of the sum) — that's the opposite of the anti-whale goal, and would incentivize sybil-like delegation structuring. |
| **Continuous quadratic curve (e.g. `weight^0.5` via a lookup table / oracle)** | Unnecessary complexity for a monotonic integer transform; a lookup table would need to cover the full `i128` balance range or accept approximation error, whereas binary-search isqrt is exact and just as cheap. |
| **Store only the final tally, not a per-voter receipt** | Loses auditability — without `AppliedVoteWeight`, there is no way to verify after the fact whether a specific vote used linear or quadratic weighting, which matters if the flag is toggled mid-voting-window on an active proposal. |

## Consequences

**Positive:**
- Whale influence is compressed (square-root relationship) without removing
  the incentive to hold and stake more governance tokens.
- Fully backwards compatible by default — existing and in-flight proposals
  are unaffected until governance explicitly opts in.
- The `AppliedVoteWeight` receipt gives per-vote auditability independent of
  which mode was active, useful for indexers and dispute resolution.

**Negative / Trade-offs:**
- If the flag is toggled *during* an active proposal's voting window,
  different voters on the same proposal could have cast votes under
  different weighting rules (some linear, some quadratic) depending on when
  they voted relative to the toggle. This mirrors how `set_min_quorum_bps`
  already behaves (mid-window parameter changes are not proposal-scoped) and
  is an accepted limitation rather than something this issue's scope covers;
  a future improvement could snapshot the mode per-proposal at creation time
  the same way `VoteWeightSnapshot` snapshots balances.
- Floor integer sqrt means small balances quantize coarsely (e.g. balances of
  1–3 all yield weight 1, since `isqrt(3) = 1`), so quadratic voting
  effectively raises the practical minimum meaningful stake for a
  distinguishable vote weight.

## Sybil-plus-flash-loan addendum (Issue #809, Update)

Splitting a balance `B` across `N` addresses yields a combined quadratic
weight of `N·isqrt(B/N) ≈ sqrt(B)·sqrt(N)` — a `sqrt(N)` multiplier over
voting from one address. This is inherent to concave weight functions, not a
bug in `isqrt`. Post–Issue #805, every Sybil needs its own balance checkpoint
predating the proposal by `MIN_VOTE_HOLD_LEDGERS` and only carries
`min(checkpoint, current)`, so a flash-funded swarm cannot materialise inside
one transaction; the slow (pre-funded, aged) variant pays linear per-address
costs for a sublinear gain. No additional per-transaction/per-block cap was
added — the checkpoint-aging rule is the rate limit. Full cost model:
[`governance-security-summary.md` §6](../governance-security-summary.md#6-quadratic-sybil-plus-flash-loan-cost-model-issue-809).

## Mainnet Launch Recommendation (Update)

After modelling outcomes with a synthetic realistic token distribution (a power-law resembling typical early-protocol holder concentration), we confirmed that quadratic voting effectively reduces whale dominance (e.g., compressing a 50% dominance down to ~13%) without disenfranchising them entirely.

**Recommendation**: We recommend enabling quadratic voting at mainnet launch. Given the highly concentrated token supply typical of early phases, linear voting would leave the protocol overly centralized in governance. Quadratic voting provides a more robust and fair governance process for the broader community during this crucial initial phase.
