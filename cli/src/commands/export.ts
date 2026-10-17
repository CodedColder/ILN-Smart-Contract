/**
 * `iln export` — export invoice data to CSV or JSON.
 *
 * Usage:
 *   iln export invoices --submitter G...
 *   iln export invoices --lp G...
 *   iln export invoices --format json --output ./invoices.json
 *   iln export invoices --from 2025-01-01 --to 2025-12-31
 *
 * Issue: #244
 */
import fs from "fs";
import { Command } from "commander";
import { formatError, isJsonMode } from "../format.js";
import { loadConfig } from "../config.js";

export interface InvoiceRow {
  id: string;
  state: string;
  submitter: string;
  payer: string;
  lp: string;
  amount: string;
  token: string;
  yieldPct: string;
  settlementDate: string;
}

/** Serialise rows to CSV with a header line. */
export function toCsv(rows: InvoiceRow[]): string {
  const header =
    "Invoice ID,State,Submitter,Payer,LP,Amount,Token,Yield %,Settlement Date";
  const lines = rows.map((r) =>
    [
      r.id,
      r.state,
      r.submitter,
      r.payer,
      r.lp,
      r.amount,
      r.token,
      r.yieldPct,
      r.settlementDate,
    ]
      .map((v) => `"${String(v).replace(/"/g, '""')}"`)
      .join(",")
  );
  return [header, ...lines].join("\n");
}

/** Serialise rows to pretty-printed JSON. */
export function toJson(rows: InvoiceRow[]): string {
  return JSON.stringify(rows, null, 2);
}

/** Apply optional date filters to a row array. */
export function filterByDate(
  rows: InvoiceRow[],
  from?: string,
  to?: string
): InvoiceRow[] {
  return rows.filter((r) => {
    const d = new Date(r.settlementDate).getTime();
    if (isNaN(d)) return true; // keep rows without a parseable date
    if (from && d < new Date(from).getTime()) return false;
    if (to && d > new Date(to).getTime()) return false;
    return true;
  });
}

/**
 * Fetch invoices from the network. In real usage this calls the SDK;
 * here we expose a hook so tests can inject mock data.
 */
export type InvoiceFetcher = (opts: {
  submitter?: string;
  lp?: string;
}) => Promise<InvoiceRow[]>;

/**
 * Default fetcher — queries the on-chain contract via the SDK for
 * invoices matching the given submitter or LP address (#877).
 */
async function defaultFetcher(opts: {
  submitter?: string;
  lp?: string;
}): Promise<InvoiceRow[]> {
  // Lazy-import the SDK so the CLI doesn't fail if the SDK package
  // isn't installed (e.g. in isolated unit tests of the CLI itself).
  const { ILNClient, TESTNET_RPC_URL } = await import("@iln/sdk");
  const { SorobanRpc } = await import("@stellar/stellar-sdk");

  const config = loadConfig();
  const networkPassphrase =
    config.network === "mainnet"
      ? "Public Global Stellar Network ; September 2015"
      : "Test SDF Network ; September 2015";

  const client = ILNClient.custom({
    rpcUrl: config.rpcUrl || TESTNET_RPC_URL,
    networkPassphrase,
    contractId: "", // resolved from registry or config in production
  });

  // Use a dummy source account for read-only simulations
  const sourceAccount = new SorobanRpc.Api.Account(
    "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    "0"
  );

  let invoices: Awaited<ReturnType<typeof import("@iln/sdk").listInvoicesBySubmitter>> = [];

  if (opts.submitter) {
    const { listInvoicesBySubmitter } = await import("@iln/sdk");
    invoices = await listInvoicesBySubmitter(
      client.rpc,
      client.contractId,
      opts.submitter,
      sourceAccount,
      client.networkPassphrase
    );
  } else if (opts.lp) {
    invoices = await client.getLpInvoices(opts.lp);
  }

  return invoices.map((inv) => ({
    id: String(inv.id),
    state: inv.status,
    submitter: inv.submitter,
    payer: inv.payer,
    lp: inv.lp,
    amount: String(inv.amount),
    token: inv.token,
    yieldPct: String(inv.yieldPct ?? "0"),
    settlementDate: inv.settlementDate ?? "",
  }));
}

export function makeExportCommand(
  fetchInvoices: InvoiceFetcher = sdkFetcher
): Command {
  const cmd = new Command("export").description(
    "Export invoice data to CSV or JSON"
  );

  cmd
    .command("invoices")
    .description("Export invoices for a submitter or LP")
    .option("--submitter <address>", "Filter by submitter Stellar address")
    .option("--lp <address>", "Filter by LP Stellar address")
    .option("--format <csv|json>", "Output format", "csv")
    .option("--output <path>", "Write to file (default: stdout)")
    .option("--from <date>", "Start date filter (YYYY-MM-DD)")
    .option("--to <date>", "End date filter (YYYY-MM-DD)")
    .action(
      async (opts: {
        submitter?: string;
        lp?: string;
        format: string;
        output?: string;
        from?: string;
        to?: string;
      }) => {
        const rootOpts = cmd.parent?.opts() as Record<string, unknown> | undefined;
        const json = isJsonMode(rootOpts);

        try {
          let rows = await fetchInvoices({
            submitter: opts.submitter,
            lp: opts.lp,
          });

          rows = filterByDate(rows, opts.from, opts.to);

          const content =
            opts.format === "json" ? toJson(rows) : toCsv(rows);

          if (opts.output) {
            fs.writeFileSync(opts.output, content, "utf-8");
            console.error(`✓ Exported ${rows.length} invoice(s) to ${opts.output}`);
          } else {
            process.stdout.write(content + "\n");
          }
        } catch (err) {
          formatError((err as Error).message, "EXPORT_ERROR", json);
        }
      }
    );

  return cmd;
}
