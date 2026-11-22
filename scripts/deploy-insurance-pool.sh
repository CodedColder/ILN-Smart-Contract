#!/bin/bash
# Deploy and initialise the insurance_pool contract to a local Stellar network.
#
# Prerequisites:
#   - Local Stellar node running (docker compose up -d stellar)
#   - Stellar CLI configured for 'local' network
#   - Test account funded (./scripts/setup-local-env.sh)
#
# Usage: ./scripts/deploy-insurance-pool.sh [network] [source]
#   network: local (default) or testnet
#   source:  alice (default) or other account name
#
# Configuration (env vars):
#   INSURANCE_POOL_ADMIN     Address authorised to file claims.
#                            Defaults to the deploying account's own address.
#   INSURANCE_POOL_COVERAGE  Flat per-claim compensation cap, in token stroops.
#                            Defaults to 1000000000 (100 XLM at 7 decimals).
#   INSURANCE_POOL_TOKEN     Required. Premium/claim token contract (SAC) ID.
#                            `initialize` takes it as a mandatory argument;
#                            without it the invoke fails and the freshly
#                            deployed pool is left uninitialized, so anyone
#                            could initialize it first (Issue #1265).
#
# The pool admin must sign `initialize` (admin.require_auth()), so
# INSURANCE_POOL_ADMIN must be the source account or another key the source
# can sign for. The script verifies the stored token afterwards.

set -euo pipefail

NETWORK="${1:-local}"
SOURCE="${2:-alice}"

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

echo "=== Deploying insurance_pool to $NETWORK ==="
echo "Source account: $SOURCE"
echo ""

if ! stellar network ls | grep -q "^$NETWORK"; then
  echo -e "${RED}❌ Network '$NETWORK' not configured${NC}"
  echo "Configure it with: stellar network add --global $NETWORK --rpc-url <url>"
  exit 1
fi

if ! stellar keys address "$SOURCE" &> /dev/null; then
  echo -e "${RED}❌ Account '$SOURCE' not found${NC}"
  echo "Create it with: stellar keys generate --global $SOURCE"
  exit 1
fi

SOURCE_ADDRESS="$(stellar keys address "$SOURCE")"
ADMIN="${INSURANCE_POOL_ADMIN:-$SOURCE_ADDRESS}"
COVERAGE="${INSURANCE_POOL_COVERAGE:-1000000000}"
TOKEN="${INSURANCE_POOL_TOKEN:-}"
if [[ -z "$TOKEN" ]]; then
  echo -e "${RED}❌ INSURANCE_POOL_TOKEN is not set (premium/claim token contract ID)${NC}"
  exit 1
fi
if [[ "$ADMIN" != "$SOURCE_ADDRESS" ]]; then
  echo -e "${RED}❌ INSURANCE_POOL_ADMIN must equal the source account ($SOURCE_ADDRESS): initialize requires the admin's signature${NC}"
  exit 1
fi

echo "Building insurance_pool..."
cargo build --target wasm32v1-none --release --quiet -p insurance_pool

WASM_PATH="target/wasm32v1-none/release/insurance_pool.wasm"
if [[ ! -f "$WASM_PATH" ]]; then
  echo -e "${RED}❌ WASM not found: $WASM_PATH${NC}"
  exit 1
fi

echo "Uploading WASM..."
UPLOAD_OUTPUT=$(stellar contract upload \
  --network "$NETWORK" \
  --source "$SOURCE" \
  --wasm "$WASM_PATH" 2>&1)

WASM_HASH=$(echo "$UPLOAD_OUTPUT" | grep -oP 'WASM hash: \K[a-f0-9]+' || true)
if [[ -z "$WASM_HASH" ]]; then
  echo -e "${RED}❌ Failed to upload WASM${NC}"
  echo "$UPLOAD_OUTPUT"
  exit 1
fi
echo "  WASM hash: $WASM_HASH"

echo "Deploying contract..."
DEPLOY_OUTPUT=$(stellar contract deploy \
  --network "$NETWORK" \
  --source "$SOURCE" \
  --wasm-hash "$WASM_HASH" 2>&1)

CONTRACT_ID=$(echo "$DEPLOY_OUTPUT" | grep -oP 'Contract ID: \K[A-Z0-9]+' || true)
if [[ -z "$CONTRACT_ID" ]]; then
  echo -e "${RED}❌ Failed to deploy contract${NC}"
  echo "$DEPLOY_OUTPUT"
  exit 1
fi
echo -e "  ${GREEN}✓${NC} Deployed: $CONTRACT_ID"

echo "Initialising (admin=$ADMIN, coverage=$COVERAGE)..."
stellar contract invoke \
  --network "$NETWORK" \
  --source "$SOURCE" \
  --id "$CONTRACT_ID" \
  -- initialize \
  --admin "$ADMIN" \
  --coverage "$COVERAGE" \
  --token "$TOKEN"

STORED_TOKEN=$(stellar contract invoke \
  --network "$NETWORK" \
  --source "$SOURCE" \
  --id "$CONTRACT_ID" \
  -- get_token_address | tr -d '"')
if [[ "$STORED_TOKEN" != "$TOKEN" ]]; then
  echo -e "${RED}❌ Initialization not confirmed: get_token_address returned '$STORED_TOKEN'${NC}"
  exit 1
fi
echo -e "  ${GREEN}✓${NC} Initialized and verified (token=$TOKEN)"

ENV_FILE=".contracts-${NETWORK}.env"
if [[ -f "$ENV_FILE" ]] && grep -q '^INSURANCE_POOL_ID=' "$ENV_FILE"; then
  sed -i.bak "s/^INSURANCE_POOL_ID=.*/INSURANCE_POOL_ID=$CONTRACT_ID/" "$ENV_FILE" && rm -f "$ENV_FILE.bak"
else
  {
    echo "INSURANCE_POOL_ID=$CONTRACT_ID"
    echo "INSURANCE_POOL_ADMIN=$ADMIN"
  } >> "$ENV_FILE"
fi

echo ""
echo -e "${GREEN}✅ insurance_pool deployed!${NC}"
echo "Contract ID saved to: $ENV_FILE"
