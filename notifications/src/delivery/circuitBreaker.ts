export type CircuitState = 'closed' | 'open' | 'half-open';

export interface CircuitBreakerOptions {
  failureThreshold?: number | undefined;
  cooldownMs?: number | undefined;
  now?: (() => number) | undefined;
  /**
   * Called exactly once per transition *into* the `open` state — including a
   * half-open probe failing. Never called for sends that were merely skipped
   * because the circuit was already open, so consumers (delivery analytics,
   * Issue #872) can count real trips rather than skipped attempts.
   */
  onTrip?: ((snapshot: CircuitSnapshot) => void) | undefined;
}

export interface CircuitSnapshot {
  state: CircuitState;
  consecutiveFailures: number;
  openedAt: number | null;
}

const DEFAULT_FAILURE_THRESHOLD = 5;
const DEFAULT_COOLDOWN_MS = 10 * 60 * 1000;

export class CircuitBreaker {
  private state: CircuitState = 'closed';
  private consecutiveFailures = 0;
  private openedAt: number | null = null;
  private probeInFlight = false;
  private readonly failureThreshold: number;
  private readonly cooldownMs: number;
  private readonly now: () => number;
  private readonly onTrip: ((snapshot: CircuitSnapshot) => void) | undefined;

  constructor(opts: CircuitBreakerOptions = {}) {
    this.failureThreshold = opts.failureThreshold ?? DEFAULT_FAILURE_THRESHOLD;
    this.cooldownMs = opts.cooldownMs ?? DEFAULT_COOLDOWN_MS;
    this.now = opts.now ?? Date.now;
    this.onTrip = opts.onTrip;
  }

  getState(): CircuitState {
    this.maybeTransitionToHalfOpen();
    return this.state;
  }

  snapshot(): CircuitSnapshot {
    this.maybeTransitionToHalfOpen();
    return {
      state: this.state,
      consecutiveFailures: this.consecutiveFailures,
      openedAt: this.openedAt,
    };
  }

  canAttempt(): boolean {
    this.maybeTransitionToHalfOpen();
    if (this.state === 'closed') return true;
    if (this.state === 'half-open') {
      if (this.probeInFlight) return false;
      this.probeInFlight = true;
      return true;
    }
    return false;
  }

  recordSuccess(): void {
    this.consecutiveFailures = 0;
    this.openedAt = null;
    this.probeInFlight = false;
    this.state = 'closed';
  }

  recordFailure(logger?: (msg: string) => void): void {
    this.consecutiveFailures += 1;
    if (this.state === 'half-open') {
      this.openCircuit(logger);
      this.probeInFlight = false;
      return;
    }
    if (this.consecutiveFailures >= this.failureThreshold) {
      this.openCircuit(logger);
    }
  }

  private openCircuit(logger?: (msg: string) => void): void {
    if (this.state !== 'open') {
      this.state = 'open';
      this.openedAt = this.now();
      logger?.(
        `circuit_breaker_open consecutive_failures=${this.consecutiveFailures}`,
      );
      this.onTrip?.({
        state: 'open',
        consecutiveFailures: this.consecutiveFailures,
        openedAt: this.openedAt,
      });
    }
  }

  private maybeTransitionToHalfOpen(): void {
    if (this.state !== 'open' || this.openedAt === null) return;
    if (this.now() - this.openedAt >= this.cooldownMs) {
      this.state = 'half-open';
      this.probeInFlight = false;
    }
  }
}
