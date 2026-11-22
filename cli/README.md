# ILN CLI

The Invoice Liquidity Network (ILN) CLI allows developers and freelancers to manage their identity, track reputation, and interact with the liquidity network directly from the terminal.

> 📋 **Release History:** See [CHANGELOG.md](./CHANGELOG.md) for version history and breaking changes.

## Installation

### Global Install
```bash
npm install -g @iln/cli
```

### Environment Setup
Ensure you have Node.js v20+ installed.

## Wallet Setup

Before using most commands, you need to set up a Stellar keypair.

### Generate a New Wallet
```bash
iln wallet generate --profile default
```
You will be prompted to set a PIN to encrypt your secret key at rest.

### Import an Existing Wallet
```bash
iln wallet import --secret S... --profile my-wallet
```

### Show Active Wallet
```bash
iln wallet show
```
Displays your public key and current balances for XLM, USDC, and EURC.

### Fund Wallet (Testnet)
```bash
iln wallet fund
```
Requests testnet XLM from Friendbot.

## Command Reference

### Reputation
Check your on-chain reputation score.
- `iln reputation` - Show your own reputation.
- `iln reputation --address G...` - Show reputation for a specific address.
- `--json` - Output as machine-readable JSON.

**Example Output:**
```
Reputation Profile for G...
--------------------------------------------------
Score:       75
Paid:        12
Defaulted:   1
Submitted:   15
Decay:       N/A
--------------------------------------------------
```

### Wallet
Manage your identity and funds.
- `iln wallet generate [--profile name]` - Create a new keypair.
- `iln wallet import --secret <S...>` - Import a secret key.
- `iln wallet show` - Show balances and public key.
- `iln wallet fund` - Get testnet XLM.
- `iln wallet list` - List all saved profiles.

### Config
Configure CLI defaults.
- `iln config set network <testnet|mainnet>`
- `iln config set rpcUrl <url>`

Before running `iln export invoices` on mainnet, set the RPC URL to your
mainnet endpoint and set `ILN_MAINNET_INVOICE_CONTRACT_ID` to the verified
invoice liquidity deployment address. The CLI rejects a missing address or
an invalid Stellar contract StrKey before it constructs the RPC client.
Verify the address against the deployment record for your selected network.

### Export
Export invoices to the network.
- `iln export <file.json>`

## Scripting & Automation

Most commands support the `--json` flag for integration into CI/CD pipelines or custom scripts.

Example:
```bash
SCORE=$(iln reputation --address G... --json | jq '.score')
if [ "$SCORE" -lt 40 ]; then
  echo "Reputation too low!"
  exit 1
fi
```

## Troubleshooting

- **Invalid PIN**: Ensure you are using the PIN set during `wallet generate` or `wallet import`.
- **Network Error**: Check your internet connection or the `rpcUrl` in your config.
- **Profile Not Found**: Run `iln wallet list` to see available profiles or `iln wallet generate` to create one.

## Shell Completion

Enable tab-completion for a better experience.

- **Bash**: `source <(iln completion bash)`
- **Zsh**: `echo 'source <(iln completion zsh)' >> ~/.zshrc`
- **Fish**: `iln completion fish > ~/.config/fish/completions/iln.fish`
