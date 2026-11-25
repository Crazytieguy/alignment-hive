import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, afterEach, describe, expect, test } from 'bun:test';
import { KnownValues, plainValue, processKnownRule, resetKnownValues } from '../lib/known-values';
import { detectSecrets, envSecretValues, resetEnvSecretValues, sanitizeDeep, sanitizeRecord, sanitizeString } from '../lib/sanitize';

// High-entropy test tokens (repeated characters fail entropy checks); GitHub PATs are ghp_ + 36 alphanumerics.
const TEST_GITHUB_TOKEN = 'ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs';
const TEST_GITHUB_TOKEN_2 = 'ghp_9z8Y7x6W5v4U3t2S1r0Q9p8O7n6M5l4K3jTu';

describe('sanitizeString', () => {
  test('splices the redaction marker exactly in place', () => {
    expect(sanitizeString(`found ${TEST_GITHUB_TOKEN} here`)).toBe('found [REDACTED:github-pat] here');
  });

  test('preserves non-secret content', () => {
    const content = 'Hello, this is a normal message.';
    expect(sanitizeString(content)).toBe(content);
    expect(detectSecrets(content)).toHaveLength(0);
  });

  test('handles multiple secrets in one string', () => {
    const webhook1 = 'https://hooks.slack.com/services/T11111111/B11111111/AAAAAAAAAAAAAAAAAAAAAA11';
    const webhook2 = 'https://hooks.slack.com/services/T22222222/B22222222/BBBBBBBBBBBBBBBBBBBBBB22';
    const sanitized = sanitizeString(`webhooks: ${webhook1} and ${webhook2}`);

    expect(sanitized).not.toContain('hooks.slack.com');
    expect(sanitized.match(/\[REDACTED:/g)?.length).toBe(2);
  });

  test('a secret matched by two overlapping rules is redacted once, whole', () => {
    const sanitized = sanitizeString(`api_key = "${TEST_GITHUB_TOKEN}"`);
    expect(sanitized.match(/\[REDACTED:/g)?.length).toBe(1);
    expect(sanitized).not.toContain('ghp_');
  });

  test("overlapping matches merge, so a longer match's tail past a narrower rule is redacted too", () => {
    // slack-bot-token matches up to the dot; the entropy net matches the whole run.
    const token = 'xoxb-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx.extraTail9Zz8Yy7';
    expect(sanitizeString(`token ${token} end`)).toBe('token [REDACTED:slack-bot-token] end');
    expect(detectSecrets(token)).toMatchObject([{ valueStart: 0, valueEnd: token.length }]);
  });
});

describe('the review page\'s shapes apply to uploads too (one rule set)', () => {
  // Low-entropy values, so only the shapes (not the entropy net) can catch them.
  const sid = 'aaaa1111bbbb2222cccc';
  const cases: Array<[string, string]> = [
    ['Bearer', `Authorization: Bearer ${sid}`],
    ['cookie line', `Cookie: locale=en; sid=${sid}`],
    ['cookie JSON', `{"Cookie":"locale=en; sid=${sid}"}`],
    ['query token', `GET /cb?a=1&token=${sid} HTTP/1.1`],
    ['named value', `client_secret = "${sid}"`],
  ];
  test.each(cases)('%s is redacted in upload output', (_name, text) => {
    expect(sanitizeString(text)).not.toContain(sid);
  });

  test('structured headers and secret-holding names are redacted in upload output', () => {
    const entry = { input: { headers: { Cookie: `locale=en; sid=${sid}`, Authorization: `Basic ${sid}` }, env: { client_secret: sid }, note: 'fine' } };
    const out = JSON.stringify(sanitizeDeep(entry));
    expect(out).not.toContain(sid);
    expect(out).toContain('"note":"fine"');
  });
});

describe('sanitizeDeep', () => {
  test('handles mixed nested structures', () => {
    const input = {
      users: [
        { name: 'Alice', token: TEST_GITHUB_TOKEN },
        { name: 'Bob', token: null },
      ],
      config: {
        apiKey: TEST_GITHUB_TOKEN_2,
        enabled: true,
        count: 42,
        missing: undefined,
      },
    };
    const sanitized = sanitizeDeep(input);

    expect(sanitized.users[0].name).toBe('Alice');
    expect(sanitized.users[0].token).toContain('[REDACTED:');
    expect(sanitized.users[1].name).toBe('Bob');
    expect(sanitized.users[1].token).toBe(null);
    expect(sanitized.config.apiKey).toContain('[REDACTED:');
    expect(sanitized.config.enabled).toBe(true);
    expect(sanitized.config.count).toBe(42);
    expect(sanitized.config.missing).toBeUndefined();
  });

  test('handles deeply nested structures within depth limit', () => {
    let nested: Record<string, unknown> = { value: TEST_GITHUB_TOKEN };
    for (let i = 0; i < 50; i++) {
      nested = { child: nested };
    }

    const sanitized = sanitizeDeep(nested);

    let current = sanitized;
    for (let i = 0; i < 50; i++) {
      current = current.child as Record<string, unknown>;
    }
    expect(current.value).toContain('[REDACTED:');
  });

  test('drops subtrees past the depth limit instead of passing them through', () => {
    // 150 levels deep — past the 100 limit, so the secret is never scanned and the whole
    // subtree must be dropped rather than returned unsanitized.
    let nested: Record<string, unknown> = { value: TEST_GITHUB_TOKEN };
    for (let i = 0; i < 150; i++) {
      nested = { child: nested };
    }

    const sanitized = sanitizeDeep(nested);

    expect(JSON.stringify(sanitized)).not.toContain('ghp_');
    expect(JSON.stringify(sanitized)).toContain('[TRUNCATED:max-depth]');
  });

  test('drops over-deep arrays too', () => {
    let nested: unknown = TEST_GITHUB_TOKEN;
    for (let i = 0; i < 150; i++) {
      nested = [nested];
    }

    expect(JSON.stringify(sanitizeDeep(nested))).not.toContain('ghp_');
  });
});

describe('SAFE_KEYS', () => {
  test('skips the high-entropy net for known-safe string fields', () => {
    // High-entropy, but named by no rule: kept under a safe key, redacted elsewhere.
    const opaque = 'aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5';
    const input = {
      uuid: opaque,
      type: 'user',
      timestamp: '2025-01-01T00:00:00Z',
      sessionId: 'sess-12345',
      content: opaque,
    };

    const sanitized = sanitizeDeep(input);

    expect(sanitized.uuid).toBe(input.uuid);
    expect(sanitized.type).toBe(input.type);
    expect(sanitized.timestamp).toBe(input.timestamp);
    expect(sanitized.sessionId).toBe(input.sessionId);
    expect(sanitized.content).toBe('[REDACTED:high-entropy-secret]');
  });

  test('a known secret under a safe key is still redacted, at any depth', () => {
    const record = {
      type: 'assistant',
      uuid: '5e41ef2f-689e-4d7d-b917-06ebc40107bc',
      cwd: '/home/user/project',
      message: {
        model: 'claude-opus-4-1-20250805',
        id: 'msg_01AbCdEfGhIjKlMnOpQrStUv',
        content: [{ type: 'tool_use', id: 'toolu_01AbCdEfGhIjKlMnOpQrStUv', name: 'mcp__x__connect', input: { name: TEST_GITHUB_TOKEN, id: TEST_GITHUB_TOKEN, model: TEST_GITHUB_TOKEN, cwd: `/repo/${TEST_GITHUB_TOKEN}`, other: TEST_GITHUB_TOKEN } }],
      },
    };
    const input = { name: '[REDACTED:github-pat]', id: '[REDACTED:github-pat]', model: '[REDACTED:github-pat]', cwd: '/repo/[REDACTED:github-pat]', other: '[REDACTED:github-pat]' };
    expect(sanitizeRecord(record)).toEqual({ ...record, message: { ...record.message, content: [{ ...record.message.content[0], input }] } });
  });

  test('uuids, and paths or branches under /auth/, are kept under a safe key', () => {
    // `eaaa…` matches gitleaks' square-access-token prefix; `/auth/<segment>` matches the link shape.
    const record = {
      uuid: 'eaaa330f-dea7-4ae7-ba89-cdeafd1c56a3',
      parentUuid: 'ed26a703-eaaa-4e42-abaa-89314a09520d',
      sessionId: 'EAAA330F-DEA7-4AE7-BA89-CDEAFD1C56A3',
      cwd: '/home/u/repo/packages/auth/oauth2-provider-module',
      gitBranch: 'feat/auth/oauth2-refresh-tokens2',
    };
    expect(sanitizeRecord(record)).toEqual(record);
    // Elsewhere the same values are still scanned as text.
    expect(sanitizeString(record.uuid)).toBe('[REDACTED:square-access-token]');
  });

  test("sanitizeRecord keeps a request's beta flags whole, and scans any other betas", () => {
    const betas = ['mid-conversation-tool-changes-2026-07-01', 'thinking-display-updates-2026-08-18'];
    const request = { type: 'api-request', params: { model: 'm', betas, note: TEST_GITHUB_TOKEN } };
    const kept = sanitizeRecord(request) as typeof request;
    expect(kept.params.betas).toEqual(betas);
    expect(kept.params.note).not.toContain('ghp_');
    expect(sanitizeDeep(request).params.betas).not.toEqual(betas);
    // Not flag-shaped, or not a request record's own params: scanned as usual.
    const secret = [`GITHUB_TOKEN=${TEST_GITHUB_TOKEN}`];
    const leaks = [
      { type: 'api-request', params: { betas: [...betas, ...secret] } },
      { type: 'assistant', params: { betas: secret } },
      { type: 'assistant', message: { content: [{ type: 'tool_use', input: { betas: secret } }] } },
      { type: 'assistant', nested: { type: 'api-request', params: { betas: secret } } },
    ];
    for (const r of leaks) expect(JSON.stringify(sanitizeRecord(r))).not.toContain('ghp_');
  });

  test('strict mode disables the SAFE_KEYS skip (workflow run blobs carry arbitrary values)', () => {
    const input = {
      name: TEST_GITHUB_TOKEN, // SAFE_KEYS-named, but the value is a secret
      cwd: '/home/user/project',
    };

    const sanitized = sanitizeDeep(input, { strict: true });

    expect(sanitized.name).toContain('[REDACTED:');
    expect(sanitized.name).not.toContain('ghp_');
    expect(sanitized.cwd).toBe('/home/user/project');
  });

  test('strict mode sanitizes object keys; the default walk leaves them untouched', () => {
    const input = { [TEST_GITHUB_TOKEN]: 'value', normalKey: 'other' };

    const strict = sanitizeDeep(input, { strict: true });
    const strictKeys = Object.keys(strict);
    expect(strictKeys.some((k) => k.includes('[REDACTED:'))).toBe(true);
    expect(strictKeys.some((k) => k.includes('ghp_'))).toBe(false);
    expect(strict.normalKey).toBe('other');

    const lax = sanitizeDeep(input);
    expect(Object.keys(lax)).toContain(TEST_GITHUB_TOKEN);
  });

  test('strict mode sanitizes keys in nested structures', () => {
    const input = { result: { items: [{ [TEST_GITHUB_TOKEN]: 'nested' }] } };

    const sanitized = sanitizeDeep(input, { strict: true });

    const nestedKeys = Object.keys(sanitized.result.items[0]);
    expect(nestedKeys.some((k) => k.includes('[REDACTED:'))).toBe(true);
    expect(nestedKeys.some((k) => k.includes('ghp_'))).toBe(false);
  });

  test('sanitizes non-string safe key values', () => {
    // A safe key whose value is an object is still walked.
    const input = { type: { nested: TEST_GITHUB_TOKEN } };
    const sanitized = sanitizeDeep(input);
    expect(sanitized.type.nested).toContain('[REDACTED:');
  });
});

describe('high-entropy safety net', () => {
  test('catches high-entropy secrets without known patterns', () => {
    // A WorkOS-style key that doesn't match any specific pattern
    const unknownSecret = 'sk_a2V5XzAxS0VXSDRKNVZLODU5M0JTUEQ5NlBOMlFaLGVDQ1dtZkF1d0NhT0xlTHhKRTBsOTROQ3k';
    const secrets = detectSecrets(unknownSecret);
    expect(secrets.length).toBeGreaterThan(0);
    expect(secrets[0].ruleId).toBe('high-entropy-secret');
  });

  test('excludes hex-only strings (hashes)', () => {
    const sha256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
    const sha1 = 'da39a3ee5e6b4b0d3255bfef95601890afd80709';
    const gitCommit = 'a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0';

    expect(detectSecrets(sha256)).toHaveLength(0);
    expect(detectSecrets(sha1)).toHaveLength(0);
    expect(detectSecrets(gitCommit)).toHaveLength(0);
  });

  test('excludes short strings', () => {
    expect(detectSecrets('aBcDeFgHiJkLmNoPqRs')).toHaveLength(0); // 19 chars
  });

  test('excludes low-entropy strings', () => {
    expect(detectSecrets('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')).toHaveLength(0);
  });

  test('excludes file paths', () => {
    const paths = [
      'hive-mind/cli/lib/format.ts',
      '~/.claude/plans/squishy-booping-beacon.md',
      'plugins/hive-mind/skills/retrieval/SKILL.md',
      'web/src/routes/_authenticated/welcome.tsx',
      '/Users/yoav/projects/alignment-hive/',
    ];
    for (const path of paths) {
      expect(detectSecrets(path)).toHaveLength(0);
    }
  });

  test('excludes absolute paths without extensions', () => {
    const paths = [
      '/Users/yoav/projects/alignment-hive',
      '/Users/yoav/worktrees/alignment-hive/agile-glow-5',
      '/private/var/folders/21/3gpj27c974j5vc436plct78w0000gn/T/coven-vcr',
    ];
    for (const path of paths) {
      expect(detectSecrets(path)).toHaveLength(0);
    }
  });

  test('excludes paths with long extensions', () => {
    const paths = ['cli/lib/fixtures/02ed589a-8b41-4004-a7aa-d15cb62f24d3.jsonl', 'web/.env.local.example'];
    for (const path of paths) {
      expect(detectSecrets(path)).toHaveLength(0);
    }
  });

  test('excludes URLs and domain paths', () => {
    const urls = [
      '//github.com/BerriAI/litellm/issues/new',
      '//img.shields.io/badge/status-active-green',
      'github.com/jesseduffield/lazygit/pkg/gui/types',
      '//www.convex.dev/components/cloudflare-r2',
    ];
    for (const url of urls) {
      expect(detectSecrets(url)).toHaveLength(0);
    }
  });

  test('excludes dot-separated code identifiers', () => {
    const identifiers = [
      'block.source.media_type',
      'process.env.BUN_INSTALL',
      'import.meta.env.VITE_CONVEX_URL',
      'convexQueryClient.hashFn',
    ];
    for (const id of identifiers) {
      expect(detectSecrets(id)).toHaveLength(0);
    }
  });

  test('excludes Anthropic API IDs', () => {
    const ids = [
      'toolu_01VvYAgQKN4qngQwybRp6S6E',
      'msg_01QSWm3cMnqs7nQLQtsnQuYU',
      'req_011CXCDYvAUswJ9fhTQbwhd6',
      'agent_msg_01WRNsKYxa9VL4PKMVz41SqE',
    ];
    for (const id of ids) {
      expect(detectSecrets(id)).toHaveLength(0);
    }
  });

  test('excludes hyphenated word sequences', () => {
    for (const name of ['snappy-questing-clover', 'robust-skipping-meadow']) {
      expect(detectSecrets(name)).toHaveLength(0);
    }
  });

  test('excludes MIME types and model paths', () => {
    for (const val of ['application/x-www-form-urlencoded', 'anthropic/claude-sonnet-4-5-20250929']) {
      expect(detectSecrets(val)).toHaveLength(0);
    }
  });

  test('still catches base64 secrets containing slashes', () => {
    // AWS secret access key format — base64 with slashes must NOT be excluded
    for (const secret of ['wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY', 'aB3cD4eF5/gH6iJ7kL8/mN9oP0qR1sT2uV3wX4y']) {
      expect(detectSecrets(secret).length).toBeGreaterThan(0);
    }
  });
});

describe('credential env var values', () => {
  // Low entropy and no digits, so only the known-value rule can catch them.
  const KEY = 'aaaabbbbccccddddeeee';
  const vars: Record<string, string> = {
    FOO_API_KEY: KEY,
    SHORT_TOKEN: 'ffffgggghhhh',
    FOO_SETTING: 'iiiijjjjkkkkllllmmmm',
    FOO_TOKEN_FILE: 'nnnnooooppppqqqqrrrr',
  };
  const withEnv = (extra: Record<string, string>) => {
    Object.assign(process.env, extra);
    resetEnvSecretValues();
  };
  afterEach(() => {
    for (const name of [...Object.keys(vars), 'FOO_DB_PASSWORD']) delete process.env[name];
    resetEnvSecretValues();
  });

  test("a credential var's value is redacted wherever it appears", () => {
    withEnv(vars);
    expect(sanitizeString(`using ${KEY} now`)).toBe('using [REDACTED:known-value] now');
    expect(JSON.stringify(sanitizeDeep({ note: `x${KEY}y` }))).toBe('{"note":"x[REDACTED:known-value]y"}');
  });

  test('values under 16 characters and non-credential names are left alone', () => {
    withEnv(vars);
    for (const name of ['SHORT_TOKEN', 'FOO_SETTING', 'FOO_TOKEN_FILE']) {
      const text = `value ${vars[name]} here`;
      expect(sanitizeString(text)).toBe(text);
    }
    expect(envSecretValues(vars)).toEqual([KEY]);
  });

  test('picks names a secret word ends', () => {
    const picked = (name: string) => envSecretValues({ [name]: KEY }).length === 1;
    for (const name of ['OPENAI_API_KEY', 'GITHUB_TOKEN', 'AWS_SECRET_ACCESS_KEY', 'DB_PASSWORD', 'client_secret', 'GPG_PASSPHRASE']) expect(picked(name)).toBe(true);
    for (const name of ['PATH', 'TOKENIZERS_PARALLELISM', 'CLAUDE_CODE_MAX_OUTPUT_TOKENS', 'AWS_ACCESS_KEY_ID', 'GITHUB_TOKEN_PATH', 'STARSHIP_SESSION_KEY']) expect(picked(name)).toBe(false);
  });

  test('a span overlapping an env value grows to cover it, so no tail of the value survives', () => {
    const password = 'zzzzyyyyxxxx:wwwwvvvv';
    withEnv({ FOO_DB_PASSWORD: password });
    const out = sanitizeString(`Q7mZp2Lk9Wv4Rt8Yn3Bs${password} end`);
    expect(out).not.toContain('wwwwvvvv');
    expect(out).toEndWith(' end');
  });

  test("a credential var's value is redacted under a safe key, even when uuid-shaped", () => {
    const uuidKey = '0f8e2c1a-5b7d-4e3f-9a6b-2c4d8e0f1a3b';
    withEnv({ ...vars, FOO_DB_PASSWORD: uuidKey });
    expect(sanitizeDeep({ name: KEY, id: uuidKey })).toEqual({ name: '[REDACTED:known-value]', id: '[REDACTED:known-value]' });
  });

  test('without credential vars nothing changes', () => {
    resetEnvSecretValues();
    expect(sanitizeString(`using ${KEY} now`)).toBe(`using ${KEY} now`);
  });
});

describe('known values from .env files and MCP configs', () => {
  // Low entropy and no digits, so only the known-value rule can catch them.
  const DOTENV = 'qqqqrrrrssssttttuuuu';
  const DATABASE_URL = 'postgres://app:hunterhunter@db.internal:5432/app';
  const PROJECT_MCP = 'vvvvwwwwxxxxyyyyzzzz';
  const GLOBAL_ENV = 'kkkkllllmmmmnnnnoooo';
  const GLOBAL_BEARER = 'ppppqqqqrrrrsssstttt';
  const OTHER_PROJECT = 'ggggffffeeeeddddcccc';
  const root = mkdtempSync(join(tmpdir(), 'hive-known-'));
  const cwd = join(root, 'proj');
  const claudeConfig = join(root, '.claude.json');
  mkdirSync(cwd);
  writeFileSync(join(cwd, '.env'), `export SERVICE_THING="${DOTENV}"\nDATABASE_URL=${DATABASE_URL}\nSHORT=abcdefghij\nAPI_BASE=https://api.example.com/v1/users\nAPP_NAME=Alignment Hive Staging Server\n`);
  writeFileSync(join(cwd, '.env.example'), 'TEMPLATE_KEY=templatetemplatetemplate\n');
  writeFileSync(join(cwd, '.mcp.json'), JSON.stringify({ mcpServers: { db: { command: 'x', env: { DB_SETTING: PROJECT_MCP } } } }));
  writeFileSync(
    claudeConfig,
    JSON.stringify({
      mcpServers: { api: { type: 'http', url: 'https://mcp.example.com', headers: { Authorization: `Bearer ${GLOBAL_BEARER}` } } },
      projects: { '/elsewhere': { mcpServers: { s: { env: { ANYTHING: OTHER_PROJECT, HOME_DIR: '/home/someone/projects' } } } }, '/x': null },
      env: { NOT_MCP: 'jjjjiiiihhhhggggffff' },
    }),
  );
  writeFileSync(join(root, '.claude.json.bak'), '');
  const known = () => new KnownValues({ env: { FOO_API_KEY: GLOBAL_ENV }, claudeConfig });
  const redacted = (value: string, rule = known().rule([cwd])) => sanitizeString(`x ${value} y`, rule) === 'x [REDACTED:known-value] y';
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  test("a .env value is redacted whatever its name, a URL's credentials included", () => {
    expect(redacted(DOTENV)).toBe(true);
    expect(sanitizeString(`psql ${DATABASE_URL}`, known().rule([cwd]))).toBe('psql [REDACTED:known-value]');
  });

  test("the cwd's .mcp.json env and the global config's env and headers are redacted, the bearer token on its own too", () => {
    expect(redacted(PROJECT_MCP)).toBe(true);
    expect(redacted(GLOBAL_ENV)).toBe(true);
    expect(redacted(GLOBAL_BEARER)).toBe(true);
    expect(sanitizeString(`curl -H 'Authorization: Bearer ${GLOBAL_BEARER}'`, known().rule([cwd]))).toBe("curl -H 'Authorization: Bearer [REDACTED:known-value]'");
    expect(redacted(OTHER_PROJECT)).toBe(true);
  });

  test('cwd files count only for a session that ran there; global values for every session', () => {
    expect(redacted(DOTENV, known().rule([]))).toBe(false);
    expect(redacted(PROJECT_MCP, known().rule(['/nowhere']))).toBe(false);
    expect(redacted(GLOBAL_BEARER, known().rule([]))).toBe(true);
  });

  test('short values, templates, plain configuration and non-MCP config are left alone', () => {
    for (const value of ['abcdefghij', 'templatetemplatetemplate', 'https://api.example.com/v1/users', 'Alignment Hive Staging Server', '/home/someone/projects', 'jjjjiiiihhhhggggffff']) {
      expect(redacted(value)).toBe(false);
    }
  });

  test('plainValue keeps URLs with credentials or token-shaped parts', () => {
    expect(plainValue('https://hooks.example.com/services/aBcD3fGh1jKlMn0pQrStUvWx')).toBe(false);
    expect(plainValue('redis://:s3cretpassword@cache:6379')).toBe(false);
    expect(plainValue('https://example.com/docs/getting-started')).toBe(true);
    expect(plainValue('2026-11-24T00:00:00Z')).toBe(false);
    expect(plainValue('2026-11-24 00:00:00')).toBe(true);
    expect(plainValue('${OTHER_SECRET_VARIABLE}')).toBe(true);
  });

  test('missing, malformed and oversized files are skipped', () => {
    const bad = join(root, 'bad');
    mkdirSync(bad);
    mkdirSync(join(bad, '.env'));
    writeFileSync(join(bad, '.env.local'), Buffer.from([0xff, 0xfe, 0x00, 0x3d, 0x0a]));
    writeFileSync(join(bad, '.mcp.json'), '{ not json');
    writeFileSync(join(bad, '.env.big'), `BIG_VALUE=${'a1'.repeat(1 << 20)}\n`);
    const malformedConfig = join(root, 'malformed.json');
    writeFileSync(malformedConfig, '{"mcpServers": ');
    const rule = new KnownValues({ env: {}, claudeConfig: malformedConfig }).rule([bad, join(root, 'missing')]);
    expect(rule).toBeNull();
    expect(new KnownValues({ env: {}, claudeConfig: join(root, 'missing.json') }).rule([cwd])).not.toBeNull();
  });

  test('each cwd is read once per KnownValues and its rule reused', () => {
    const values = known();
    const first = values.rule([cwd, cwd]);
    writeFileSync(join(cwd, '.env.later'), 'LATER_VALUE=lllllaaaaattttteeeee\n');
    try {
      expect(values.rule([cwd])).toBe(first);
      expect(redacted('lllllaaaaattttteeeee', values.rule([cwd]))).toBe(false);
      expect(redacted('lllllaaaaattttteeeee', known().rule([cwd]))).toBe(true);
    } finally {
      rmSync(join(cwd, '.env.later'));
    }
  });

  test('resetKnownValues makes the process rule read the env again', () => {
    process.env.FOO_KNOWN_RESET_TOKEN = 'mmmmnnnnbbbbvvvvcccc';
    try {
      resetKnownValues();
      expect(redacted('mmmmnnnnbbbbvvvvcccc', processKnownRule())).toBe(true);
      delete process.env.FOO_KNOWN_RESET_TOKEN;
      expect(redacted('mmmmnnnnbbbbvvvvcccc', processKnownRule())).toBe(true);
      resetKnownValues();
      expect(redacted('mmmmnnnnbbbbvvvvcccc', processKnownRule())).toBe(false);
    } finally {
      delete process.env.FOO_KNOWN_RESET_TOKEN;
      resetKnownValues();
    }
  });
});
