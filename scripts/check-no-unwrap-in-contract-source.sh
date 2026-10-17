#!/usr/bin/env bash
# CI gate: fail if unwrap()/expect() appears in non-test contract source.
# Issue #845 — prevents new panic-prone patterns from being introduced.
#
# Excludes:
#   - Test files (test.rs, *_test.rs, tests_*/*)
#   - Examples, fuzz, and benchmark crates
#   - Lines with an explicit #[allow] justification comment
set -euo pipefail

CONTRACT_DIRS=(
  contracts/invoice_liquidity/src
  contracts/iln_governance/src
  contracts/iln_distribution/src
  contracts/insurance_pool/src
  contracts/reputation_bonus/src
)

FOUND=0
for dir in "${CONTRACT_DIRS[@]}"; do
  if [ ! -d "$dir" ]; then
    continue
  fi
  # Grep for unwrap()/expect() in .rs files, excluding test files.
  MATCHES=$(grep -rn '\.\(unwrap\|expect\)(' "$dir" \
    --include='*.rs' \
    | grep -v 'test\.rs' \
    | grep -v '_test\.rs' \
    | grep -v 'tests_' \
    | grep -v 'tests/' \
    | grep -v '#\[allow(' \
    || true)
  if [ -n "$MATCHES" ]; then
    echo "❌ Found unwrap()/expect() in non-test contract source ($dir):"
    echo "$MATCHES"
    FOUND=1
  fi
done

if [ "$FOUND" -eq 1 ]; then
  echo ""
  echo "🚫 CI gate failed: unwrap()/expect() is not allowed in contract source."
  echo "   Use typed errors (Result<T, ContractError>) instead."
  echo "   If absolutely justified, add #[allow(clippy::unwrap_used)] with a comment."
  exit 1
fi

echo "✅ No unwrap()/expect() found in non-test contract source."
