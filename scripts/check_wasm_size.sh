#!/usr/bin/env bash
# Issue #1066: absolute WASM binary size threshold.
#
# docs/wasm-size.md previously only documented *guidance* ("typically 10KB
# to 80KB") with a manual `wc -c` verification step -- nothing enforced it,
# and nothing failed a build or PR if a contract silently grew past a safe
# size. This script gives that guidance a real, CI-usable gate: it builds
# (if needed) each contract's optimized WASM and fails (non-zero exit) if
# any exceeds MAX_WASM_SIZE_BYTES.
#
# The default threshold below (131072 bytes / 128 KiB) matches
# soroban-sdk 27.0.6's bundled snapshot of Stellar Mainnet's contract code
# size limit (NetworkInvocationResourceLimits::mainnet().max_contract_code_entry_size_bytes,
# dated 2026-07-10) -- see docs/wasm-size.md for the full derivation. That
# snapshot isn't pulled dynamically, so it can drift from the live network
# between SDK releases; reconcile against `stellar network settings
# --network mainnet` (or https://lab.stellar.org/network-limits) before
# treating it as authoritative for a mainnet-blocking decision, and prefer
# upgrading soroban-sdk over hand-editing this default if it's ever found
# to be stale.
#
# Issue #1068: also prints a near-limit WARNING (no failure) once a
# contract's size passes NEAR_LIMIT_WARNING_RATIO of MAX_WASM_SIZE_BYTES --
# the WASM-size counterpart to the near-limit monitoring in
# contracts/invoice_liquidity/src/tests_benchmarks.rs. Surfaces drift
# toward the ceiling before it ever actually fails this script.
#
# Usage:
#   bash scripts/check_wasm_size.sh                    # check all contracts below
#   bash scripts/check_wasm_size.sh invoice_liquidity   # check one contract by crate name
#   MAX_WASM_SIZE_BYTES=262144 bash scripts/check_wasm_size.sh   # override the threshold

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_TRIPLE="wasm32v1-none"
WASM_DIR="$ROOT/target/$TARGET_TRIPLE/release"

# 128 KiB: roughly 2x the top of the "typical" range docs/wasm-size.md
# records (10-80 KB), well below any historically-known Soroban contract
# code size limit, chosen to leave headroom for legitimate growth while
# still catching a genuine regression.
MAX_WASM_SIZE_BYTES="${MAX_WASM_SIZE_BYTES:-131072}"

# Issue #1068: fraction of MAX_WASM_SIZE_BYTES at which a build counts as
# "near the limit" -- mirrors NEAR_LIMIT_WARNING_RATIO in
# tests_benchmarks.rs. Expressed as a percentage so the comparison below
# can stay in plain integer bash arithmetic.
NEAR_LIMIT_WARNING_PERCENT="${NEAR_LIMIT_WARNING_PERCENT:-70}"
WARN_THRESHOLD_BYTES=$(( MAX_WASM_SIZE_BYTES * NEAR_LIMIT_WARNING_PERCENT / 100 ))

DEFAULT_CONTRACTS=(invoice_liquidity iln_governance iln_distribution reputation_bonus insurance_pool)

if [[ $# -gt 0 ]]; then
  CONTRACTS=("$@")
else
  CONTRACTS=("${DEFAULT_CONTRACTS[@]}")
fi

FAILED=0

printf "%-20s %12s %12s\n" "Contract" "Size (bytes)" "Limit (bytes)"
printf "%-20s %12s %12s\n" "--------" "------------" "-------------"

for crate in "${CONTRACTS[@]}"; do
  wasm_path="$WASM_DIR/$crate.wasm"

  if [[ ! -f "$wasm_path" ]]; then
    echo "Building $crate ($TARGET_TRIPLE, release)..." >&2
    (cd "$ROOT" && cargo build --target "$TARGET_TRIPLE" --release -p "$crate")
  fi

  if [[ ! -f "$wasm_path" ]]; then
    echo "::error::$crate: expected WASM artifact not found at $wasm_path after build"
    FAILED=1
    continue
  fi

  size_bytes=$(stat -c%s "$wasm_path" 2>/dev/null || wc -c <"$wasm_path")
  printf "%-20s %12s %12s\n" "$crate" "$size_bytes" "$MAX_WASM_SIZE_BYTES"

  if (( size_bytes > MAX_WASM_SIZE_BYTES )); then
    echo "::error::$crate: WASM size $size_bytes bytes exceeds MAX_WASM_SIZE_BYTES=$MAX_WASM_SIZE_BYTES"
    FAILED=1
  elif (( size_bytes > WARN_THRESHOLD_BYTES )); then
    echo "::warning::$crate: WASM size $size_bytes bytes is past ${NEAR_LIMIT_WARNING_PERCENT}% of MAX_WASM_SIZE_BYTES=$MAX_WASM_SIZE_BYTES (warn threshold: $WARN_THRESHOLD_BYTES)"
  fi
done

if [[ "$FAILED" -ne 0 ]]; then
  echo "WASM size check FAILED."
  exit 1
fi

echo "WASM size check passed for: ${CONTRACTS[*]}"
