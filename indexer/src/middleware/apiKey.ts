import { createHash, timingSafeEqual } from 'node:crypto';
import type { NextFunction, Request, Response } from 'express';

const API_KEY_HEADER = 'x-api-key';

function digestKey(value: string): Buffer {
  return createHash('sha256').update(value, 'utf8').digest();
}

/**
 * Optional API-key authentication.
 *
 * - No keys configured: every request continues as anonymous (public read API).
 * - Valid key: marks the request authenticated so it gets the API-key rate tier.
 * - Invalid key: rejected with 401 and never receives the authenticated tier.
 * - Missing key: continues as anonymous.
 *
 * Key comparison is constant-time and evaluates every configured key so a
 * timing side channel cannot reveal how much of a key matched. Multiple keys
 * allow rotation: list old and new keys together, then drop the old one.
 */
export function createApiKeyMiddleware(apiKeys: string[]) {
  const validKeyDigests = apiKeys
    .map((key) => key.trim())
    .filter(Boolean)
    .map(digestKey);

  return (req: Request, res: Response, next: NextFunction) => {
    if (validKeyDigests.length === 0) {
      next();
      return;
    }

    const apiKey = req.header(API_KEY_HEADER)?.trim();

    if (!apiKey) {
      next();
      return;
    }

    const candidateDigest = digestKey(apiKey);
    let matched = false;
    for (const validDigest of validKeyDigests) {
      if (timingSafeEqual(candidateDigest, validDigest)) {
        matched = true;
      }
    }

    if (matched) {
      res.locals.apiKeyAuthenticated = true;
      next();
      return;
    }

    res.status(401).json({ error: 'Invalid API key' });
  };
}
