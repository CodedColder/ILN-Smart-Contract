// sdk/examples/insurance_claim.ts
import { ILNClient } from '../src';

async function run() {
  const client = new ILNClient({ network: 'testnet' });
  console.log("Filing insurance claim for default...");
  const claimId = await client.insurance.fileClaim({ invoiceId: "123", reason: "DEFAULT" });
  console.log("Claim ID:", claimId);
}
run();
