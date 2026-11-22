#!/bin/bash
# Deploy all ILN contracts to Stellar testnet and emit a summary file.

set -euo pipefail

# Load .env if present (ignores comments and lines with invalid bash identifiers)
if [[ -f .env ]]; then
  while IFS= read -r line; do
    key="${line%%=*}"
    value="${line#*=}"
    export "$key"="$value"
  done < <(grep -E '^[A-Za-z_][A-Za-z0-9_]*=' .env)
fi

NETWORK="testnet"
SOURCE="deployer"
SUMMARY_FILE="deploy-summary.json"
ENV_FILE=".contracts-${NETWORK}.env"

if [[ -z "${STELLAR_TESTNET_DEPLOYER_SECRET:-}" ]]; then
  echo "Missing STELLAR_TESTNET_DEPLOYER_SECRET in environment." >&2
  exit 1
fi

# Ensure network config exists.
if ! stellar network ls | grep -q "^${NETWORK}$"; then
  stellar network add ${NETWORK} \
    --rpc-url https://soroban-testnet.stellar.org \
    --network-passphrase "Test SDF Network ; September 2015"
fi

# Ensure deployer key exists.
if ! stellar keys address "${SOURCE}" &> /dev/null; then
  stellar keys add "${SOURCE}" --secret-key <<< "${STELLAR_TESTNET_DEPLOYER_SECRET}"
fi

# Build optimized WASM.
cargo build --target wasm32v1-none --release

# Strip the contractspecv0 custom section from WASM files that exceed
# the Stellar on-chain limit (128 KB). The spec is only needed for
# off-chain client generation.
strip_spec() {
  python3 -c "
import sys
with open(sys.argv[1], 'rb') as f:
    data = bytearray(f.read())
i = 8
out = data[:i]
while i < len(data):
    sec_id = data[i]; j = i + 1
    size = 0; shift = 0
    while j < len(data):
        byte = data[j]; size |= (byte & 0x7F) << shift; j += 1; shift += 7
        if byte < 0x80: break
    sec_end = j + size
    if sec_id == 0:
        k = j; name_len = 0; shift = 0
        while k < sec_end:
            byte = data[k]; name_len |= (byte & 0x7F) << shift; k += 1; shift += 7
            if byte < 0x80: break
        name = data[k:k+name_len].decode('utf-8', errors='replace')
        if name == 'contractspecv0':
            print(f'  Stripped {name} ({sec_end - i} bytes)')
            i = sec_end; continue
    out.extend(data[i:sec_end]); i = sec_end
with open(sys.argv[1], 'wb') as f: f.write(out)
" "$1"
}

CONTRACT_NAMES=(
  invoice_liquidity
  iln_governance
  iln_distribution
  reputation_bonus
)

declare -A CONTRACTS=(
  ["invoice_liquidity"]="target/wasm32v1-none/release/invoice_liquidity.wasm"
  ["iln_governance"]="target/wasm32v1-none/release/iln_governance.wasm"
  ["iln_distribution"]="target/wasm32v1-none/release/iln_distribution.wasm"
  ["reputation_bonus"]="target/wasm32v1-none/release/reputation_bonus.wasm"
)

declare -A CONTRACT_IDS

for contract_name in "${CONTRACT_NAMES[@]}"; do
  wasm_path="${CONTRACTS[$contract_name]}"

  if [[ ! -f "${wasm_path}" ]]; then
    echo "WASM not found: ${wasm_path}" >&2
    exit 1
  fi

  wasm_size=$(wc -c < "${wasm_path}")
  if [[ "${wasm_size}" -gt 131072 ]]; then
    echo "${contract_name}: ${wasm_size} bytes exceeds 128 KB limit, stripping spec..."
    strip_spec "${wasm_path}"
    wasm_size=$(wc -c < "${wasm_path}")
  fi
  if [[ "${wasm_size}" -gt 131072 ]]; then
    echo "${contract_name}: still ${wasm_size} bytes, running wasm-opt..."
    wasm-opt -Oz --strip-debug --strip-producers --strip-target-features \
      --strip-toolchain-annotations --zero-filled-memory \
      -o "${wasm_path}.tmp" "${wasm_path}"
    mv "${wasm_path}.tmp" "${wasm_path}"
  fi

  upload_output=$(stellar contract upload \
    --network "${NETWORK}" \
    --source "${SOURCE}" \
    --wasm "${wasm_path}" 2>&1)

  wasm_hash=$(echo "${upload_output}" | grep -oP '\b[a-f0-9]{64}\b' | head -1 || true)
  if [[ -z "${wasm_hash}" ]]; then
    echo "Failed to upload WASM for ${contract_name}" >&2
    echo "${upload_output}" >&2
    exit 1
  fi

  deploy_output=$(stellar contract deploy \
    --network "${NETWORK}" \
    --source "${SOURCE}" \
    --wasm-hash "${wasm_hash}" 2>&1)

  contract_id=$(echo "${deploy_output}" | grep -oP '[A-Z0-9]{56}' | tail -1 || true)
  if [[ -z "${contract_id}" ]]; then
    echo "Failed to deploy ${contract_name}" >&2
    echo "${deploy_output}" >&2
    exit 1
  fi

  CONTRACT_IDS[${contract_name}]="${contract_id}"
  echo "${contract_name}=${contract_id}"
done

cat > "${ENV_FILE}" <<EOF
# Contract IDs for ${NETWORK} network
# Generated: $(date -u +"%Y-%m-%dT%H:%M:%SZ")

INVOICE_LIQUIDITY_ID=${CONTRACT_IDS[invoice_liquidity]}
ILN_GOVERNANCE_ID=${CONTRACT_IDS[iln_governance]}
ILN_DISTRIBUTION_ID=${CONTRACT_IDS[iln_distribution]}
REPUTATION_BONUS_ID=${CONTRACT_IDS[reputation_bonus]}
NETWORK=${NETWORK}
SOURCE=${SOURCE}
EOF

# ── Initialize invoice_liquidity (Issue #1162) ────────────────────────────────
# `stellar contract deploy` only installs the instance; initialization is a
# separate, *authenticated* transaction. Leaving it to a later public window
# would let an arbitrary first caller become admin (and thereby control fees,
# tokens, oracle settings, pause state, upgrades and migrations), so we
# initialize here, signed by the deployer.
#
#   INVOICE_LIQUIDITY_ADMIN  admin address (default: the deployer)
#   TESTNET_XLM_SAC          native XLM SAC (default: known testnet SAC)
#   TESTNET_USDC_SAC         USDC SAC   (default: native XLM SAC)
#   TESTNET_EURC_SAC         EURC SAC   (default: native XLM SAC)
#
# If INVOICE_LIQUIDITY_ADMIN is an identity other than the deployer, the
# deployer cannot authorize on its behalf: run the `initialize` step yourself
# with `--source <admin-key>` (see docs/initialization-hardening.md).
INVOICE_LIQUIDITY_ID="${CONTRACT_IDS[invoice_liquidity]}"
DEPLOYER_ADDRESS="$(stellar keys address "${SOURCE}")"
INIT_ADMIN="${INVOICE_LIQUIDITY_ADMIN:-${DEPLOYER_ADDRESS}}"
XLM_SAC="${TESTNET_XLM_SAC:-CDLZ472EC4UB7SA74XCHWYVEGERSIJU224RLUXEDCXTCW6U537BC7D37}"
USDC_SAC="${TESTNET_USDC_SAC:-$XLM_SAC}"
EURC_SAC="${TESTNET_EURC_SAC:-$XLM_SAC}"

if [[ "${INIT_ADMIN}" != "${DEPLOYER_ADDRESS}" ]]; then
  echo "⚠️  INVOICE_LIQUIDITY_ADMIN (${INIT_ADMIN}) differs from the deployer."
  echo "   The deployer cannot authorize on its behalf — run the initializer with"
  echo "   the admin key instead:"
  echo "     stellar contract invoke --network ${NETWORK} --source <admin-key> \\"
  echo "       --id ${INVOICE_LIQUIDITY_ID} -- initialize \\"
  echo "       --admin ${INIT_ADMIN} --usdc_token ${USDC_SAC} \\"
  echo "       --eurc_token ${EURC_SAC} --xlm_token ${XLM_SAC}"
else
  echo "Initializing invoice_liquidity (${INVOICE_LIQUIDITY_ID})..."
  set +e
  init_output=$(stellar contract invoke \
    --network "${NETWORK}" \
    --source "${SOURCE}" \
    --id "${INVOICE_LIQUIDITY_ID}" \
    -- initialize \
    --admin "${INIT_ADMIN}" \
    --usdc_token "${USDC_SAC}" \
    --eurc_token "${EURC_SAC}" \
    --xlm_token "${XLM_SAC}" 2>&1)
  init_status=$?
  set -e
  if [[ ${init_status} -ne 0 ]]; then
    if echo "${init_output}" | grep -qiE 'AlreadyInitialized|already initialized'; then
      echo "invoice_liquidity already initialized — skipping."
    else
      echo "invoice_liquidity initialization failed:" >&2
      echo "${init_output}" >&2
      exit 1
    fi
  fi
fi

cat > "${SUMMARY_FILE}" <<EOF
{
  "network": "${NETWORK}",
  "invoice_liquidity": "${CONTRACT_IDS[invoice_liquidity]}",
  "iln_governance": "${CONTRACT_IDS[iln_governance]}",
  "iln_distribution": "${CONTRACT_IDS[iln_distribution]}",
  "reputation_bonus": "${CONTRACT_IDS[reputation_bonus]}"
}
EOF

echo "Summary written to ${SUMMARY_FILE}"
