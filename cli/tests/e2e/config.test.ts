/**
 * Tests for config module — iln config command (#245, #247).
 */
import fs from "fs";
import os from "os";
import path from "path";
import { StrKey } from "@stellar/stellar-sdk";
import {
  loadConfig,
  saveConfig,
  resetConfig,
  getConfigValue,
  setConfigValue,
  DEFAULTS,
  MAINNET_NETWORK,
  TESTNET_NETWORK,
  requireInvoiceContract,
} from "../../src/config";

let tmpDir: string;

beforeEach(() => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "iln-cfg-"));
});

afterEach(() => {
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

describe("config module (#245)", () => {
  it("requires an explicit valid mainnet invoice contract before use", () => {
    const address = StrKey.encodeContract(Buffer.alloc(32, 1));
    expect(() => requireInvoiceContract(MAINNET_NETWORK, "")).toThrow(/ILN_MAINNET_INVOICE_CONTRACT_ID/);
    expect(() => requireInvoiceContract(MAINNET_NETWORK, "C".repeat(56))).toThrow(/No valid/);
    expect(requireInvoiceContract(MAINNET_NETWORK, address)).toBe(address);
    expect(requireInvoiceContract(TESTNET_NETWORK, address)).toBe(TESTNET_NETWORK.contracts.invoiceLiquidity);
  });
  it("loadConfig returns defaults when no config file exists", () => {
    const cfg = loadConfig(tmpDir);
    expect(cfg.network).toBe("testnet");
    expect(cfg.rpcUrl).toContain("soroban-testnet");
  });

  it("saveConfig and loadConfig round-trip correctly", () => {
    saveConfig({ network: "mainnet", rpcUrl: "https://rpc.example.com" }, tmpDir);
    const cfg = loadConfig(tmpDir);
    expect(cfg.network).toBe("mainnet");
    expect(cfg.rpcUrl).toBe("https://rpc.example.com");
  });

  it("setConfigValue persists network and loadConfig reads it back", () => {
    setConfigValue("network", "mainnet", tmpDir);
    expect(loadConfig(tmpDir).network).toBe("mainnet");
  });

  it("setConfigValue persists rpcUrl", () => {
    setConfigValue("rpcUrl", "https://custom.rpc", tmpDir);
    expect(getConfigValue("rpcUrl", tmpDir)).toBe("https://custom.rpc");
  });

  it("setConfigValue throws on unknown key", () => {
    expect(() => setConfigValue("unknownKey", "foo", tmpDir)).toThrow(/Unknown config key/);
  });

  it("setConfigValue throws on invalid network value", () => {
    expect(() => setConfigValue("network", "devnet", tmpDir)).toThrow(/testnet.*mainnet/);
  });

  it("getConfigValue returns the stored value", () => {
    setConfigValue("rpcUrl", "https://custom-rpc.example.com", tmpDir);
    expect(getConfigValue("rpcUrl", tmpDir)).toBe("https://custom-rpc.example.com");
  });

  it("resetConfig restores defaults", () => {
    setConfigValue("network", "mainnet", tmpDir);
    resetConfig(tmpDir);
    expect(loadConfig(tmpDir).network).toBe(DEFAULTS.network);
  });
});
