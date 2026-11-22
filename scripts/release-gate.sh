#!/usr/bin/env bash
# Release gate for security-sensitive contract changes (Issue #1263).
#
# Runs every verification a contract release depends on and exits non-zero
# if ANY of them fails. Unlike `make lint` / `make test`, which a release
# manager could run piecemeal (or skip), this is one command with one
# pass/fail answer and a summary table of what ran.
#
# Steps (in order; each one runs even if an earlier one failed, so a single
# run reports every problem):
#   fmt            cargo fmt --all -- --check
#   clippy         cargo clippy --all-targets -- -D warnings
#   no-unwrap      scripts/check-no-unwrap-in-contract-source.sh (#845)
#   test           cargo test (whole workspace, native target)
#   wasm-build     cargo build --target wasm32v1-none --release
#   wasm-size      scripts/check_wasm_size.sh (#1066)
#   access-matrix  scripts/generate-access-control-matrix.py --check (#856)
#   public-docs    scripts/check-contract-public-docs.py (#857)
#   abi-drift      regenerates docs/contract-abi.md + docs/contract-spec.json
#                  and fails if either differs from the committed copy
#   deploy-dry-run scripts/deploy-mainnet.sh --dry-run (no transactions)
#
# Scope: the gate only *requires* these steps when the release range touches
# contract code (contracts/**, Cargo.toml, Cargo.lock). The range is
# RELEASE_GATE_BASE...HEAD (default origin/main). If the base ref cannot be
# resolved the gate fails closed and treats the release as security-sensitive.
#
# Environment:
#   RELEASE_GATE_BASE   base ref for the change scan (default: origin/main)
#   RELEASE_GATE_FORCE  =1 runs every step even with no contract changes
#   RELEASE_GATE_SKIP   space-separated step names to skip. Skipped steps are
#                       printed as SKIPPED in the summary and make the gate
#                       exit 2 (not 0), so a skip can never look like a pass.
#
# Exit codes: 0 all steps passed (or no contract changes), 1 a step failed,
#             2 no step failed but at least one was skipped.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

BASE="${RELEASE_GATE_BASE:-origin/main}"
FORCE="${RELEASE_GATE_FORCE:-0}"
SKIP=" ${RELEASE_GATE_SKIP:-} "

SENSITIVE_PATHS=(contracts Cargo.toml Cargo.lock)

echo "=== ILN contract release gate (#1263) ==="

if [[ "$FORCE" != "1" ]]; then
  if git rev-parse --verify --quiet "$BASE" >/dev/null; then
    changed="$(git diff --name-only "$BASE"...HEAD -- "${SENSITIVE_PATHS[@]}")"
    if [[ -z "$changed" ]]; then
      echo "No contract changes in $BASE...HEAD; gate not required."
      exit 0
    fi
    echo "Security-sensitive changes in $BASE...HEAD:"
    echo "$changed" | sed 's/^/  /'
  else
    echo "Base ref '$BASE' not found; failing closed and running the full gate."
  fi
fi

declare -a NAMES=()
declare -a RESULTS=()
FAILED=0
SKIPPED=0

run_step() {
  local name="$1"
  shift
  NAMES+=("$name")
  if [[ "$SKIP" == *" $name "* ]]; then
    echo ""
    echo "--- [$name] SKIPPED (RELEASE_GATE_SKIP)"
    RESULTS+=("SKIPPED")
    SKIPPED=1
    return
  fi
  echo ""
  echo "--- [$name] $*"
  if "$@"; then
    RESULTS+=("PASS")
  else
    RESULTS+=("FAIL")
    FAILED=1
  fi
}

abi_drift() {
  npx --yes tsx scripts/gen-abi.ts &&
    make --no-print-directory spec &&
    git diff --exit-code -- docs/contract-abi.md docs/contract-spec.json
}

run_step fmt cargo fmt --all -- --check
run_step clippy cargo clippy --all-targets -- -D warnings
run_step no-unwrap bash scripts/check-no-unwrap-in-contract-source.sh
run_step test cargo test
run_step wasm-build cargo build --target wasm32v1-none --release
run_step wasm-size bash scripts/check_wasm_size.sh
run_step access-matrix python3 scripts/generate-access-control-matrix.py --check
run_step public-docs python3 scripts/check-contract-public-docs.py
run_step abi-drift abi_drift
run_step deploy-dry-run bash scripts/deploy-mainnet.sh --dry-run

echo ""
echo "| Step | Result |"
echo "| ---- | ------ |"
for i in "${!NAMES[@]}"; do
  echo "| ${NAMES[$i]} | ${RESULTS[$i]} |"
done

if [[ "$FAILED" -ne 0 ]]; then
  echo ""
  echo "Release gate FAILED: do not tag or deploy this revision."
  exit 1
fi
if [[ "$SKIPPED" -ne 0 ]]; then
  echo ""
  echo "Release gate INCOMPLETE: no step failed, but skipped steps were not verified."
  exit 2
fi
echo ""
echo "Release gate PASSED."
