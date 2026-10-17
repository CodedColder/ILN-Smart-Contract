/** Mutable deterministic clock for tests that schedule work. */
export function fakeClock(start = 1_700_000_000_000) {
  let current = start;
  return {
    now: () => current,
    advance(ms: number): number {
      current += ms;
      return current;
    },
    set(value: number): number {
      current = value;
      return current;
    },
  };
}

const DAY_MS = 24 * 60 * 60 * 1000;
export { DAY_MS };
