import http from 'node:http';
import https from 'node:https';
import supertest from 'supertest';

/**
 * Test-wide HTTP hygiene for cross-talk between concurrent local services.
 *
 * Why loopback IPv4: `server.listen(0)` binds the IPv6 wildcard `::`. On
 * macOS a foreign local process can still bind `127.0.0.1:<same port>`
 * afterwards (a specific IPv4 binding coexists with the wildcard), and the
 * *specific* binding wins routing for connects to `127.0.0.1` — so a test
 * could receive another service's response (observed as an HTML
 * `401 Unauthorized` from a process that is not part of this repo).
 * Binding `127.0.0.1` ourselves makes our socket the specific one; the
 * kernel then refuses a colliding IPv4 bind.
 *
 * Binding a host is asynchronous (`dns.lookup`), while supertest reads
 * `server.address()` synchronously inside its constructor to build the
 * request URL. So supertest's `serverAddress` is patched to resolve the URL
 * lazily, once the server is actually listening.
 *
 * Why no keep-alive: Node >= 19 keeps `http.globalAgent` connections pooled.
 * Test helpers that call `http.request` without an agent can therefore reuse
 * a socket whose server has already closed within the same run.
 */
const LOOPBACK = '127.0.0.1';

type ListenArgs = unknown[];

const originalListen = http.Server.prototype.listen;

function hasHost(args: ListenArgs): boolean {
  const [first, second] = args;
  if (first && typeof first === 'object' && !Array.isArray(first)) {
    return Boolean((first as { host?: unknown }).host);
  }
  return typeof second === 'string';
}

function patchListen(proto: typeof http.Server.prototype): void {
  proto.listen = function listen(this: http.Server, ...args: ListenArgs): http.Server {
    const first = args[0];
    const isPortBind =
      typeof first === 'number' ||
      (first !== null &&
        typeof first === 'object' &&
        typeof (first as { port?: unknown }).port === 'number');
    if (isPortBind && !hasHost(args)) {
      if (typeof first === 'number') args.splice(1, 0, LOOPBACK);
      else args[0] = { ...(first as object), host: LOOPBACK };
    }
    return originalListen.apply(this, args as never[]) as http.Server;
  } as typeof proto.listen;
}

patchListen(http.Server.prototype);
patchListen(https.Server.prototype);

// --- supertest: resolve the request URL once the server is listening -------

interface SupertestTest {
  _server?: http.Server;
  url: string;
  end(fn?: (err: unknown, res: unknown) => void): unknown;
}

type SupertestModule = {
  Test: { prototype: SupertestTest };
};

const { Test } = supertest as unknown as SupertestModule;
const originalEnd = Test.prototype.end as (
  this: SupertestTest,
  fn?: (err: unknown, res: unknown) => void,
) => unknown;

function urlFor(port: number, path: string): string {
  return `http://${LOOPBACK}:${port}${path}`;
}

(Test.prototype as unknown as Record<string, unknown>).serverAddress = function serverAddress(
  this: SupertestTest,
  app: unknown,
  path: string,
): string {
  const server = app as http.Server;
  if (server.address()) {
    return urlFor((server.address() as { port: number }).port, path);
  }
  // Not listening yet: our patched listen binds a host, which resolves
  // asynchronously, so the real URL is filled in by `end()` below.
  this._server = originalListen.call(server, { port: 0, host: LOOPBACK }) as http.Server;
  (this as unknown as { _pendingPath?: string })._pendingPath = path;
  return urlFor(0, path);
};

Test.prototype.end = function end(
  this: SupertestTest,
  fn?: (err: unknown, res: unknown) => void,
): unknown {
  const pending = (this as unknown as { _pendingPath?: string })._pendingPath;
  if (!pending || !this._server) {
    return originalEnd.call(this, fn);
  }
  const resolveUrl = (): void => {
    const address = this._server?.address();
    if (address && typeof address === 'object') {
      this.url = urlFor(address.port, pending);
    }
  };
  // The placeholder URL carries port 0 until the loopback bind resolves.
  if (this._server.listening) {
    resolveUrl();
    return originalEnd.call(this, fn);
  }
  this._server.once('listening', () => {
    resolveUrl();
    originalEnd.call(this, fn);
  });
  return this;
};

http.globalAgent.keepAlive = false;
https.globalAgent.keepAlive = false;
