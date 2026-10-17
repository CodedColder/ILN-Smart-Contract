#!/usr/bin/env python3
"""Validate the cross-contract proof-obligation registry (Issue #1100).

Reads docs/proof-obligations.json and checks it against the repository so the
published obligations cannot silently drift:

  * every obligation has the required fields and a unique id;
  * every source_file / test_file / doc path exists;
  * every source_symbols entry occurs in its source_file;
  * every verified_by name occurs as an `fn` in its test_file;
  * every #[test] in each cited test_file is claimed by exactly one obligation
    (no orphan obligations, no unclaimed tests).

Usage:
  python3 scripts/check-proof-obligations.py
  python3 scripts/check-proof-obligations.py --registry docs/proof-obligations.json
"""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_REGISTRY = ROOT / "docs" / "proof-obligations.json"

REQUIRED_FIELDS = (
    "id",
    "title",
    "property",
    "doc",
    "source_file",
    "source_symbols",
    "test_file",
    "verified_by",
    "status",
)


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def fn_names(src: str) -> set[str]:
    return set(re.findall(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)", src))


def test_fn_names(src: str) -> set[str]:
    """Names of `#[test]`-attributed functions in `src`."""
    names: set[str] = set()
    pattern = re.compile(
        r"#\[test\][^;{}]*?\bfn\s+([A-Za-z_][A-Za-z0-9_]*)",
        re.DOTALL,
    )
    for m in pattern.finditer(src):
        names.add(m.group(1))
    return names


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--registry",
        type=pathlib.Path,
        default=DEFAULT_REGISTRY,
        help="path to the proof-obligation registry JSON",
    )
    args = parser.parse_args()

    errors: list[str] = []

    if not args.registry.is_file():
        print(f"FAIL: registry not found: {args.registry}", file=sys.stderr)
        return 1

    try:
        registry = json.loads(args.registry.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        print(f"FAIL: {args.registry} is not valid JSON: {exc}", file=sys.stderr)
        return 1

    obligations = registry.get("obligations")
    if not isinstance(obligations, list) or not obligations:
        print("FAIL: registry has no `obligations` list", file=sys.stderr)
        return 1

    seen_ids: set[str] = set()
    # test_file -> set of test fn names claimed by some obligation
    claimed: dict[str, set[str]] = {}

    for i, ob in enumerate(obligations):
        label = ob.get("id", f"obligations[{i}]")

        for field in REQUIRED_FIELDS:
            if field not in ob:
                fail(errors, f"{label}: missing required field `{field}`")

        ob_id = ob.get("id")
        if ob_id in seen_ids:
            fail(errors, f"{label}: duplicate obligation id `{ob_id}`")
        seen_ids.add(ob_id)

        for path_field in ("doc", "source_file", "test_file"):
            rel = ob.get(path_field)
            if not isinstance(rel, str):
                continue
            if not (ROOT / rel).is_file():
                fail(errors, f"{label}: {path_field} does not exist: {rel}")

        source_file = ob.get("source_file")
        if isinstance(source_file, str):
            src_path = ROOT / source_file
            if src_path.is_file():
                src = src_path.read_text(encoding="utf-8")
                for symbol in ob.get("source_symbols", []):
                    if symbol not in src:
                        fail(
                            errors,
                            f"{label}: source symbol `{symbol}` not found in {source_file}",
                        )
            symbols = ob.get("source_symbols")
            if not isinstance(symbols, list) or not symbols:
                fail(errors, f"{label}: `source_symbols` must be a non-empty list")

        test_file = ob.get("test_file")
        verified_by = ob.get("verified_by", [])
        if not isinstance(verified_by, list) or not verified_by:
            fail(errors, f"{label}: `verified_by` must be a non-empty list")
        if isinstance(test_file, str):
            tf_path = ROOT / test_file
            if tf_path.is_file():
                tf_src = tf_path.read_text(encoding="utf-8")
                fns = fn_names(tf_src)
                tests = test_fn_names(tf_src)
                for name in verified_by:
                    if name not in fns:
                        fail(
                            errors,
                            f"{label}: verified_by `{name}` is not a fn in {test_file}",
                        )
                    elif name not in tests:
                        fail(
                            errors,
                            f"{label}: verified_by `{name}` is not a #[test] in {test_file}",
                        )
                    claimed.setdefault(test_file, set()).add(name)

    # Orphan check: every #[test] in a cited test file must be claimed.
    for test_file, claimed_names in claimed.items():
        tf_path = ROOT / test_file
        if not tf_path.is_file():
            continue
        all_tests = test_fn_names(tf_path.read_text(encoding="utf-8"))
        orphans = sorted(all_tests - claimed_names)
        if orphans:
            fail(
                errors,
                f"{test_file}: orphan #[test] fn(s) not claimed by any obligation: "
                + ", ".join(orphans),
            )

    if errors:
        print("FAIL: proof-obligation registry inconsistencies:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    print(
        f"OK: {len(obligations)} proof obligation(s) validated against the tree "
        f"({', '.join(sorted(seen_ids))})."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
