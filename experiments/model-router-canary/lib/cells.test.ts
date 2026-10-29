import { expect, test } from 'bun:test';
import { appendFileSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { cellSettings, judgeDeadUrl, judgeEdit, judgeSmoke, parseResult, routedLines, servedBy, userWiring } from './cells';
import { logSince, type RunResult } from './env';

const tmp = () => mkdtempSync(join(tmpdir(), 'canary-cells-'));
const settingsFile = (value: unknown): string => {
  const f = join(tmp(), 'settings.json');
  writeFileSync(f, JSON.stringify(value));
  return f;
};
const ok = (stdout = ''): RunResult => ({ code: 0, stdout, stderr: '', ms: 1000, timedOut: false });

const HELPER = '/Users/u/.local/state/model-router/launcher/bootstrap.sh proxy-authorization';

test('user wiring copies only the router-written proxy keys and the helper', () => {
  const f = settingsFile({
    env: {
      HTTPS_PROXY: 'http://127.0.0.1:8787',
      CLAUDE_CODE_ENABLE_PROXY_AUTH_HELPER: '1',
      CLAUDE_CODE_GATEWAY_HINT_HEADERS: '1',
      CLAUDE_CODE_MAX_CONTEXT_TOKENS: '258400',
      EDITOR: 'vim',
    },
    proxyAuthHelper: HELPER,
    model: 'opus',
    fallbackModel: 'sonnet',
    permissions: { allow: ['Bash'] },
  });
  expect(userWiring(f)).toEqual({
    env: { HTTPS_PROXY: 'http://127.0.0.1:8787', CLAUDE_CODE_ENABLE_PROXY_AUTH_HELPER: '1', CLAUDE_CODE_GATEWAY_HINT_HEADERS: '1' },
    proxyAuthHelper: HELPER,
  });
});

test('user wiring falls back to the base-URL keys when the helper is not the router', () => {
  const f = settingsFile({
    env: { ANTHROPIC_BASE_URL: 'http://127.0.0.1:8787/t/abc', _CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL: '1', HTTPS_PROXY: 'http://corp:3128' },
    proxyAuthHelper: 'corp-helper',
  });
  expect(userWiring(f)).toEqual({ env: { ANTHROPIC_BASE_URL: 'http://127.0.0.1:8787/t/abc', _CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL: '1' } });
});

test('user wiring refuses settings with no router wiring', () => {
  expect(() => userWiring(settingsFile({ env: { EDITOR: 'vim' } }))).toThrow(/no model-router wiring/);
});

test('base-url settings: a GPT cell gets a dummy token and no login, a Claude cell the login and no token', () => {
  const w = { kind: 'base-url', baseUrl: 'http://127.0.0.1:8897/t/tok' } as const;
  const gpt = cellSettings(w, { claude: false });
  expect(gpt.login).toBe(false);
  expect(gpt.settings.env).toEqual({
    ANTHROPIC_BASE_URL: 'http://127.0.0.1:8897/t/tok',
    ANTHROPIC_AUTH_TOKEN: 'canary-unused',
    _CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL: '1',
    DISABLE_AUTOUPDATER: '1',
  });
  expect(gpt.settings.proxyAuthHelper).toBeUndefined();
  const claude = cellSettings(w, { claude: true });
  expect(claude.login).toBe(true);
  expect(claude.settings.env.ANTHROPIC_AUTH_TOKEN).toBeUndefined();
  expect(claude.settings.env.ANTHROPIC_BASE_URL).toBe('http://127.0.0.1:8897/t/tok');
});

test('user-proxy settings use the user wiring and the login for both cells', () => {
  const user = () => ({ env: { HTTPS_PROXY: 'http://127.0.0.1:8787' }, proxyAuthHelper: HELPER });
  for (const claude of [false, true]) {
    const s = cellSettings({ kind: 'user-proxy' }, { claude, user });
    expect(s.login).toBe(true);
    expect(s.settings).toEqual({ env: { HTTPS_PROXY: 'http://127.0.0.1:8787', DISABLE_AUTOUPDATER: '1' }, proxyAuthHelper: HELPER });
  }
});

const result = (over: Record<string, unknown> = {}): string =>
  JSON.stringify({
    type: 'result',
    subtype: 'success',
    is_error: false,
    result: 'DONE',
    num_turns: 4,
    duration_ms: 12_500,
    modelUsage: { 'gpt-6-luna': { inputTokens: 900, outputTokens: 120 } },
    ...over,
  });

const EDITED = 'def magic():\n    return 4817\n';

test('parseResult reads the JSON result and ignores noise lines', () => {
  expect(parseResult(result())?.num_turns).toBe(4);
  expect(parseResult(`warning: something\n${result()}\n`)?.result).toBe('DONE');
  expect(parseResult('not json')).toBeUndefined();
});

test('servedBy matches suffixed keys and ignores entries without output', () => {
  const r = parseResult(result({ modelUsage: { 'gpt-6-luna[1m]': { outputTokens: 5 }, 'claude-haiku-5-5': { outputTokens: 0 } } }))!;
  expect(servedBy(r, 'gpt-6-luna')).toEqual(['gpt-6-luna[1m]']);
  expect(servedBy(r, 'claude-haiku-5-5')).toEqual([]);
});

test('gpt-edit passes on an edited file served by the GPT model', () => {
  const v = judgeEdit(ok(result()), parseResult(result()), EDITED, 'gpt-6-luna', 2);
  expect(v.status).toBe('pass');
  expect(v.metrics).toEqual({ duration_s: 12.5, num_turns: 4 });
});

test('gpt-edit fails when the file is unchanged', () => {
  const v = judgeEdit(ok(result()), parseResult(result()), 'def magic():\n    return 0\n', 'gpt-6-luna', undefined);
  expect(v.status).toBe('fail');
  expect(v.detail).toContain('does not return 4817');
});

test('gpt-edit names the tools Claude Code denied', () => {
  const out = result({ permission_denials: [{ tool_name: 'Edit', tool_use_id: 'x', tool_input: {} }], result: 'permission was denied' });
  expect(judgeEdit(ok(out), parseResult(out), 'def magic():\n    return 0\n', 'gpt-6-luna', 1).detail).toContain('permission denied for Edit');
});

test('gpt-edit fails when a Claude model answered instead (the model self-report is never trusted)', () => {
  const out = result({ modelUsage: { 'claude-sonnet-5': { outputTokens: 300 } }, result: 'I am gpt-6-luna. DONE' });
  const v = judgeEdit(ok(out), parseResult(out), EDITED, 'gpt-6-luna', undefined);
  expect(v.status).toBe('fail');
  expect(v.detail).toContain('does not list gpt-6-luna');
});

test('gpt-edit warns when a non-Haiku Claude model also did work', () => {
  const out = result({ modelUsage: { 'gpt-6-luna': { outputTokens: 10 }, 'claude-haiku-5-5': { outputTokens: 3 }, 'claude-opus-5-5': { outputTokens: 50 } } });
  const v = judgeEdit(ok(out), parseResult(out), EDITED, 'gpt-6-luna', 1);
  expect(v.status).toBe('warn');
  expect(v.detail).toContain('claude-opus-5-5');
  expect(v.detail).not.toContain('haiku');
});

test('gpt-edit fails when the router never saw the model', () => {
  expect(judgeEdit(ok(result()), parseResult(result()), EDITED, 'gpt-6-luna', 0).detail).toContain('bypassed the router');
});

test('gpt-edit reports the session error text and status on failure', () => {
  const out = result({ is_error: true, api_error_status: 400, terminal_reason: 'api_error', result: "API Error: 400 Invalid schema for function 'Artifact'", modelUsage: {} });
  const v = judgeEdit({ ...ok(out), code: 1 }, parseResult(out), 'def magic():\n    return 0\n', 'gpt-6-luna', undefined);
  expect(v.status).toBe('fail');
  expect(v.detail).toBe("exit 1 HTTP 400 (api_error): API Error: 400 Invalid schema for function 'Artifact'");
  const timeout = judgeEdit({ code: null, stdout: '', stderr: '', ms: 300_000, timedOut: true }, undefined, '', 'gpt-6-luna', undefined);
  expect(timeout.detail).toBe('timed out after 300 s');
});

test('claude-smoke needs the reply, a Claude model in modelUsage, and the router log line', () => {
  const out = result({ result: 'ok', modelUsage: { 'claude-haiku-5-5': { outputTokens: 2 } } });
  expect(judgeSmoke(ok(out), parseResult(out), 'claude-haiku-5-5', 1).status).toBe('pass');
  expect(judgeSmoke(ok(out), parseResult(out), 'claude-haiku-5-5', 0).status).toBe('fail');
  const wrong = result({ result: 'Hello!', modelUsage: { 'claude-haiku-5-5': { outputTokens: 2 } } });
  expect(judgeSmoke(ok(wrong), parseResult(wrong), 'claude-haiku-5-5', undefined).detail).toContain('not "ok"');
});

test('the dead-URL check passes only on a fast connection error with no router traffic', () => {
  const refused = result({ is_error: true, terminal_reason: 'api_error', result: 'API Error: Connection refused — a firewall or proxy may be blocking it (ECONNREFUSED)' });
  const failed = { ...ok(refused), code: 1 };
  expect(judgeDeadUrl(failed, parseResult(refused), 0).status).toBe('pass');
  expect(judgeDeadUrl(failed, parseResult(refused), 3).status).toBe('fail');
  // Settings env ignored: the child talks to api.anthropic.com without a login.
  const noLogin = result({ is_error: true, result: 'Not logged in · Please run /login' });
  expect(judgeDeadUrl({ ...ok(noLogin), code: 1 }, parseResult(noLogin), undefined).detail).toContain('another reason');
  expect(judgeDeadUrl(ok(result({ result: 'ok' })), parseResult(result({ result: 'ok' })), undefined).detail).toContain('succeeded');
});

// Real router lines carry ANSI styling around field names.
const line = (model: string) =>
  `\x1b[2m2026-10-28T20:19:40Z\x1b[0m \x1b[32m INFO\x1b[0m routing request \x1b[3mmethod\x1b[0m\x1b[2m=\x1b[0mPOST \x1b[3mpath\x1b[0m\x1b[2m=\x1b[0m/v1/messages \x1b[3mfamily\x1b[0m\x1b[2m=\x1b[0m"gpt" \x1b[3mmodel\x1b[0m\x1b[2m=\x1b[0m"${model}"\n`;

test('routedLines matches styled router lines by model', () => {
  const f = join(tmp(), 'router.log');
  writeFileSync(f, line('gpt-6-luna') + line('gpt-6-luna-mini') + line('gpt-6-luna') + line('claude-haiku-5-5') + 'other\n');
  const { lines } = logSince(f, 0);
  expect(routedLines(lines, 'gpt-6-luna')).toBe(2);
  expect(routedLines(lines, 'claude-haiku-5-5')).toBe(1);
  expect(routedLines(lines, 'gpt-6-sol')).toBe(0);
  expect(routedLines(lines)).toBe(4);
});

test('logSince returns only what was appended, and everything after a truncation', () => {
  const f = join(tmp(), 'router.log');
  writeFileSync(f, 'old line\n');
  const offset = 9;
  appendFileSync(f, 'new line\n');
  expect(logSince(f, offset).lines).toEqual(['new line']);
  writeFileSync(f, 'fresh\n');
  expect(logSince(f, offset).lines).toEqual(['fresh']);
  expect(logSince(join(tmp(), 'missing.log'), 0).readable).toBe(false);
});
