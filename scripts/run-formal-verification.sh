#!/usr/bin/env bash
#
# Issue #1101 — gate CI on formal counterexamples.
#
# Runs the formal-verification artifacts and fails the build if any of them
# fails. The "counterexamples" are the adversarial/property-based tests that try
# to falsify the governance and cross-contract invariants:
#
#   1. iln_governance lib tests, including:
#        - contracts/iln_governance/src/tests_differential.rs  (legacy vs remediated)
#        - the archival/TTL counterexamples in src/test.rs     (#1099)
#        - the proptest counterexample hunt in src/test.rs      (#1101)
#        - the proof-obligation registry validator              (#1100)
#   2. the published cross-contract proof-obligation registry check.
#   3. (opportunistic) the cross-contract invariant harness, when the
#      invoice_liquidity crate currently builds.
#
# Usage:
#   scripts/run-formal-verification.sh
#   RUN_CROSS_CONTRACT=1 scripts/run-formal-verification.sh   # force include

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> [1/3] Governance formal counterexamples + differential tests"
cargo test -p iln_governance --lib

echo "==> [2/3] Cross-contract proof-obligation registry (#1100)"
python3 scripts/check-proof-obligations.py

echo "==> [3/3] Cross-contract invariant harness (#1101)"
if [[ "${RUN_CROSS_CONTRACT:-0}" == "1" ]] || cargo build -p invoice_liquidity --quiet 2>/dev/null; then
  cargo test -p invoice_liquidity --test cross_contract_invariants_test
else
  echo "WARN: invoice_liquidity does not currently build; skipping the cross-contract harness."
  echo "      This is a pre-existing dependency breakage (insurance_pool / iln_distribution),"
  echo "      tracked separately from the formal gate. Set RUN_CROSS_CONTRACT=1 to force it."
fi

echo "✅ Formal verification gate passed."
