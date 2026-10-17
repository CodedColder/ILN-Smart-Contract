#!/usr/bin/env python3
"""Generate docs/access-control-matrix.generated.md from contract source (Issue #856).

Scans #[contractimpl] pub fn entry points across the five protocol crates and
infers Allowed Role(s) from require_admin / require_auth / check_rate_limit.

Usage:
  python3 scripts/generate-access-control-matrix.py          # write generated doc
  python3 scripts/generate-access-control-matrix.py --check  # exit 1 if drift
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
OUT = ROOT / "docs" / "access-control-matrix.generated.md"


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
        for fm in re.finditer(
            r"(?m)^([ \t]*/\*\*[\s\S]*?\*/\s*)?([ \t]*///.*(?:\n[ \t]*///.*)*)?\s*pub fn\s+(\w+)\s*\(",
            block,
        ):
            name = fm.group(3)
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
            doc = (fm.group(1) or "") + (fm.group(2) or "")
            results.append({"name": name, "body": body, "doc": doc})
    return results


def infer_access(body: str) -> str:
    if "require_admin" in body:
        role = "Admin"
    elif re.search(r"\.require_auth\s*\(", body) or "require_auth(" in body:
        role = "Caller (require_auth)"
    else:
        role = "Anyone"
    if re.search(r"check_rate_limit\s*\(", body):
        return f"{role}; rate-limited"
    return role


def scan():
    by_crate = {}
    for c in CRATES:
        src_dir = ROOT / "contracts" / c / "src"
        fns = []
        for f in sorted(src_dir.rglob("*.rs")):
            if re.search(r"test", f.name, re.I):
                continue
            text = f.read_text()
            for item in extract_contractimpl_fns(text):
                item["file"] = str(f.relative_to(ROOT))
                item["access"] = infer_access(item["body"])
                fns.append(item)
        seen = {}
        for item in fns:
            prev = seen.get(item["name"])
            if prev is None or item["file"].endswith("lib.rs"):
                seen[item["name"]] = item
        by_crate[c] = sorted(seen.values(), key=lambda x: x["name"])
    return by_crate


def render(by_crate) -> str:
    lines = [
        "<!-- GENERATED FILE — do not edit by hand. -->",
        "<!-- Regenerate: python3 scripts/generate-access-control-matrix.py -->",
        "<!-- CI fails if this drifts from #[contractimpl] entry points (Issue #856). -->",
        "",
        "# Access Control Matrix (Generated)",
        "",
        "Derived automatically from `#[contractimpl]` `pub fn` entry points by scanning",
        "for `require_admin`, `require_auth`, and `check_rate_limit` in each function body.",
        "",
        "Role inference is intentionally mechanical:",
        "",
        "- **Admin** — body calls `require_admin`",
        "- **Caller (require_auth)** — body calls `require_auth` / `.require_auth()` without `require_admin`",
        "- **Anyone** — neither guard present (views / permissionless helpers)",
        "- **rate-limited** — body also calls `check_rate_limit`",
        "",
        "Narrative context, audit findings, and pause semantics remain in",
        "[`access-control.md`](access-control.md). If this file and the hand-written",
        "doc disagree on a function's gate, **believe the generated matrix** and update",
        "the narrative.",
        "",
    ]
    for c, fns in by_crate.items():
        lines.append(f"## `{c}`")
        lines.append("")
        lines.append("| Instruction | Inferred gate | Source |")
        lines.append("| ----------- | ------------- | ------ |")
        for fn in fns:
            lines.append(f"| `{fn['name']}` | {fn['access']} | `{fn['file']}` |")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="fail if generated doc would change")
    args = ap.parse_args()
    by_crate = scan()
    text = render(by_crate)
    if args.check:
        if not OUT.exists():
            print(f"MISSING {OUT}", file=sys.stderr)
            sys.exit(1)
        current = OUT.read_text()
        if current != text:
            print(
                f"DRIFT: {OUT} is out of date. Run: python3 scripts/generate-access-control-matrix.py",
                file=sys.stderr,
            )
            sys.exit(1)
        print(f"OK {OUT} matches #[contractimpl] scan")
        return
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text)
    total = sum(len(v) for v in by_crate.values())
    print(f"Wrote {OUT} ({total} entry points)")


if __name__ == "__main__":
    main()
