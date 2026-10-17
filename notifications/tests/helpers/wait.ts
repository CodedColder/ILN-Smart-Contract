/**
 * Poll a condition on the real clock — used for timer-driven behaviour
 * (digest flush, health sweep) where the service runs on `setInterval`.
 */
export async function waitFor(predicate: () => boolean, timeoutMs = 1000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) {
      throw new Error('waitFor: condition not met before timeout');
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}
