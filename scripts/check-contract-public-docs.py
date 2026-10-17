#!/usr/bin/env python3
"""Fail CI when #[contractimpl] pub fns lack required doc sections (Issue #857).

Required on every contract entry point:
  - a doc comment (`///` or `/** */`)
  - an `Access:` line (project convention)

Additionally, mutating entry points (inferred Admin / require_auth) must include
`# Arguments`, `# Returns`, and `# Errors` sections (rustdoc headers).

Usage:
  python3 scripts/check-contract-public-docs.py
  python3 scripts/check-contract-public-docs.py --fix-access   # insert missing Access: lines
"""
from __future__ import annotations

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
CRATES = [
    "invoice_liquidity",
    "iln_governance",
    "iln_distribution",
    "insurance_pool",
    "reputation_bonus",
]


def extract_contractimpl_fns(src: str):
    results = []
    for m in re.finditer(r"#\[contractimpl\]", src):
        rest = src[m.end() :]
        impl_m = re.search(r"\bimpl\b[^{]*\{", rest)
        if not impl_m:
            continue
        brace_start = m.end() + impl_m.end() - 1
        depth = 0
        i = brace_start
        block = None
        while i < len(src):
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
                if depth == 0:
                    block = src[brace_start + 1 : i]
                    break
            i += 1
        if block is None:
            continue
        base = brace_start + 1
        for fm in re.finditer(
            r"(?m)^([ \t]*/\*\*[\s\S]*?\*/\s*)?([ \t]*///.*(?:\n[ \t]*///.*)*)?\s*(pub fn\s+(\w+)\s*\()",
            block,
        ):
            name = fm.group(4)
            doc = (fm.group(1) or "") + (fm.group(2) or "")
            abs_pub = base + fm.start(3)
            # body
            fn_sig_end = fm.end()
            depth_p = 1
            k = fn_sig_end
            while k < len(block) and depth_p > 0:
                if block[k] == "(":
                    depth_p += 1
                elif block[k] == ")":
                    depth_p -= 1
                k += 1
            brace = block.find("{", k)
            body = ""
            if brace >= 0:
                d = 0
                p = brace
                while p < len(block):
                    if block[p] == "{":
                        d += 1
                    elif block[p] == "}":
                        d -= 1
                        if d == 0:
                            body = block[brace + 1 : p]
                            break
                    p += 1
            results.append(
                {
                    "name": name,
                    "doc": doc,
                    "body": body,
                    "abs_pub": abs_pub,
                }
            )
    return results


def infer_access(body: str) -> str:
    if "require_admin" in body:
        role = "Admin only"
    elif re.search(r"\.require_auth\s*\(", body) or "require_auth(" in body:
        role = "Caller (require_auth)"
    else:
        role = "Anyone"
    return role


def is_mutating(body: str) -> bool:
    return "require_admin" in body or bool(
        re.search(r"\.require_auth\s*\(|require_auth\(", body)
    )


def check_fn(item):
    problems = []
    doc = item["doc"]
    if not doc.strip():
        problems.append("missing doc comment")
    if not re.search(r"Access\s*:", doc):
        problems.append("missing Access:")
    if is_mutating(item["body"]):
        if not re.search(r"# Arguments\b", doc):
            problems.append("missing # Arguments")
        if not re.search(r"# Returns\b", doc):
            problems.append("missing # Returns")
        if not re.search(r"# Errors\b", doc):
            problems.append("missing # Errors")
    return problems


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--fix-access",
        action="store_true",
        help="insert missing `/// Access:` lines (does not invent Arguments/Returns/Errors)",
    )
    ap.add_argument(
        "--allow-missing-sections",
        action="store_true",
        help="only require doc + Access: (used while backfilling # Arguments/# Returns/# Errors)",
    )
    args = ap.parse_args()

    failures = []
    fixed_files = 0
    for c in CRATES:
        src_dir = ROOT / "contracts" / c / "src"
        for path in sorted(src_dir.rglob("*.rs")):
            if re.search(r"test", path.name, re.I):
                continue
            text = path.read_text()
            items = extract_contractimpl_fns(text)
            # fix Access from the end so indices stay valid
            edits = []
            for item in items:
                problems = check_fn(item)
                if args.allow_missing_sections:
                    problems = [p for p in problems if p in ("missing doc comment", "missing Access:")]
                if not problems:
                    continue
                if args.fix_access and problems == ["missing Access:"] or (
                    args.fix_access and "missing Access:" in problems and "missing doc comment" not in problems
                ):
                    access = infer_access(item["body"])
                    # insert Access line immediately before `pub fn`
                    indent = re.match(r"[ \t]*", text[item["abs_pub"]:]).group(0)
                    insertion = f"{indent}/// Access: {access}\n"
                    edits.append((item["abs_pub"], insertion, item["name"]))
                    problems = [p for p in problems if p != "missing Access:"]
                for p in problems:
                    failures.append(f"{path.relative_to(ROOT)}::{item['name']}: {p}")
            if edits and args.fix_access:
                for pos, insertion, _name in sorted(edits, key=lambda x: -x[0]):
                    text = text[:pos] + insertion + text[pos:]
                path.write_text(text)
                fixed_files += 1

    if args.fix_access:
        print(f"Updated Access: docs in {fixed_files} file(s)")
    if failures:
        print(f"{len(failures)} public entry point doc gap(s):", file=sys.stderr)
        for f in failures[:200]:
            print(f"  {f}", file=sys.stderr)
        if len(failures) > 200:
            print(f"  ... {len(failures) - 200} more", file=sys.stderr)
        sys.exit(1)
    print("OK: all #[contractimpl] entry points meet the public-doc gate")


if __name__ == "__main__":
    main()
