import { describe, expect, test } from 'bun:test';
import { allowTrpcRequest } from '../lib/review-router';

const PORT = 4242;
const request = (headers: Record<string, string>, method = 'POST') =>
  new Request(`http://localhost:${PORT}/trpc/sessions.upload`, { method, headers });

describe('allowTrpcRequest', () => {
  test('the review page: same-origin requests under this server’s Host', () => {
    expect(allowTrpcRequest(request({ host: `localhost:${PORT}` }, 'GET'), PORT)).toBe(true);
    expect(
      allowTrpcRequest(
        request({ host: `localhost:${PORT}`, origin: `http://localhost:${PORT}`, 'sec-fetch-site': 'same-origin' }),
        PORT,
      ),
    ).toBe(true);
    expect(
      allowTrpcRequest(
        request({ host: `127.0.0.1:${PORT}`, origin: `http://127.0.0.1:${PORT}`, 'sec-fetch-site': 'same-origin' }),
        PORT,
      ),
    ).toBe(true);
    // Typed into the address bar, or sent by curl.
    expect(allowTrpcRequest(request({ host: `localhost:${PORT}`, 'sec-fetch-site': 'none' }, 'GET'), PORT)).toBe(true);
  });

  test('a DNS-rebinding page: another Host', () => {
    expect(allowTrpcRequest(request({ host: `evil.example:${PORT}` }), PORT)).toBe(false);
    expect(allowTrpcRequest(request({ host: `localhost:${PORT + 1}` }), PORT)).toBe(false);
    expect(allowTrpcRequest(request({}), PORT)).toBe(false);
  });

  test('a cross-site page: the right Host, but its own Origin', () => {
    expect(allowTrpcRequest(request({ host: `localhost:${PORT}`, origin: 'https://evil.example' }), PORT)).toBe(false);
    expect(allowTrpcRequest(request({ host: `localhost:${PORT}`, origin: 'null' }), PORT)).toBe(false);
    // Another port, or the other loopback name, is a different origin to the browser.
    expect(
      allowTrpcRequest(request({ host: `localhost:${PORT}`, origin: `http://localhost:${PORT + 1}` }), PORT),
    ).toBe(false);
    expect(
      allowTrpcRequest(request({ host: `localhost:${PORT}`, origin: `http://127.0.0.1:${PORT}` }), PORT),
    ).toBe(false);
  });

  test('a cross-site page: Sec-Fetch-Site says so even without an Origin', () => {
    expect(allowTrpcRequest(request({ host: `localhost:${PORT}`, 'sec-fetch-site': 'cross-site' }), PORT)).toBe(false);
    expect(allowTrpcRequest(request({ host: `localhost:${PORT}`, 'sec-fetch-site': 'same-site' }), PORT)).toBe(false);
  });
});
