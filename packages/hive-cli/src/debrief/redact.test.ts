import { expect, test } from 'bun:test';
import diffScript from '../../assets/review-diff.js' with { type: 'text' };
import pageScript from '../../assets/review-page.js' with { type: 'text' };
import diffCss from '../../assets/review-diff.css' with { type: 'text' };
import pageCss from '../../assets/review-page.css' with { type: 'text' };
import codeCss from '../../assets/review-code.css' with { type: 'text' };
import { syntheticSecret } from './fixtures';
import { SECRET_NAME, auditPage, findSecrets, findSecretsInValue, redactText, redactValue } from './redact';
import type { SecretKind } from './redact';

const random = syntheticSecret;
const token = random(32, 1);

const positives: Array<[SecretKind, string, string]> = [
  ['link', `open https://app.example.test/invite/${token} now`, 'open https://app.example.test/invite/[token] now'],
  ['query', `GET /cb?a=1&code=${random(20, 2)} HTTP/1.1`, 'GET /cb?a=1&code=[token] HTTP/1.1'],
  ['cookie', `Cookie: hb_session=${random(24, 3)}; theme=dark`, 'Cookie: [token]'],
  ['cookie', `set hb_session=${random(24, 4)} for the dev user`, 'set hb_session=[token] for the dev user'],
  ['bearer', `Authorization: Bearer ${random(30, 5)}`, 'Authorization: Bearer [token]'],
  ['jwt', `jwt ${['eyJ' + random(20, 6), 'eyJ' + random(20, 7), random(20, 8)].join('.')} end`, 'jwt [token] end'],
  ['key', `ANTHROPIC=${'sk-ant-api03-' + random(93, 9)}AA`, 'ANTHROPIC=[key]'],
  ['key', `token ${'ghp_' + random(36, 10)}`, 'token [key]'],
  ['value', `export OPENAI_API_KEY="${random(24, 11)}"`, 'export OPENAI_API_KEY="[token]"'],
  ['value', `{"client_secret": "${random(24, 12)}"}`, '{"client_secret": "[token]"}'],
  ['value', `{ _id: "k1", inviteToken: "${random(24, 15)}" }`, '{ _id: "k1", inviteToken: "[token]" }'],
  ['value', `auth:\n  token: ${random(24, 16)}\n`, 'auth:\n  token: [token]\n'],
  // A streamed response is text that happens to start with `data:`, not a data URI.
  ['value', `data: {"access_token":"${random(24, 17)}"}\n\n`, 'data: {"access_token":"[token]"}\n\n'],
  ['value', `client_secret = "${random(24, 18)}"`, 'client_secret = "[token]"'],
  ['value', `const inviteToken = '${random(24, 19)}';`, "const inviteToken = '[token]';"],
  // Names with words after the secret word, URL passwords, flags and plain-word passwords (the Fable review's shapes).
  ['value', `AWS_SECRET_ACCESS_KEY=${random(40, 50)}`, 'AWS_SECRET_ACCESS_KEY=[token]'],
  ['value', `STRIPE_SECRET_KEY=${random(26, 51)}`, 'STRIPE_SECRET_KEY=[token]'],
  ['value', `CONVEX_DEPLOY_KEY=prod:happy-animal-123|${random(26, 52)}`, 'CONVEX_DEPLOY_KEY=[token]'],
  ['value', `SUPABASE_SERVICE_ROLE_KEY=${random(26, 53)}`, 'SUPABASE_SERVICE_ROLE_KEY=[token]'],
  ['value', `DATABASE_URL=postgres://app:${random(16, 54)}@db.example.com:5432/app`, 'DATABASE_URL=postgres://app:[token]@db.example.com:5432/app'],
  ['value', `redis://default:${random(18, 55)}@redis.example.com:6379`, 'redis://default:[token]@redis.example.com:6379'],
  ['value', `hive login --token ${random(26, 56)}`, 'hive login --token [token]'],
  ['value', `{"AWS_SECRET_ACCESS_KEY": "${random(40, 57)}"}`, '{"AWS_SECRET_ACCESS_KEY": "[token]"}'],
  ['value', 'POSTGRES_PASSWORD=mysecretpassword', 'POSTGRES_PASSWORD=[token]'],
  ['value', 'db:\n  password: correcthorsebattery\n', 'db:\n  password: [token]\n'],
  // A cookie header's whole value in every form it is dumped in: a benign first pair does not stop it, nor a second credential.
  ['cookie', `{"Cookie":"locale=en; sid=${random(20, 70)}"}`, '{"Cookie":"[token]"}'],
  ['cookie', `{"cookie": "sid=${random(20, 71)}; auth=${random(16, 72)}"}`, '{"cookie": "[token]"}'],
  ['cookie', `{\\"Cookie\\":\\"locale=en; sid=${random(20, 73)}\\"}`, '{\\"Cookie\\":\\"[token]\\"}'],
  ['cookie', `headers = {'cookie': 'a=b; sid=${random(20, 74)}'}`, "headers = {'cookie': '[token]'}"],
  ['cookie', `Set-Cookie: sid=${random(20, 75)}; Path=/; HttpOnly; Secure`, 'Set-Cookie: [token]'],
  ['cookie', `headers:\n  cookie: locale=en; sid=${random(20, 76)}\n`, 'headers:\n  cookie: [token]\n'],
  ['value', `{"Authorization": "Basic ${random(20, 77)}=="}`, '{"Authorization": "Basic [token]"}'],
  ['value', `"proxy-authorization": "${random(20, 78)}"`, '"proxy-authorization": "[token]"'],
  ['privateKey', `-----BEGIN RSA PRIVATE KEY-----\n${random(64, 13)}\n${random(64, 14)}\n-----END RSA PRIVATE KEY-----`, '[key]\n\n\n'],
];
test.each(positives)('%s: redacted in place, newlines kept', (kind, input, output) => {
  const { text, found } = redactText(input);
  expect(text).toBe(output);
  expect(found.map((hit) => hit.kind)).toEqual([kind]);
  expect(text.split('\n')).toHaveLength(input.split('\n').length);
  expect(redactText(text)).toEqual({ text, found: [] });
  expect(findSecrets(input).map((hit) => hit.kind)).toEqual([kind]);
});

test('ordinary code, ids, paths and data URIs are not secrets', () => {
  const negatives = [
    'password: string', 'process.env.API_TOKEN', 'src/auth/callbackHandler.ts', `client_01${random(24, 20).toUpperCase()}`,
    '123e4567-e89b-12d3-a456-426614174000', '8cfcf94a6f0e1c2b3d4e5f60718293a4b5c6d7e8', `data:image/png;base64,${random(120, 21)}`,
    'const token = generateRandomToken1234();', `sessionId=123e4567-e89b-12d3-a456-426614174000`, `/auth/${random(24, 22)}.png`,
    'max_tokens = 4096', 'const tokenizer = loadTokenizer2026v1();', 'src/auth/oauth2-callback-handler/index.ts',
    'https://example.com/api?key=primary-color', 'https://example.com/?lang=en&code=SYNTAX_ERROR', 'api_key: ${OPENAI_API_KEY_2024}', 'POSTGRES_PASSWORD=${PASSWORD}',
    'password: getPassword2026()', 'cp --key-file src/config/settings.json out/', `style="background:url(data:image/png;base64,${'AKC' + 'p' + random(69, 58)}+/)"`, 'client_secret = read_secret_file2026()', `data:image/png;base64,${random(64, 23)}==`, 'The Set-Cookie: header carries the session', '[token] and [key]',
    'https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:ital,wght@0,400;0,500;0,600;1,400&display=swap',
    `export ALIGNMENT_HIVE_AUTH_FILE="$PWD/auth.json"`, 'const options = { token: makeInviteToken2026abc(), secret: process.env.API_TOKEN };',
    'We set a cookie: it expires daily', 'Authorization: Basic authentication is required', 'Authorization: required', '{"Cookie": "[token]"}',
  ];
  for (const text of negatives) expect([text, redactText(text).found]).toEqual([text, []]);
});

test('secret-holding names: whole property names, never parts of other words', () => {
  for (const name of ['token', 'access_token', 'client_secret', 'clientSecret', 'inviteToken', 'GITHUB_TOKEN', 'OPENAI_API_KEY', 'apiKey', 'private-key', 'Authorization', 'set-cookie', 'AWS_SECRET_ACCESS_KEY', 'STRIPE_SECRET_KEY', 'CONVEX_DEPLOY_KEY', 'deployKey', 'db_passwd', 'credentials']) expect([name, SECRET_NAME.test(name)]).toEqual([name, true]);
  for (const name of ['max_tokens', 'tokenizer', 'tokens', 'secretary', 'description', 'input']) expect([name, SECRET_NAME.test(name)]).toEqual([name, false]);
});

test('structured values keep the property name that marks them secret', () => {
  const nested = { env: { token: random(20, 30) }, headers: { Authorization: `Bearer ${random(24, 31)}` }, list: [{ api_key: random(20, 32) }, { name: 'x' }], note: random(24, 33) };
  const { value, found } = redactValue(nested);
  expect(value).toEqual({ env: { token: '[token]' }, headers: { Authorization: 'Bearer [token]' }, list: [{ api_key: '[token]' }, { name: 'x' }], note: nested.note });
  expect(found.map((hit) => hit.kind).sort()).toEqual(['bearer', 'value', 'value']);
  expect(findSecretsInValue(nested).map((hit) => hit.kind).sort()).toEqual(['bearer', 'value', 'value']);
  expect(findSecretsInValue(value)).toEqual([]);
  // Names with words after the secret word, and a password that is a plain word.
  const env = { AWS_SECRET_ACCESS_KEY: random(40, 34), deployKey: random(26, 35), STRIPE_SECRET_KEY: random(26, 36), db_password: 'correcthorse', port: '5432' };
  expect(redactValue(env).value).toEqual({ AWS_SECRET_ACCESS_KEY: '[token]', deployKey: '[token]', STRIPE_SECRET_KEY: '[token]', db_password: '[token]', port: '5432' });
});

test('structured headers: a cookie value is secret whole, an authorization credential keeps its scheme', () => {
  const headers = { Cookie: `locale=en; sid=${random(20, 80)}; auth=${random(16, 81)}`, 'set-cookie': `sid=${random(20, 82)}; Path=/`, Authorization: `Basic ${random(20, 83)}`, accept: 'text/html' };
  const { value, found } = redactValue({ headers });
  expect(value).toEqual({ headers: { Cookie: '[token]', 'set-cookie': '[token]', Authorization: 'Basic [token]', accept: 'text/html' } });
  expect(found.map((hit) => hit.kind).sort()).toEqual(['cookie', 'cookie', 'value']);
  expect(findSecretsInValue({ headers }).map((hit) => hit.kind).sort()).toEqual(['cookie', 'cookie', 'value']);
  expect(findSecretsInValue(value)).toEqual([]);
  // The audit sees the same dumps escaped into the page, and a raw one in the page's JSON.
  const sid = random(20, 84);
  expect(auditPage(`<pre>{&#34;Cookie&#34;:&#34;locale=en; sid=${sid}&#34;}</pre>`).map((hit) => hit.kind)).toEqual(['cookie']);
  expect(auditPage(`<script type="application/json" id="p">${JSON.stringify({ headers: { cookie: `locale=en; sid=${sid}` } })}</script>`).map((hit) => hit.kind)).toEqual(['cookie']);
});

test('the page audit reads decoded text, attributes and parsed JSON, never escaped HTML', () => {
  const secret = random(24, 40);
  // Escaped HTML hides both from raw-text patterns: &#34; around a JSON value, &#38; before a second parameter.
  expect(auditPage(`<pre>{&#34;token&#34;: &#34;${secret}&#34;}</pre>`).map((hit) => hit.kind)).toEqual(['value']);
  expect(auditPage(`<a href="https://x.test/cb?a=1&#38;token=${secret}">link</a>`).map((hit) => hit.kind)).toEqual(['query']);
  expect(auditPage(`<script type="application/json" id="p">{"env":{"token":"${secret}"}}</script>`).map((hit) => hit.kind)).toEqual(['value']);
  expect(auditPage(`<img src="data:image/png;base64,${random(200, 41)}"><p>fine</p>`)).toEqual([]);
});

test('a page with a large data URI is audited in linear time: the text around it is scanned, the payload skipped', () => {
  const started = performance.now();
  expect(auditPage(`<img src="data:image/png;base64,${'A'.repeat(400_000)}"><p>client_secret = "${random(24, 60)}"</p>`).map((hit) => hit.kind)).toEqual(['value']);
  expect(performance.now() - started).toBeLessThan(5000);
});

test('the renderer\'s own scripts and styles carry nothing the audit would flag', () => {
  for (const asset of [diffScript, pageScript, diffCss, pageCss, codeCss]) expect(findSecrets(asset)).toEqual([]);
});
