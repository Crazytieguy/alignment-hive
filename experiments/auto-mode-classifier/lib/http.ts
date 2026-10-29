// One HTTP request over node:http, never fetch: the calling session exports HTTP(S)_PROXY at the live router, which
// Bun's fetch would honour even for 127.0.0.1.
import { type IncomingHttpHeaders, request } from 'node:http';

export type HttpReply = { status?: number; headers: IncomingHttpHeaders; bytes: Buffer };

/** The reply, read to its end; rejects on a transport error, or once the socket has idled `timeoutMs`. */
export function httpRequest(o: { host: string; port: number; method: string; path: string; headers: Record<string, string>; body?: Uint8Array; timeoutMs: number }): Promise<HttpReply> {
  return new Promise((done, fail) => {
    const req = request({ host: o.host, port: o.port, method: o.method, path: o.path, headers: o.headers }, (res) => {
      const chunks: Array<Buffer> = [];
      res.on('data', (d: Buffer) => chunks.push(d));
      res.on('end', () => done({ status: res.statusCode, headers: res.headers, bytes: Buffer.concat(chunks) }));
      res.on('error', fail);
    });
    req.setTimeout(o.timeoutMs, () => req.destroy(new Error(`timed out after ${o.timeoutMs} ms`)));
    req.on('error', fail);
    req.end(o.body);
  });
}
