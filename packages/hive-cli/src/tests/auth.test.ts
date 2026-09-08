import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, expect, test } from 'bun:test';

// Auth state is process-wide (one cached login), so this file owns the auth file for its process.
let dir: string;
let authFile: string;
const originalFetch = globalThis.fetch;

// A stand-in for WorkOS, reached over HTTP by the detached `hive auth-refresh` child (whose fetch
// this process cannot mock). Each test installs the responses it expects.
let workos: ReturnType<typeof Bun.serve>;
let workosRequests: Array<URLSearchParams> = [];
let workosReply: (params: URLSearchParams) => Response = () =>
  Response.json({ error: 'invalid_grant' }, { status: 400 });

function unsignedJwt(payload: Record<string, unknown>): string {
  const b64 = (o: object) => Buffer.from(JSON.stringify(o)).toString('base64url');
  return `${b64({ alg: 'none' })}.${b64(payload)}.sig`;
}

const nowS = () => Math.floor(Date.now() / 1000);
const user = { id: 'u', email: 'u@example.com' };

async function writeAuth(refreshToken: string, exp: number): Promise<void> {
  await writeFile(
    authFile,
    JSON.stringify({ access_token: unsignedJwt({ sub: user.id, exp }), refresh_token: refreshToken, user }),
  );
}

async function readRefreshToken(): Promise<string> {
  return JSON.parse(await readFile(authFile, 'utf8')).refresh_token;
}

beforeAll(async () => {
  dir = await mkdtemp(join(tmpdir(), 'hive-auth-'));
  authFile = join(dir, 'auth.json');
  process.env.ALIGNMENT_HIVE_AUTH_FILE = authFile;
  workos = Bun.serve({
    port: 0,
    fetch: async (req) => {
      const params = new URLSearchParams(await req.text());
      workosRequests.push(params);
      return workosReply(params);
    },
  });
  // Read once at module load, so it must be set before the first import of ../lib/auth.
  process.env.ALIGNMENT_HIVE_WORKOS_URL = `http://127.0.0.1:${workos.port}`;
  // Any in-process refresh attempt is a failure unless a test says otherwise.
  globalThis.fetch = (() => Promise.reject(new Error('network disabled in test'))) as unknown as typeof fetch;
});

afterAll(async () => {
  globalThis.fetch = originalFetch;
  delete process.env.ALIGNMENT_HIVE_AUTH_FILE;
  delete process.env.ALIGNMENT_HIVE_WORKOS_URL;
  await workos.stop(true);
  await rm(dir, { recursive: true, force: true });
});

test('an unexpired token whose payload needs base64url decoding is used without a refresh', async () => {
  // The '?>' bytes encode to '-' and '_' under base64url; a plain base64 decoder rejects them.
  const token = unsignedJwt({ sub: 'u', note: '?>?>?>', exp: nowS() + 3600 });
  await writeFile(authFile, JSON.stringify({ access_token: token, refresh_token: 'r', user }));
  const { getAuthData } = await import('../lib/auth');
  const auth = await getAuthData();
  expect(auth?.user.email).toBe('u@example.com');
});

test('an expired token is refreshed by a detached child and the result read back from disk', async () => {
  await writeAuth('r1', nowS() - 10);
  workosRequests = [];
  workosReply = (params) =>
    params.get('refresh_token') === 'r1'
      ? Response.json({ access_token: unsignedJwt({ sub: 'u', exp: nowS() + 3600 }), refresh_token: 'r2', user })
      : Response.json({ error: 'invalid_grant' }, { status: 400 });
  const { getAuthData } = await import('../lib/auth');
  const auth = await getAuthData();
  expect(auth?.refresh_token).toBe('r2');
  expect(await readRefreshToken()).toBe('r2');
  expect(workosRequests.map((p) => p.get('grant_type'))).toEqual(['refresh_token']);
});

test('a refresh the server rejects surfaces the child failure and leaves the file alone', async () => {
  await writeAuth('dead', nowS() - 10);
  workosReply = () => Response.json({ error: 'invalid_grant' }, { status: 400 });
  const { getAuthData } = await import('../lib/auth');
  await expect(getAuthData()).rejects.toThrow('Token refresh failed (400)');
  expect(await readRefreshToken()).toBe('dead');
});

test('a refresh that finishes after another login does not overwrite that login', async () => {
  const now = nowS();
  const userA = { id: 'a', email: 'a@example.com' };
  const userB = { id: 'b', email: 'b@example.com' };
  await writeFile(
    authFile,
    JSON.stringify({ access_token: unsignedJwt({ sub: 'a', exp: now - 10 }), refresh_token: 'ra', user: userA }),
  );
  let release!: () => void;
  const released = new Promise<void>((resolve) => (release = resolve));
  let requested!: () => void;
  const requestSent = new Promise<void>((resolve) => (requested = resolve));
  globalThis.fetch = (async () => {
    requested();
    await released;
    return Response.json({
      access_token: unsignedJwt({ sub: 'a', exp: now + 3600 }),
      refresh_token: 'ra2',
      user: userA,
    });
  }) as unknown as typeof fetch;
  try {
    const { refreshAuthFile, saveAuthData } = await import('../lib/auth');
    const pending = refreshAuthFile();
    await requestSent;
    await saveAuthData({ access_token: unsignedJwt({ sub: 'b', exp: now + 3600 }), refresh_token: 'rb', user: userB });
    release();
    expect((await pending)?.user.email).toBe('b@example.com');
    expect(await readRefreshToken()).toBe('rb');
  } finally {
    globalThis.fetch = (() => Promise.reject(new Error('network disabled in test'))) as unknown as typeof fetch;
  }
});

test('an exchange whose reply is lost is replayed once, and the same token is sent again', async () => {
  await writeAuth('t', nowS() - 10);
  const sent: Array<string | null> = [];
  globalThis.fetch = ((_url: string, init: RequestInit) => {
    sent.push(new URLSearchParams(init.body as string).get('refresh_token'));
    // WorkOS rotated the token on the first request but the response never arrived.
    if (sent.length === 1) return Promise.reject(new Error('socket hang up'));
    return Promise.resolve(
      Response.json({ access_token: unsignedJwt({ sub: 'u', exp: nowS() + 3600 }), refresh_token: 't2', user }),
    );
  }) as unknown as typeof fetch;
  try {
    const { refreshAuthFile } = await import('../lib/auth');
    expect((await refreshAuthFile())?.refresh_token).toBe('t2');
    expect(sent).toEqual(['t', 't']);
    expect(await readRefreshToken()).toBe('t2');
  } finally {
    globalThis.fetch = (() => Promise.reject(new Error('network disabled in test'))) as unknown as typeof fetch;
  }
});

test('a rejection from WorkOS is not replayed', async () => {
  await writeAuth('dead', nowS() - 10);
  let calls = 0;
  globalThis.fetch = (() => {
    calls++;
    return Promise.resolve(Response.json({ error: 'invalid_grant' }, { status: 400 }));
  }) as unknown as typeof fetch;
  try {
    const { refreshAuthFile } = await import('../lib/auth');
    await expect(refreshAuthFile()).rejects.toThrow('Token refresh failed (400)');
    expect(calls).toBe(1);
    expect(await readRefreshToken()).toBe('dead');
  } finally {
    globalThis.fetch = (() => Promise.reject(new Error('network disabled in test'))) as unknown as typeof fetch;
  }
});

test('a rejected refresh retries once against a token another process saved meanwhile', async () => {
  await writeAuth('stale', nowS() - 10);
  const sent: Array<string | null> = [];
  globalThis.fetch = (async (_url: string, init: RequestInit) => {
    const token = new URLSearchParams(init.body as string).get('refresh_token');
    sent.push(token);
    if (token === 'stale') {
      // Another process rotated the token while this request was in flight; its access token
      // has since expired too, so only an exchange of the newer token can succeed.
      await writeAuth('newer', nowS() - 10);
      return Response.json({ error: 'invalid_grant' }, { status: 400 });
    }
    return Response.json({ access_token: unsignedJwt({ sub: 'u', exp: nowS() + 3600 }), refresh_token: 'newest', user });
  }) as unknown as typeof fetch;
  try {
    const { refreshAuthFile } = await import('../lib/auth');
    expect((await refreshAuthFile())?.refresh_token).toBe('newest');
    expect(sent).toEqual(['stale', 'newer']);
    expect(await readRefreshToken()).toBe('newest');
  } finally {
    globalThis.fetch = (() => Promise.reject(new Error('network disabled in test'))) as unknown as typeof fetch;
  }
});
