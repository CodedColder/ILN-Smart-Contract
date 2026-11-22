//! Issue #1100 — validate the published cross-contract proof-obligation
//! registry (`docs/proof-obligations.json`) against the repository tree.
//!
//! This mirrors `scripts/check-proof-obligations.py` so the same guarantee is
//! enforced by `cargo test` (no Python/CI dependency for local development):
//!
//!   * every obligation's `source_file` / `test_file` / `doc` exists;
//!   * every `source_symbols` entry occurs in its `source_file`;
//!   * every `verified_by` name occurs as an `fn` in its `test_file`;
//!   * every `#[test]` in each cited `test_file` is claimed by some obligation
//!     (no orphans).

#![cfg(test)]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::string::String;
use std::vec::Vec;

const REGISTRY: &str = include_str!("../../../docs/proof-obligations.json");

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/contracts/iln_governance
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("manifest dir must have two ancestors")
        .to_path_buf()
}

/// Extract the `fn <name>` identifiers present in `src`.
fn fn_names(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while let Some(pos) = src[i..].find("fn ") {
        let start = i + pos + 3;
        let mut end = start;
        while end < bytes.len() {
            let c = bytes[end] as char;
            if c.is_ascii_alphanumeric() || c == '_' {
                end += 1;
            } else {
                break;
            }
        }
        if end > start {
            out.insert(String::from(&src[start..end]));
        }
        i = end;
    }
    out
}

/// Names of `#[test]`-attributed functions in `src`.
fn test_fn_names(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(pos) = rest.find("#[test]") {
        rest = &rest[pos + "#[test]".len()..];
        if let Some(fn_pos) = rest.find("fn ") {
            // Only accept a `fn` that appears before the next attribute/item
            // boundary; attributes between `#[test]` and `fn` are allowed.
            let between = &rest[..fn_pos];
            if between.contains(';') || between.contains('{') {
                continue;
            }
            let start = fn_pos + 3;
            let mut end = start;
            let bytes = rest.as_bytes();
            while end < bytes.len() {
                let c = bytes[end] as char;
                if c.is_ascii_alphanumeric() || c == '_' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > start {
                out.insert(String::from(&rest[start..end]));
            }
            rest = &rest[end..];
        } else {
            break;
        }
    }
    out
}

fn read(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(root.join(rel)).ok()
}

#[test]
fn proof_obligations_registry_is_consistent() {
    let root = repo_root();
    let registry: serde_json::Value =
        serde_json::from_str(REGISTRY).expect("proof-obligations.json must be valid JSON");

    let obligations = registry
        .get("obligations")
        .and_then(|v| v.as_array())
        .expect("registry must have an `obligations` array");
    assert!(
        !obligations.is_empty(),
        "registry must publish at least one obligation"
    );

    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut claimed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (i, ob) in obligations.iter().enumerate() {
        let id = ob
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("obligations[{i}] missing string `id`"));
        assert!(
            seen_ids.insert(String::from(id)),
            "duplicate obligation id: {id}"
        );

        for field in [
            "title",
            "property",
            "doc",
            "source_file",
            "test_file",
            "status",
        ] {
            assert!(
                ob.get(field).and_then(|v| v.as_str()).is_some(),
                "{id}: missing string field `{field}`"
            );
        }

        for path_field in ["doc", "source_file", "test_file"] {
            let rel = ob[path_field].as_str().unwrap();
            assert!(
                root.join(rel).is_file(),
                "{id}: {path_field} does not exist: {rel}"
            );
        }

        let source_file = ob["source_file"].as_str().unwrap();
        let src = read(&root, source_file).unwrap();
        let symbols = ob
            .get("source_symbols")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{id}: `source_symbols` must be an array"));
        assert!(
            !symbols.is_empty(),
            "{id}: `source_symbols` must be non-empty"
        );
        for sym in symbols {
            let sym = sym.as_str().unwrap();
            assert!(
                src.contains(sym),
                "{id}: source symbol `{sym}` not found in {source_file}"
            );
        }

        let test_file = ob["test_file"].as_str().unwrap();
        let test_src = read(&root, test_file).unwrap();
        let fns = fn_names(&test_src);
        let tests = test_fn_names(&test_src);
        let verified_by = ob
            .get("verified_by")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{id}: `verified_by` must be an array"));
        assert!(
            !verified_by.is_empty(),
            "{id}: `verified_by` must be non-empty"
        );
        for name in verified_by {
            let name = name.as_str().unwrap();
            assert!(
                fns.contains(name),
                "{id}: verified_by `{name}` is not a fn in {test_file}"
            );
            assert!(
                tests.contains(name),
                "{id}: verified_by `{name}` is not a #[test] in {test_file}"
            );
            claimed
                .entry(String::from(test_file))
                .or_default()
                .insert(String::from(name));
        }
    }

    // No orphan obligations: every #[test] in a cited file must be claimed.
    for (test_file, claimed_names) in &claimed {
        let test_src = read(&root, test_file).unwrap();
        for name in test_fn_names(&test_src) {
            assert!(
                claimed_names.contains(&name),
                "{test_file}: orphan #[test] fn `{name}` is not claimed by any obligation"
            );
        }
    }
// Pure model of governance authorization and execution obligations.
// Inputs are ballots with weights fixed at cast time, stored total supply,
// configured quorum basis points, veto power/threshold, delegation edges, and
// timelock/execution outcomes. Failure behavior is modeled as terminal-state
// rejection, duplicate-vote rejection, invalid configuration, cycle/depth
// rejection, and retryable failed execution without changing state.

use super::ProposalStatus;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ModelError {
    AlreadyVoted,
    InvalidWeight,
    InvalidQuorumBps,
    QuorumNotReached,
    VotingOngoing,
    AlreadyResolved,
    NotVetoable,
    VetoPowerDisabled,
    TimelockNotExpired,
    ExecutionFailed,
    CannotDelegateToSelf,
    DelegationCyclePrevented,
    MaxDelegationDepthExceeded,
    ArithmeticOverflow,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum VoteChoice {
    For,
    Against,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Ballot {
    voter: u64,
    weight: i128,
    choice: VoteChoice,
}

fn tally(ballots: &[Ballot]) -> Result<(i128, i128), ModelError> {
    let mut votes_for = 0_i128;
    let mut votes_against = 0_i128;

    for (index, ballot) in ballots.iter().enumerate() {
        if ballot.weight < 0 {
            return Err(ModelError::InvalidWeight);
        }
        if ballots[..index]
            .iter()
            .any(|previous| previous.voter == ballot.voter)
        {
            return Err(ModelError::AlreadyVoted);
        }
        match ballot.choice {
            VoteChoice::For => {
                votes_for = votes_for
                    .checked_add(ballot.weight)
                    .ok_or(ModelError::ArithmeticOverflow)?;
            }
            VoteChoice::Against => {
                votes_against = votes_against
                    .checked_add(ballot.weight)
                    .ok_or(ModelError::ArithmeticOverflow)?;
            }
        }
    }

    Ok((votes_for, votes_against))
}

fn quorum_reached(
    total_supply: i128,
    quorum_bps: u32,
    votes_for: i128,
    votes_against: i128,
) -> Result<bool, ModelError> {
    if quorum_bps == 0 || quorum_bps > 10_000 {
        return Err(ModelError::InvalidQuorumBps);
    }
    if total_supply <= 0 {
        return Ok(false);
    }

    let cast = votes_for
        .checked_add(votes_against)
        .ok_or(ModelError::ArithmeticOverflow)?;
    let threshold = total_supply
        .checked_mul(i128::from(quorum_bps))
        .ok_or(ModelError::ArithmeticOverflow)?
        / 10_000;
    Ok(cast >= threshold)
}

fn finalize(
    status: &ProposalStatus,
    quorum: bool,
    votes_for: i128,
    votes_against: i128,
) -> Result<ProposalStatus, ModelError> {
    match status {
        ProposalStatus::Active => {
            if !quorum {
                return Ok(ProposalStatus::Rejected);
            }
            if votes_for > votes_against {
                Ok(ProposalStatus::Passed)
            } else {
                Ok(ProposalStatus::Rejected)
            }
        }
        _ => Err(ModelError::AlreadyResolved),
    }
}

fn execute(
    status: &ProposalStatus,
    timelock_elapsed: bool,
    call_succeeded: bool,
) -> Result<ProposalStatus, ModelError> {
    match status {
        ProposalStatus::Active => Err(ModelError::VotingOngoing),
        ProposalStatus::Passed => {
            if !timelock_elapsed {
                return Err(ModelError::TimelockNotExpired);
            }
            if !call_succeeded {
                return Err(ModelError::ExecutionFailed);
            }
            Ok(ProposalStatus::Executed)
        }
        _ => Err(ModelError::AlreadyResolved),
    }
}

fn apply_veto(
    status: &ProposalStatus,
    veto_enabled: bool,
    threshold_reached: bool,
) -> Result<ProposalStatus, ModelError> {
    if !veto_enabled {
        return Err(ModelError::VetoPowerDisabled);
    }
    match status {
        ProposalStatus::Active | ProposalStatus::Passed if threshold_reached => {
            Ok(ProposalStatus::Vetoed)
        }
        ProposalStatus::Vetoed => Err(ModelError::NotVetoable),
        _ => Ok((*status).clone()),
    }
}

fn resolve_delegate(
    delegations: &[(u64, u64)],
    from: u64,
    max_depth: usize,
) -> Result<u64, ModelError> {
    let mut current = from;
    let mut path = [0_u64; 16];

    for depth in 0..=max_depth {
        if depth >= path.len() {
            return Err(ModelError::MaxDelegationDepthExceeded);
        }
        if path[..depth].contains(&current) {
            return Err(ModelError::DelegationCyclePrevented);
        }
        path[depth] = current;

        let Some(next) = delegations
            .iter()
            .find_map(|(source, target)| (*source == current).then_some(*target))
        else {
            return Ok(current);
        };
        if next == current {
            return Err(ModelError::CannotDelegateToSelf);
        }
        if path[..=depth].contains(&next) {
            return Err(ModelError::DelegationCyclePrevented);
        }
        if depth == max_depth {
            return Err(ModelError::MaxDelegationDepthExceeded);
        }
        current = next;
    }

    Err(ModelError::MaxDelegationDepthExceeded)
}

fn tally_delegated(
    ballots: &[Ballot],
    delegations: &[(u64, u64)],
    max_depth: usize,
) -> Result<(i128, i128), ModelError> {
    let mut effective = [Ballot {
        voter: 0,
        weight: 0,
        choice: VoteChoice::For,
    }; 16];
    let mut count = 0_usize;

    for ballot in ballots {
        let terminal = resolve_delegate(delegations, ballot.voter, max_depth)?;
        if count >= effective.len() {
            return Err(ModelError::ArithmeticOverflow);
        }
        effective[count] = Ballot {
            voter: terminal,
            weight: ballot.weight,
            choice: ballot.choice,
        };
        count += 1;
    }

    tally(&effective[..count])
}

#[test]
fn one_address_has_exactly_one_effective_vote() {
    assert_eq!(
        tally(&[
            Ballot { voter: 1, weight: 10, choice: VoteChoice::For },
            Ballot { voter: 2, weight: 5, choice: VoteChoice::Against },
        ]),
        Ok((10, 5))
    );
    assert_eq!(
        tally(&[
            Ballot { voter: 1, weight: 10, choice: VoteChoice::For },
            Ballot { voter: 1, weight: 8, choice: VoteChoice::Against },
        ]),
        Err(ModelError::AlreadyVoted)
    );
}

#[test]
fn vote_weight_is_fixed_at_cast_time() {
    let ballots = [Ballot { voter: 7, weight: 25, choice: VoteChoice::For }];
    assert_eq!(tally(&ballots), Ok((25, 0)));
    assert_eq!(tally(&ballots), Ok((25, 0)));
}

#[test]
fn quorum_uses_stored_supply_and_basis_points() {
    assert_eq!(quorum_reached(1_000, 100, 90, 0), Ok(false));
    assert_eq!(quorum_reached(1_000, 100, 100, 0), Ok(true));
    assert_eq!(quorum_reached(1_000, 100, 20, 80), Ok(true));
    assert_eq!(quorum_reached(1_000, 0, 500, 0), Err(ModelError::InvalidQuorumBps));
    assert_eq!(quorum_reached(1_000, 10_001, 500, 0), Err(ModelError::InvalidQuorumBps));
}

#[test]
fn passing_requires_quorum_and_majority() {
    assert_eq!(
        finalize(&ProposalStatus::Active, true, 51, 49),
        Ok(ProposalStatus::Passed)
    );
    assert_eq!(
        finalize(&ProposalStatus::Active, true, 50, 50),
        Ok(ProposalStatus::Rejected)
    );
    assert_eq!(
        finalize(&ProposalStatus::Active, false, 99, 1),
        Ok(ProposalStatus::Rejected)
    );
}

#[test]
fn timelock_and_failed_execution_keep_proposal_retryable() {
    assert_eq!(
        execute(&ProposalStatus::Passed, false, true),
        Err(ModelError::TimelockNotExpired)
    );
    assert_eq!(
        execute(&ProposalStatus::Passed, true, false),
        Err(ModelError::ExecutionFailed)
    );
    assert_eq!(
        execute(&ProposalStatus::Passed, true, true),
        Ok(ProposalStatus::Executed)
    );
}

#[test]
fn veto_is_strict_and_final() {
    assert_eq!(
        apply_veto(&ProposalStatus::Active, false, true),
        Err(ModelError::VetoPowerDisabled)
    );
    let vetoed = apply_veto(&ProposalStatus::Active, true, true).unwrap();
    assert_eq!(vetoed, ProposalStatus::Vetoed);
    assert_eq!(
        execute(&vetoed, true, true),
        Err(ModelError::AlreadyResolved)
    );
    assert_eq!(
        apply_veto(&vetoed, true, true),
        Err(ModelError::NotVetoable)
    );
}

#[test]
fn delegation_resolution_handles_terminal_cycles_and_depth() {
    let delegations = [(1_u64, 2_u64), (2_u64, 3_u64)];
    assert_eq!(resolve_delegate(&delegations, 1, 4), Ok(3));
    assert_eq!(resolve_delegate(&delegations, 3, 4), Ok(3));
    assert_eq!(
        resolve_delegate(&[(1, 2), (2, 1)], 1, 4),
        Err(ModelError::DelegationCyclePrevented)
    );
    assert_eq!(
        resolve_delegate(&[(1, 1)], 1, 4),
        Err(ModelError::CannotDelegateToSelf)
    );
    assert_eq!(
        resolve_delegate(&[(1, 2), (2, 1)], 1, 1),
        Err(ModelError::DelegationCyclePrevented)
    );
}

#[test]
fn redelegation_cannot_double_count_tally() {
    let delegations = [(1_u64, 3_u64), (2_u64, 3_u64)];
    assert_eq!(
        tally_delegated(
            &[
                Ballot { voter: 1, weight: 10, choice: VoteChoice::For },
                Ballot { voter: 2, weight: 7, choice: VoteChoice::For },
            ],
            &delegations,
            4,
        ),
        Ok((17, 0))
    );
}
