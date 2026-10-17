// sdk/examples/invoice_lifecycle.ts
import { ILNClient } from '../src';

async function run() {
  const client = new ILNClient({ network: 'testnet' });
  
  console.log("Submitting invoice...");
  const invoiceId = await client.invoices.submit({ amount: 1000, debtor: "GABC..." });
  
  console.log("Funding invoice...");
  await client.liquidity.fund(invoiceId, { amount: 1000 });
  
  console.log("Settling invoice...");
  await client.invoices.settle(invoiceId);
  console.log("Done!");
}
run();
