// sdk/examples/governance_lifecycle.ts
import { ILNClient } from '../src';

async function run() {
  const client = new ILNClient({ network: 'testnet' });
  
  console.log("Creating proposal...");
  const propId = await client.governance.createProposal({ title: "Change Rate", action: { type: "SetRate", value: 500 } });
  
  console.log("Voting...");
  await client.governance.vote(propId, "YES");
  
  console.log("Executing...");
  await client.governance.execute(propId);
}
run();
