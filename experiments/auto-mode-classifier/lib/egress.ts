// The only proxy the confined Claude Code knows (HTTPS_PROXY). The router's HTTPS-proxy front tunnels any host it
// does not intercept, so a WebFetch an allowed action triggers would otherwise reach the internet through it. This
// gate sits in front: CONNECT to api.anthropic.com goes to the bench router with the router's proxy credential added
// (a header of the hop to the local router, never part of the request to Anthropic); CONNECT to the OAuth hosts is
// passed to the router to tunnel (token refresh only); everything else, including plain-HTTP proxying, gets 403.
import { appendFileSync } from 'node:fs';
import { connect, createServer, type Socket } from 'node:net';

export const INTERCEPT_HOSTS = new Set(['api.anthropic.com:443']);
export const TUNNEL_HOSTS = new Set(['platform.claude.com:443', 'console.anthropic.com:443']);

export type GateDecision = { action: 'intercept' | 'tunnel' } | { action: 'deny'; why: string };

/** Pure: what the gate does with a proxy request head's first line. */
export function gateDecision(requestLine: string): GateDecision {
  const m = /^CONNECT\s+(\S+)\s+HTTP\/1\.[01]$/i.exec(requestLine.trim());
  if (!m) return { action: 'deny', why: `not a CONNECT: ${requestLine.slice(0, 80)}` };
  const authority = m[1]!.toLowerCase();
  if (INTERCEPT_HOSTS.has(authority)) return { action: 'intercept' };
  if (TUNNEL_HOSTS.has(authority)) return { action: 'tunnel' };
  return { action: 'deny', why: `CONNECT ${authority}` };
}

/** The head to send the router: the client's head with any Proxy-Authorization replaced by the router's (intercept only). */
export function rewriteHead(head: string, proxyAuth: string | undefined): string {
  const lines = head.split('\r\n').filter((l) => l && !/^proxy-authorization:/i.test(l));
  if (proxyAuth) lines.splice(1, 0, `Proxy-Authorization: ${proxyAuth}`);
  return lines.join('\r\n') + '\r\n\r\n';
}

export function startGate(o: { port: number; routerPort: number; proxyAuth: string; log: string }): Promise<{ stop: () => void }> {
  const sockets = new Set<Socket>();
  const server = createServer((client) => {
    sockets.add(client);
    client.on('close', () => sockets.delete(client));
    let buf = Buffer.alloc(0);
    const onData = (d: Buffer): void => {
      buf = Buffer.concat([buf, d]);
      const end = buf.indexOf('\r\n\r\n');
      if (end < 0) {
        if (buf.length > 16384) client.destroy();
        return;
      }
      client.off('data', onData);
      const head = buf.subarray(0, end).toString('latin1');
      const rest = buf.subarray(end + 4);
      const line = head.split('\r\n')[0] ?? '';
      const d2 = gateDecision(line);
      appendFileSync(o.log, JSON.stringify({ t: new Date().toISOString(), line: line.slice(0, 200), ...d2 }) + '\n');
      if (d2.action === 'deny') {
        client.end('HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n');
        return;
      }
      const up = connect(o.routerPort, '127.0.0.1', () => {
        up.write(rewriteHead(head + '\r\n\r\n', d2.action === 'intercept' ? o.proxyAuth : undefined));
        if (rest.length) up.write(rest);
        client.pipe(up);
        up.pipe(client);
      });
      sockets.add(up);
      up.on('close', () => sockets.delete(up));
      up.on('error', () => client.destroy());
      client.on('error', () => up.destroy());
    };
    client.on('data', onData);
  });
  return new Promise((done, fail) => {
    server.once('error', fail);
    server.listen(o.port, '127.0.0.1', () =>
      done({
        stop: () => {
          for (const s of sockets) s.destroy();
          server.close();
        },
      }),
    );
  });
}
