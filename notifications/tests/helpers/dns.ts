import type { DnsResolver } from '../../src/delivery/ssrfValidator';

/**
 * Stub DNS resolver for tests.
 *
 * `validateWebhookUrl` resolves hostnames before delivery so a subscriber
 * cannot point a webhook at an internal address (SSRF hardening). Tests use
 * placeholder hostnames such as `hook.example` that will never resolve, and
 * must not depend on (or perform) real DNS lookups — so they inject this
 * resolver instead. Every hostname resolves to a public address; the
 * localhost / private-IP checks run before resolution and still apply.
 */
export const stubDnsResolver: DnsResolver = async () => ['93.184.216.34'];
