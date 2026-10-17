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
}
