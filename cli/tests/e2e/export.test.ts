/**
 * Tests for `iln export` command logic (#244, #247).
 */
import { it, expect, vi } from "vitest";
import { toCsv, toJson, filterByDate, makeExportCommand, InvoiceRow } from "../../src/commands/export";
import * as config from "../../src/config";

it("rejects an unconfigured mainnet contract through the default export command", async () => {
  const configMock = vi.spyOn(config, "loadConfig").mockReturnValue({
    network: "mainnet",
    rpcUrl: config.MAINNET_NETWORK.rpcUrl,
  });
  vi.stubEnv("ILN_MAINNET_INVOICE_CONTRACT_ID", "");
  const output = vi.spyOn(console, "error").mockImplementation(() => {});
  const exit = vi.spyOn(process, "exit").mockImplementation(() => { throw new Error("exit"); });

  try {
    await expect(makeExportCommand().parseAsync(["node", "iln", "invoices", "--submitter", "GABC"])).rejects.toThrow("exit");
    expect(output).toHaveBeenCalledWith(expect.stringContaining("ILN_MAINNET_INVOICE_CONTRACT_ID"));
  } finally {
    configMock.mockRestore();
    exit.mockRestore();
    output.mockRestore();
    vi.unstubAllEnvs();
  }
});

const ROWS: InvoiceRow[] = [
  {
    id: "1",
    state: "funded",
    submitter: "GABC",
    payer: "GDEF",
    lp: "GHIJ",
    amount: "1000",
    token: "USDC",
    yieldPct: "3.5",
    settlementDate: "2025-06-01",
  },
  {
    id: "2",
    state: "settled",
    submitter: "GKLM",
    payer: "GNOP",
    lp: "GQRS",
    amount: "2000",
    token: "USDC",
    yieldPct: "4.0",
    settlementDate: "2025-09-15",
  },
];

describe("toCsv (#244)", () => {
  it("includes the correct CSV header", () => {
    const csv = toCsv(ROWS);
    expect(csv.split("\n")[0]).toBe(
      "Invoice ID,State,Submitter,Payer,LP,Amount,Token,Yield %,Settlement Date"
    );
  });

  it("produces one data row per invoice", () => {
    const lines = toCsv(ROWS).split("\n");
    expect(lines).toHaveLength(3); // header + 2 rows
  });

  it("includes invoice ID in the row", () => {
    expect(toCsv(ROWS)).toContain('"1"');
    expect(toCsv(ROWS)).toContain('"2"');
  });

  it("escapes double quotes inside values", () => {
    const rows: InvoiceRow[] = [{ ...ROWS[0], state: 'has "quotes"' }];
    expect(toCsv(rows)).toContain('"has ""quotes"""');
  });
});

describe("toJson (#244)", () => {
  it("parses back to the original array", () => {
    const parsed = JSON.parse(toJson(ROWS));
    expect(parsed).toHaveLength(2);
    expect(parsed[0].id).toBe("1");
  });
});

describe("filterByDate (#244)", () => {
  it("returns all rows when no filter is set", () => {
    expect(filterByDate(ROWS)).toHaveLength(2);
  });

  it("filters by --from date", () => {
    const result = filterByDate(ROWS, "2025-07-01");
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("2");
  });

  it("filters by --to date", () => {
    const result = filterByDate(ROWS, undefined, "2025-07-01");
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("1");
  });

  it("filters by both --from and --to", () => {
    const result = filterByDate(ROWS, "2025-01-01", "2025-07-01");
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("1");
  });

  it("returns all rows when range covers all", () => {
    expect(filterByDate(ROWS, "2024-01-01", "2026-12-31")).toHaveLength(2);
  });
});
