import { Command } from "commander";
import * as readline from "readline";
import { resolveProfile } from "../config.js";
import { formatOutput, formatError, isJsonMode } from "../format.js";

export interface PauseResult {
  txHash: string;
  paused: boolean;
}

export type PauseExecutor = () => Promise<PauseResult>;
export type StateChecker = () => Promise<boolean>;

async function promptConfirm(message: string): Promise<boolean> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => {
    rl.question(`${message} `, (answer) => {
      rl.close();
      resolve(answer.trim().toLowerCase() === "y");
    });
  });
}

// Default mock executors
async function defaultPauseExecutor(): Promise<PauseResult> {
  return {
    txHash: `TX${Math.random().toString(36).slice(2).toUpperCase()}`,
    paused: true,
  };
}

async function defaultUnpauseExecutor(): Promise<PauseResult> {
  return {
    txHash: `TX${Math.random().toString(36).slice(2).toUpperCase()}`,
    paused: false,
  };
}

let defaultState = false;
async function defaultStateChecker(): Promise<boolean> {
  return defaultState;
}

export function makePauseCommand(
  stateChecker: StateChecker = defaultStateChecker,
  pauseExecutor: PauseExecutor = defaultPauseExecutor,
  confirm: (msg: string) => Promise<boolean> = promptConfirm
): Command {
  const cmd = new Command("pause").description("Pause all contract operations");

  cmd
    .option("--yes", "Skip confirmation prompt")
    .option("--dry-run", "Simulate the transaction without submitting")
    .action(async (opts: { yes?: boolean }) => {
      const rootOpts = cmd.parent?.opts() as Record<string, unknown> | undefined;
      const json = isJsonMode(rootOpts);

      try {
        // Require admin authentication
        const profile = resolveProfile(rootOpts?.profile as string | undefined);
        if (!profile) {
          formatError("No connected wallet found. Run: iln wallet generate", "NO_WALLET", json);
          return;
        }

        // Check current state
        if (opts.dryRun) {
          formatOutput({ simulated: true, action: "pause" }, json, () => {
            console.log("DRY RUN MODE ENABLED");
            console.log("[Simulation Result] ParameterUpdated { param: pause, old: false, new: true }");
          });
          return;
        }
        const isCurrentlyPaused = await stateChecker();
        if (isCurrentlyPaused) {
          formatOutput({ paused: true, message: "contract is already paused" }, json, () => {
            console.log("Contract is already paused. No changes made.");
          });
          return;
        }

        // Confirmation prompt
        if (!opts.yes) {
          const msg = "Confirm pause of contract? [y/N]";
          const confirmed = await confirm(msg);
          if (!confirmed) {
            formatOutput({ aborted: true, message: "contract not paused" }, json, () => {
              console.log("Aborted — contract not paused.");
            });
            return;
          }
        }

        const result = await pauseExecutor();
        // Update defaultState if using defaultStateChecker
        defaultState = true;

        formatOutput({ ...result, state: "Paused" }, json, () => {
          console.log(`Contract paused. TX: ${result.txHash}`);
          console.log(`Contract State: Paused`);
        });
      } catch (err) {
        formatError((err as Error).message, "PAUSE_ERROR", json);
      }
    });

  return cmd;
}

export function makeUnpauseCommand(
  stateChecker: StateChecker = defaultStateChecker,
  unpauseExecutor: PauseExecutor = defaultUnpauseExecutor,
  confirm: (msg: string) => Promise<boolean> = promptConfirm
): Command {
  const cmd = new Command("unpause").description("Unpause contract operations");

  cmd
    .option("--yes", "Skip confirmation prompt")
    .option("--dry-run", "Simulate the transaction without submitting")
    .action(async (opts: { yes?: boolean }) => {
      const rootOpts = cmd.parent?.opts() as Record<string, unknown> | undefined;
      const json = isJsonMode(rootOpts);

      try {
        // Require admin authentication
        const profile = resolveProfile(rootOpts?.profile as string | undefined);
        if (!profile) {
          formatError("No connected wallet found. Run: iln wallet generate", "NO_WALLET", json);
          return;
        }

        // Check current state
        if (opts.dryRun) {
          formatOutput({ simulated: true, action: "pause" }, json, () => {
            console.log("DRY RUN MODE ENABLED");
            console.log("[Simulation Result] ParameterUpdated { param: pause, old: false, new: true }");
          });
          return;
        }
        const isCurrentlyPaused = await stateChecker();
        if (!isCurrentlyPaused) {
          formatOutput({ paused: false, message: "contract is already unpaused" }, json, () => {
            console.log("Contract is already unpaused. No changes made.");
          });
          return;
        }

        // Confirmation prompt
        if (!opts.yes) {
          const msg = "Confirm unpause of contract? [y/N]";
          const confirmed = await confirm(msg);
          if (!confirmed) {
            formatOutput({ aborted: true, message: "contract not unpaused" }, json, () => {
              console.log("Aborted — contract not unpaused.");
            });
            return;
          }
        }

        if ((opts as any).dryRun) {
          formatOutput({ simulated: true, action: "unpause" }, json, () => {
            console.log("DRY RUN MODE ENABLED");
            console.log("[Simulation Result] ParameterUpdated { param: pause, old: true, new: false }");
          });
          return;
        }
        const result = await unpauseExecutor();
        // Update defaultState if using defaultStateChecker
        defaultState = false;

        formatOutput({ ...result, state: "Active" }, json, () => {
          console.log(`Contract unpaused. TX: ${result.txHash}`);
          console.log(`Contract State: Active`);
        });
      } catch (err) {
        formatError((err as Error).message, "UNPAUSE_ERROR", json);
      }
    });

  return cmd;
}
