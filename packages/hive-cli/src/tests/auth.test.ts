import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, expect, test } from 'bun:test';

// Auth state is process-wide (one cached login), so this file owns the auth file for its process.
let dir: string;
const originalFetch = globalThis.fetch;

function unsignedJwt(payload: Record<string, unknown>): string {
  const b64 = (o: object) => Buffer.from(JSON.stringify(o)).toString('base64url');
  return `${b64({ alg: 'none' })}.${b64(payload)}.sig`;
}

beforeAll(async () => {
  dir = await mkdtemp(join(tmpdir(), 'hive-auth-'));
  process.env.ALIGNMENT_HIVE_AUTH_FILE = join(dir, 'auth.json');
  // Any refresh attempt is a failure for this test.
  globalThis.fetch = (() => Promise.reject(new Error('network disabled in test'))) as unknown as typeof fetch;
});

afterAll(async () => {
  globalThis.fetch = originalFetch;
  delete process.env.ALIGNMENT_HIVE_AUTH_FILE;
  await rm(dir, { recursive: true, force: true });
});

test('an unexpired token whose payload needs base64url decoding is used without a refresh', async () => {
  // The '?>' bytes encode to '-' and '_' under base64url; a plain base64 decoder rejects them.
  const token = unsignedJwt({ sub: 'u', note: '?>?>?>', exp: Math.floor(Date.now() / 1000) + 3600 });
  await writeFile(
    process.env.ALIGNMENT_HIVE_AUTH_FILE!,
    JSON.stringify({ access_token: token, refresh_token: 'r', user: { id: 'u', email: 'u@example.com' } }),
  );
  const { getAuthData } = await import('../lib/auth');
  const auth = await getAuthData();
  expect(auth?.user.email).toBe('u@example.com');
});

test('a refresh that finishes after another login does not overwrite that login', async () => {
  const now = Math.floor(Date.now() / 1000);
  const userA = { id: 'a', email: 'a@example.com' };
  const userB = { id: 'b', email: 'b@example.com' };
  await writeFile(
    process.env.ALIGNMENT_HIVE_AUTH_FILE!,
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
    const { getAuthData, saveAuthData } = await import('../lib/auth');
    const pending = getAuthData();
    await requestSent;
    await saveAuthData({ access_token: unsignedJwt({ sub: 'b', exp: now + 3600 }), refresh_token: 'rb', user: userB });
    release();
    expect((await pending)?.user.email).toBe('b@example.com');
    expect(JSON.parse(await readFile(process.env.ALIGNMENT_HIVE_AUTH_FILE!, 'utf8')).refresh_token).toBe('rb');
  } finally {
    globalThis.fetch = (() => Promise.reject(new Error('network disabled in test'))) as unknown as typeof fetch;
  }
});
