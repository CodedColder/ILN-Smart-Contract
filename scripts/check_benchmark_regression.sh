#!/usr/bin/env bash
# Compare benchmark output against baseline and warn on >10% regression.
# Relative-drift check is still warning-only, as before (CI never fails on
# it alone; prints ::warning:: lines for GitHub Actions).
#
# Issue #1066: also enforces an ABSOLUTE instruction/memory ceiling
# (MAX_CPU_INSTRUCTIONS / MAX_MEMORY_BYTES, independent of the baseline),
# mirroring the Rust-side assertions in
# contracts/invoice_liquidity/src/tests_benchmarks.rs. Unlike the relative
# check, breaching the absolute ceiling makes this script exit non-zero --
# a hard circuit breaker for a regression so large it approaches the
# network's real resource limits, on top of the existing soft 10%-drift
# early warning. See that file's module doc comment and docs/benchmarks.md
# for why these specific numbers were chosen and the caveat that they must
# be reconciled against the live network config if those protocol-governed
# limits ever change.
#
# Issue #1068: also surfaces (as ::warning::, never failing the script)
# the near-limit monitoring tier that tests_benchmarks.rs computes and
# prints as `NEAR_LIMIT_WARNING` lines -- an invocation already past 70% of
# the absolute ceiling, even though it's still passing. This is a third,
# strictly softer tier below #1066's hard failure: relative-drift warning
# -> near-limit warning -> absolute-ceiling failure.
#
# Usage:
#   bash scripts/check_benchmark_regression.sh                       # check all contracts below
#   bash scripts/check_benchmark_regression.sh <contract-dir>:<baseline-file>   # check one

set -uo pipefail

REGRESSION_THRESHOLD="${BENCHMARK_REGRESSION_THRESHOLD:-10}"
MAX_CPU_INSTRUCTIONS="${MAX_CPU_INSTRUCTIONS:-100000000}"
MAX_MEMORY_BYTES="${MAX_MEMORY_BYTES:-41943040}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OVERALL_FAILED=0

# "<contract-dir>:<baseline-file>" pairs to check when no argument is given.
DEFAULT_TARGETS=(
  "contracts/invoice_liquidity:contracts/invoice_liquidity/benchmarks/baseline.json"
  "contracts/iln_governance:contracts/iln_governance/benchmarks/baseline.json"
)

if [[ $# -gt 0 ]]; then
  TARGETS=("$1")
else
  TARGETS=("${DEFAULT_TARGETS[@]}")
fi

for target in "${TARGETS[@]}"; do
  CONTRACT_DIR="${target%%:*}"
  BASELINE_FILE="${target##*:}"

  if [[ ! -f "$ROOT/$BASELINE_FILE" ]]; then
    echo "::warning::Baseline file not found: $BASELINE_FILE"
    continue
  fi

  echo "Running benchmark tests for $CONTRACT_DIR..."
  BENCHMARK_OUTPUT="$(cd "$ROOT/$CONTRACT_DIR" && cargo test --target x86_64-unknown-linux-gnu benchmark -- --nocapture 2>&1)" || true

  export BENCHMARK_OUTPUT
  export BASELINE_FILE="$ROOT/$BASELINE_FILE"
  export REGRESSION_THRESHOLD
  export MAX_CPU_INSTRUCTIONS
  export MAX_MEMORY_BYTES
  export CONTRACT_DIR

  python3 <<'PY'
import json
import os
import re
import sys

contract_dir = os.environ["CONTRACT_DIR"]
baseline_path = os.environ["BASELINE_FILE"]
threshold_pct = float(os.environ["REGRESSION_THRESHOLD"])
max_cpu = int(os.environ["MAX_CPU_INSTRUCTIONS"])
max_mem = int(os.environ["MAX_MEMORY_BYTES"])
output = os.environ.get("BENCHMARK_OUTPUT", "")

with open(baseline_path) as f:
    baseline = json.load(f)["benchmarks"]

pattern = re.compile(r"BENCHMARK\s+(\w+)\s+cpu=(\d+)\s+mem=(\d+)")
measured = {m.group(1): {"cpu": int(m.group(2)), "mem": int(m.group(3))} for m in pattern.finditer(output)}

# Issue #1068: pass through the Rust side's own near-limit warnings
# (tests_benchmarks.rs's `assert_within_resource_budget`) as GitHub Actions
# annotations. The shell/CI layer doesn't recompute the 70% threshold --
# the Rust test already measured and decided this; this just makes that
# decision visible in CI output too, not only in local `--nocapture` runs.
near_limit_pattern = re.compile(
    r"NEAR_LIMIT_WARNING\s+(\w+)\s+resource=(\w+)\s+measured=(\d+)\s+warn_threshold=(\d+)\s+max=(\d+)"
)
for m in near_limit_pattern.finditer(output):
    name, resource, measured_val, warn_threshold, max_val = m.groups()
    print(
        f"::warning::{contract_dir}::{name} {resource} usage {measured_val} is past the "
        f"near-limit warning threshold {warn_threshold} (absolute ceiling {max_val})"
    )

if not measured:
    print(f"::warning::No BENCHMARK lines found in test output for {contract_dir}")
    raise SystemExit(0)

print(f"\n### {contract_dir}")
print("| Function | CPU (measured) | CPU (baseline) | Mem (measured) | Mem (baseline) |")
print("| -------- | -------------- | -------------- | -------------- | -------------- |")

absolute_ceiling_breached = False

for name, base in baseline.items():
    current = measured.get(name)
    if not current:
        print(f"::warning::Missing benchmark measurement for {contract_dir}::{name}")
        continue

    cpu_base, mem_base = base["cpu"], base["mem"]
    cpu_cur, mem_cur = current["cpu"], current["mem"]
    cpu_pct = ((cpu_cur - cpu_base) / cpu_base) * 100 if cpu_base else 0
    mem_pct = ((mem_cur - mem_base) / mem_base) * 100 if mem_base else 0

    print(f"| {name} | {cpu_cur} | {cpu_base} | {mem_cur} | {mem_base} |")

    # Relative drift from the recorded baseline: warning-only, as before.
    if cpu_pct > threshold_pct:
        print(
            f"::warning::Benchmark regression: {contract_dir}::{name} CPU instructions "
            f"increased {cpu_pct:.1f}% ({cpu_cur} vs baseline {cpu_base})"
        )
    if mem_pct > threshold_pct:
        print(
            f"::warning::Benchmark regression: {contract_dir}::{name} memory bytes "
            f"increased {mem_pct:.1f}% ({mem_cur} vs baseline {mem_base})"
        )

    # Issue #1066: absolute ceiling, independent of the baseline -- this
    # one fails the script (non-zero exit), not just a warning.
    if cpu_cur > max_cpu:
        print(
            f"::error::{contract_dir}::{name} CPU instructions {cpu_cur} exceed the "
            f"absolute ceiling MAX_CPU_INSTRUCTIONS={max_cpu}"
        )
        absolute_ceiling_breached = True
    if mem_cur > max_mem:
        print(
            f"::error::{contract_dir}::{name} memory bytes {mem_cur} exceed the "
            f"absolute ceiling MAX_MEMORY_BYTES={max_mem}"
        )
        absolute_ceiling_breached = True

if absolute_ceiling_breached:
    sys.exit(1)
PY
  if [[ $? -ne 0 ]]; then
    OVERALL_FAILED=1
  fi
done

if [[ "$OVERALL_FAILED" -ne 0 ]]; then
  echo "Benchmark check FAILED: at least one function exceeded its absolute resource ceiling."
  exit 1
fi
