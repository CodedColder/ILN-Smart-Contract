import { Command } from 'commander';
import { simulateTransaction, submitTransaction } from '../utils';

export const adminCmd = new Command('admin');

adminCmd
  .command('set-parameter')
  .argument('<param>', 'Parameter name')
  .argument('<value>', 'New value')
  .option('--dry-run', 'Simulate the transaction without submitting')
  .action(async (param, value, options) => {
    console.log(`Setting ${param} to ${value}`);
    if (options.dryRun) {
      console.log('DRY RUN MODE ENABLED');
      const sim = await simulateTransaction('setParameter', { param, value });
      console.log(`[Simulation Result] ParameterUpdated { param: ${param}, old: ${sim.oldValue}, new: ${value} }`);
      return;
    }
    const result = await submitTransaction('setParameter', { param, value });
    console.log(`Transaction submitted: ${result.hash}`);
  });
