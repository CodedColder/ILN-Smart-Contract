import { Command } from 'commander';
import { simulateTransaction, submitTransaction } from '../utils';

export const govCmd = new Command('governance');

govCmd
  .command('execute-proposal')
  .argument('<id>', 'Proposal ID')
  .option('--dry-run', 'Simulate the transaction without submitting')
  .action(async (id, options) => {
    console.log(`Executing proposal ${id}`);
    if (options.dryRun) {
      console.log('DRY RUN MODE ENABLED');
      const sim = await simulateTransaction('executeProposal', { id });
      console.log(`[Simulation Result] Expected state changes: ${JSON.stringify(sim.changes)}`);
      return;
    }
    const result = await submitTransaction('executeProposal', { id });
    console.log(`Transaction submitted: ${result.hash}`);
  });
