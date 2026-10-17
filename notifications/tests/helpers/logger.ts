import type { Logger } from '../../src/lib/logger';

export interface LogEntry {
  level: string;
  msg: string;
  fields: Record<string, unknown>;
}

/**
 * Silent structured logger double for service tests: records every line with
 * its bindings instead of writing to stdout/stderr.
 */
export function makeLogger(bindings: Record<string, unknown> = {}): {
  logger: Logger;
  entries: LogEntry[];
} {
  const entries: LogEntry[] = [];
  const build = (extra: Record<string, unknown>): Logger => ({
    debug: (msg, fields) => entries.push({ level: 'debug', msg, fields: { ...extra, ...fields } }),
    info: (msg, fields) => entries.push({ level: 'info', msg, fields: { ...extra, ...fields } }),
    warn: (msg, fields) => entries.push({ level: 'warn', msg, fields: { ...extra, ...fields } }),
    error: (msg, fields) => entries.push({ level: 'error', msg, fields: { ...extra, ...fields } }),
    child: (more) => build({ ...extra, ...more }),
  });
  return { logger: build(bindings), entries };
}

/** Lines emitted at a given level. */
export function levelOf(entries: LogEntry[], level: string): LogEntry[] {
  return entries.filter((entry) => entry.level === level);
}
