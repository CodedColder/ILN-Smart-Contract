/**
 * ILN CLI configuration manager.
 *
 * Config file:   ~/.iln/config.json
 * Profile files: ~/.iln/profiles/<name>.json
 *
 * Issues: #245 (iln config command), #246 (--profile flag), #879 (multi-network profiles)
 */
import fs from "fs";
import os from "os";
import path from "path";
import { encrypt, decrypt } from "./crypto.js";

// ── Network profile type ─────────────────────────────────────────────────────

export interface NetworkProfile {
  /** Display name (e.g. "testnet", "mainnet", "futurenet") */
  name: string;
  /** Stellar network passphrase */
  passphrase: string;
  /** Soroban RPC endpoint URL */
  rpcUrl: string;
  /** Deployed contract addresses for this network */
  contracts: {
    invoiceLiquidity: string;
    insurancePool?: string;
    distribution?: string;
    governance?: string;
  };
}

// ── Default network profiles ─────────────────────────────────────────────────

export const TESTNET_NETWORK: NetworkProfile = {
  name: "testnet",
  passphrase: "Test SDF Network ; September 2015",
  rpcUrl: "https://soroban-testnet.stellar.org",
  contracts: {
    invoiceLiquidity: "CCVXGPKFAN374T62PLZAHWIS4UKUVTOYRD72HT36SGWWX7LRD5VFUUJD",
  },
};

export const MAINNET_NETWORK: NetworkProfile = {
  name: "mainnet",
  passphrase: "Public Global Stellar Network ; September 2015",
  rpcUrl: "https://soroban.stellar.org",
  contracts: {
    // TODO: replace with actual mainnet contract IDs after deployment
    invoiceLiquidity: "",
  },
};

export const FUTURENET_NETWORK: NetworkProfile = {
  name: "futurenet",
  passphrase: "Test SDF Future Network ; October 2022",
  rpcUrl: "https://soroban-futurenet.stellar.org",
  contracts: {
    invoiceLiquidity: "",
  },
};

export const BUILTIN_NETWORKS: Record<string, NetworkProfile> = {
  testnet: TESTNET_NETWORK,
  mainnet: MAINNET_NETWORK,
  futurenet: FUTURENET_NETWORK,
};

export const MAINNET_CONFIRMATION_PHRASE = "CONFIRM-MAINNET";

// ── ILNConfig ────────────────────────────────────────────────────────────────

export interface ILNConfig {
  network: "testnet" | "mainnet";
  rpcUrl: string;
  defaultProfile?: string;
  pin?: string; // Encrypted or hashed PIN? No, usually we use PIN to derive key.
  /** Named network profiles. Built-in networks are always available. */
  networks?: Record<string, NetworkProfile>;
  /** Currently active network name (must exist in networks) */
  activeNetwork?: string;
}

export interface ProfileData {
  name: string;
  publicKey: string;
  secretKey?: string;
}

export const DEFAULTS: ILNConfig = {
  network: "testnet",
  rpcUrl: "https://soroban-testnet.stellar.org",
  activeNetwork: "testnet",
  networks: { ...BUILTIN_NETWORKS },
};

/** Resolve the ILN home directory (injectable for tests). */
export function getIlnDir(baseDir?: string): string {
  return path.join(baseDir ?? os.homedir(), ".iln");
}

function configFile(baseDir?: string): string {
  return path.join(getIlnDir(baseDir), "config.json");
}

function profilesDir(baseDir?: string): string {
  return path.join(getIlnDir(baseDir), "profiles");
}

function ensureDirs(baseDir?: string): void {
  const iln = getIlnDir(baseDir);
  if (!fs.existsSync(iln)) fs.mkdirSync(iln, { recursive: true });
  const prof = profilesDir(baseDir);
  if (!fs.existsSync(prof)) fs.mkdirSync(prof, { recursive: true });
}

export function loadConfig(baseDir?: string): ILNConfig {
  ensureDirs(baseDir);
  const file = configFile(baseDir);
  if (!fs.existsSync(file)) return { ...DEFAULTS };
  try {
    return { ...DEFAULTS, ...JSON.parse(fs.readFileSync(file, "utf-8")) } as ILNConfig;
  } catch {
    return { ...DEFAULTS };
  }
}

export function saveConfig(config: ILNConfig, baseDir?: string): void {
  ensureDirs(baseDir);
  fs.writeFileSync(configFile(baseDir), JSON.stringify(config, null, 2), "utf-8");
}

export function resetConfig(baseDir?: string): void {
  saveConfig({ ...DEFAULTS }, baseDir);
}

export function getConfigValue(key: keyof ILNConfig, baseDir?: string): string | undefined {
  const cfg = loadConfig(baseDir);
  const val = cfg[key];
  return val !== undefined ? String(val) : undefined;
}

export function setConfigValue(key: string, value: string, baseDir?: string): void {
  const allowedKeys: (keyof ILNConfig)[] = ["network", "rpcUrl", "defaultProfile"];
  if (!allowedKeys.includes(key as keyof ILNConfig)) {
    throw new Error(`Unknown config key: ${key}. Allowed: ${allowedKeys.join(", ")}`);
  }
  if (key === "network" && value !== "testnet" && value !== "mainnet") {
    throw new Error('network must be "testnet" or "mainnet"');
  }
  const cfg = loadConfig(baseDir);
  (cfg as unknown as Record<string, unknown>)[key] = value;
  saveConfig(cfg, baseDir);
}

// ── Profile helpers (#246) ────────────────────────────────────────────────────

export function profilePath(name: string, baseDir?: string): string {
  ensureDirs(baseDir);
  return path.join(profilesDir(baseDir), `${name}.json`);
}

export function saveProfile(profile: ProfileData, baseDir?: string, pin?: string): void {
  ensureDirs(baseDir);
  const data = { ...profile };
  if (data.secretKey && pin) {
    data.secretKey = encrypt(data.secretKey, pin);
  }
  fs.writeFileSync(profilePath(profile.name, baseDir), JSON.stringify(data, null, 2), "utf-8");
}

export function loadProfile(name: string, baseDir?: string, pin?: string): ProfileData {
  const file = profilePath(name, baseDir);
  if (!fs.existsSync(file)) {
    throw new Error(`Profile "${name}" not found. Run: iln wallet generate --profile ${name}`);
  }
  const data = JSON.parse(fs.readFileSync(file, "utf-8")) as ProfileData;
  if (data.secretKey && pin) {
    try {
      data.secretKey = decrypt(data.secretKey, pin);
    } catch {
      throw new Error(`Invalid PIN for profile "${name}"`);
    }
  }
  return data;
}

export function listProfiles(baseDir?: string): ProfileData[] {
  const dir = profilesDir(baseDir);
  ensureDirs(baseDir);
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir)
    .filter((f) => f.endsWith(".json"))
    .map((f) => {
      try {
        return JSON.parse(fs.readFileSync(path.join(dir, f), "utf-8")) as ProfileData;
      } catch {
        return null;
      }
    })
    .filter((p): p is ProfileData => p !== null);
}

export function resolveProfile(profileFlag?: string, baseDir?: string, pin?: string): ProfileData | null {
  const name = profileFlag ?? loadConfig(baseDir).defaultProfile ?? "default";
  try {
    return loadProfile(name, baseDir, pin);
  } catch {
    return null;
  }
}

// ── Multi-network profile management (#879) ──────────────────────────────────

/** Get the merged network map (built-ins + user-added). */
function getNetworks(cfg: ILNConfig): Record<string, NetworkProfile> {
  return { ...BUILTIN_NETWORKS, ...(cfg.networks ?? {}) };
}

/**
 * Register a named network profile.
 *
 * @throws if the name collides with a built-in network
 */
export function addNetwork(
  name: string,
  passphrase: string,
  rpcUrl: string,
  contracts: NetworkProfile["contracts"],
  baseDir?: string,
): void {
  if (BUILTIN_NETWORKS[name]) {
    throw new Error(`Cannot override built-in network "${name}". Choose a different name.`);
  }
  const cfg = loadConfig(baseDir);
  if (!cfg.networks) cfg.networks = { ...BUILTIN_NETWORKS };
  cfg.networks[name] = { name, passphrase, rpcUrl, contracts };
  saveConfig(cfg, baseDir);
}

/**
 * Remove a named network profile.
 *
 * @throws if the name is a built-in network or is currently active
 */
export function removeNetwork(name: string, baseDir?: string): void {
  if (BUILTIN_NETWORKS[name]) {
    throw new Error(`Cannot remove built-in network "${name}".`);
  }
  const cfg = loadConfig(baseDir);
  if (cfg.activeNetwork === name) {
    throw new Error(`Cannot remove the currently active network "${name}". Switch to another network first.`);
  }
  delete cfg.networks?.[name];
  saveConfig(cfg, baseDir);
}

/**
 * Switch the active network by name.
 *
 * @throws if the network name is not registered
 */
export function switchNetwork(name: string, baseDir?: string): void {
  const cfg = loadConfig(baseDir);
  const networks = getNetworks(cfg);
  if (!networks[name]) {
    throw new Error(`Network "${name}" not found. Available: ${Object.keys(networks).join(", ")}`);
  }
  cfg.activeNetwork = name;
  // Keep legacy fields in sync
  cfg.network = name === "mainnet" ? "mainnet" : "testnet";
  cfg.rpcUrl = networks[name].rpcUrl;
  saveConfig(cfg, baseDir);
}

/**
 * Get the currently active network profile.
 */
export function getActiveNetwork(baseDir?: string): NetworkProfile {
  const cfg = loadConfig(baseDir);
  const networks = getNetworks(cfg);
  const name = cfg.activeNetwork ?? "testnet";
  return networks[name] ?? TESTNET_NETWORK;
}

/**
 * List all registered network profiles (built-in + user-added).
 */
export function listNetworks(baseDir?: string): NetworkProfile[] {
  const cfg = loadConfig(baseDir);
  return Object.values(getNetworks(cfg));
}

/**
 * Check if a network name is the current active network.
 */
export function isActiveNetwork(name: string, baseDir?: string): boolean {
  const cfg = loadConfig(baseDir);
  return (cfg.activeNetwork ?? "testnet") === name;
}

/**
 * Verify that the operator explicitly confirms a mainnet action.
 *
 * For interactive CLI commands, pass a `confirm` function that reads
 * user input. For programmatic use, pass the literal confirmation
 * phrase (`CONFIRM-MAINNET`) as the `input` argument.
 *
 * @param networkName - The network being targeted
 * @param input - User-provided confirmation string (from stdin or prompt)
 * @throws if network is mainnet and confirmation is missing or wrong
 *
 * @example
 * ```ts
 * // In a CLI command handler:
 * requireMainnetConfirmation("mainnet", readline.question("Type CONFIRM-MAINNET to proceed: "));
 * ```
 */
export function requireMainnetConfirmation(
  networkName: string,
  input?: string,
): void {
  if (networkName !== "mainnet") return;
  if (input !== MAINNET_CONFIRMATION_PHRASE) {
    throw new Error(
      `This command targets mainnet. To proceed, provide confirmation: "${MAINNET_CONFIRMATION_PHRASE}"`
    );
  }
}
