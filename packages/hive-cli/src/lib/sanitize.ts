// Secret redaction. Uploads replace each span detectSecrets finds with `[REDACTED:<rule>]`: gitleaks'
// rules (secret-rules.ts, generated), SECRET_SHAPES for secrets no rule names, a high-entropy
// safety net, and the literal values of this process's credential env vars (withEnvValues). A
// value under a SAFE_KEYS name gets a narrower list (sanitizeDeep). The debrief page marks spans
// from a narrower list through secretSpans.

import { SECRET_RULES } from './secret-rules';
import type { SecretRule } from './secret-rules';

/** What each shape below finds. */
export type ShapeId = 'link' | 'query' | 'cookie' | 'bearer' | 'value';
/** Words that mark a name as holding a secret; words may follow them (`SECRET_ACCESS_KEY`, `DB_PASSWORD_FILE`). */
const WORDS = ['token', 'secret', 'passw(?:or)?d', 'credentials?', 'api[_-]?key', 'private[_-]?key', '(?:access|deploy|service[_-]?role|signing|encryption|master|license)[_-]?key'];
const SUFFIX = '(?:[_-][A-Za-z0-9]+)*';
/** A named value's shapes run only on text holding one of these (compared lower-cased). */
const NAME_KEYWORDS = ['token', 'secret', 'passw', 'credential', 'key', 'authorization', 'cookie'];
/** Names that hold secrets, whole: `access_token`, `clientSecret`, `GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY`; not `max_tokens` or `tokenizer`. */
const NAME = `(?:[A-Za-z0-9_-]*(?:${WORDS.join('|')})${SUFFIX}|authorization|cookie|set-cookie)`;
/** A password's name: its value may be a plain word. */
const PASS_NAME = `(?:[A-Za-z0-9_-]*passw(?:or)?d${SUFFIX})`;
export const SECRET_NAME = new RegExp(`^${NAME}$`, 'i');
/**
 * An env var whose value is a credential: a secret word ends the name (`OPENAI_API_KEY`, `GITHUB_TOKEN`, `DB_PASSWORD`),
 * so metadata about a secret (`TOKEN_FILE`, `SECRET_PATH`, `TOKEN_URL`) and words like `MAX_TOKENS` are not one.
 */
const ENV_SECRET_NAME = new RegExp(`^[A-Za-z0-9_-]*(?:${[...WORDS, 'passphrase', 'secrets'].join('|')})$`, 'i');
const PASS_NAME_RE = new RegExp(`^${PASS_NAME}$`, 'i');
const RANDOM = (chars: string) => `(?=${chars}*\\d)(?=${chars}*[A-Za-z])`;
/** What a redaction left: the page's markers and the upload placeholders. */
const PLACEHOLDER = /^\[(?:token|key|REDACTED:[\w-]+)\]$/;
/** Under a secret-holding name: no whitespace and not already a marker; at least 8 characters for a password, else 16 with a digit and a letter. */
function namedSecret(value: string, name: string): boolean {
  if (/\s/.test(value) || PLACEHOLDER.test(value)) return false;
  return PASS_NAME_RE.test(name) ? value.length >= 8 : value.length >= 16 && /\d/.test(value) && /[A-Za-z]/.test(value);
}
const NOT_MARKER = '(?!\\[(?:token|key)\\])';
const VALUE = `(?=[^\\s"'\\\\]{16})${RANDOM(`[^\\s"'\\\\]`)}${NOT_MARKER}[^\\s"'\\\\]+`;
/** A variable reference (`$DB_PASSWORD`, `${PASSWORD}`) is not the password. */
const PASS_VALUE = `${NOT_MARKER}(?!\\$)[^\\s"'\\\\]{8,}`;
/**
 * A value after a secret-holding name, `context(pass)` being what precedes it for a password's name or any other:
 * a password's value from 8 characters, any other from 16 with a digit and a letter. `values` swaps in other shapes.
 */
const named = (context: (pass: boolean) => string, values: { pass: string; other: string } = { pass: PASS_VALUE, other: VALUE }) =>
  `(?<=${context(true)})${values.pass}|(?<=${context(false)})${values.other}`;
/** An env name: upper case, a secret word with any words before and after it. */
const envName = (pass: boolean) => `\\b(?:[A-Z0-9]+_)*(?:${(pass ? ['passw(?:or)?d'] : [...WORDS, 'authorization', 'cookie']).map((word) => word.toUpperCase()).join('|')})(?:_[A-Z0-9]+)*`;
const UUID = '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\b';
/** An unquoted value: a token-shaped run that no call follows (`token: abc123…`, not `token: makeToken1234()`); `group` numbers its capture. */
const BARE = (min: number, random: boolean, group: number) => `(?=[A-Za-z0-9_.~+/=-]{${min}})${random ? RANDOM('[A-Za-z0-9_.~+/=-]') : ''}(?=([A-Za-z0-9_.~+/=-]+))\\${group}(?![(\\w])`;
/**
 * An HTTP header's name and colon as a line (`Cookie: `), a quoted or escaped JSON key (`"Cookie": "`, `\"cookie\":\"`)
 * or a YAML/object key, `names` being the header names; the value follows.
 */
const header = (names: string) => `(?:^|[\\s'"{,\\\\\\[(])(?:${names})\\\\?["']?[ \\t]*:[ \\t]*\\\\?["']?[ \\t]*`;
/** A header value that is a credential in every form: a scheme's credential with a lower-case letter and an upper-case letter or digit. */
const SCHEME_PARAM = '(?=[A-Za-z0-9._~+/=-]*[a-z])(?=[A-Za-z0-9._~+/=-]*[A-Z0-9])[A-Za-z0-9._~+/=-]{12,}';
/** A word in any case, for a pattern whose other parts are case-sensitive. */
const anyCase = (word: string) => word.replace(/[a-z]/g, (letter) => `[${letter}${letter.toUpperCase()}]`);
const AUTHORIZATION = header(anyCase('(?:proxy-)?authorization'));
/** Secrets no gitleaks rule names. Every shape matches the secret alone (context in lookbehind), so a replacement keeps the key, path or header name. */
export const SECRET_SHAPES: Array<SecretRule & { id: ShapeId }> = [
  { id: 'link', keywords: ['/invite/', '/magic/', '/reset/', '/verify/', '/login/', '/join/', '/claim/', '/signin/', '/auth/'], regex: new RegExp(`(?<=/(?:invite|magic|reset|verify|login|join|claim|signin|auth)/)${RANDOM('[A-Za-z0-9_-]')}[A-Za-z0-9_-]{16,}(?![A-Za-z0-9_/-]|\\.[A-Za-z]{1,5}\\b)`, 'gi') },
  { id: 'query', keywords: ['token=', 'code=', 'key=', 'secret=', 'password=', 'signature=', 'sig=', 'credential='], regex: new RegExp(`(?<=[?&](?:token|access_token|refresh_token|id_token|code|api_key|apikey|key|secret|password|signature|sig|x-amz-signature|x-amz-credential)=)${RANDOM('[^&#\\s"\'<>]')}[^&#\\s"'<>]{8,}`, 'gi') },
  // A cookie header's whole value, every pair of it, up to the closing quote or the line's end.
  { id: 'cookie', keywords: ['cookie'], regex: new RegExp(`(?<=${header('(?:set-)?cookie')})(?=[^'"\\r\\n]*=)[^'"\\s\\\\](?:[^'"\\r\\n]*[^'"\\s\\\\])?`, 'gim') },
  // Authorization with a scheme other than Bearer (`Basic dXNlcjpwYXNz`), or a bare credential with a digit.
  { id: 'value', keywords: ['authorization'], regex: new RegExp(`(?<=${AUTHORIZATION}${anyCase('(?:basic|digest|token|apikey|key)')}[ \\t]+)${SCHEME_PARAM}|(?<=${AUTHORIZATION})(?!${anyCase('bearer')}\\b)${RANDOM('[A-Za-z0-9._~+/=-]')}[A-Za-z0-9._~+/=-]{16,}`, 'gm') },
  { id: 'cookie', keywords: ['session'], regex: new RegExp(`(?<=\\b[\\w-]*session[\\w-]*=)(?!${UUID})${RANDOM('[A-Za-z0-9_.%-]')}[A-Za-z0-9_.%-]{8,}`, 'gi') },
  { id: 'bearer', keywords: ['bearer'], regex: new RegExp(`(?<=\\bBearer\\s+)${RANDOM('[A-Za-z0-9._~+/-]')}[A-Za-z0-9._~+/-]{16,}=*`, 'gi') },
  // A URL's userinfo password: `postgres://app:<password>@db…`.
  { id: 'value', keywords: ['://'], regex: /(?<=\b[a-z][a-z0-9+.-]*:\/\/[^\s/:@"']+:)[^\s/@"']{6,}(?=@)/gi },
  // A token passed as a flag: `hive login --token <token>`.
  { id: 'value', keywords: ['-token', '-password', '-passwd', '-secret', '-api', '-deploy', '-key'], regex: new RegExp(`(?<=\\s--?(?:token|password|passwd|secret|api[_-]?key|deploy[_-]?key|key)[= ])${RANDOM('[^\\s"\']')}[^\\s"']{12,}`, 'gi') },
  // Env names are upper case, so `token = generateToken1234()` in code is left alone.
  { id: 'value', keywords: NAME_KEYWORDS, regex: new RegExp(named((pass) => `${envName(pass)}\\s*=\\s*["']?`), 'g') },
  // A quoted literal assigned to a secret-holding name in any case: `client_secret = "…"`, `const inviteToken = '…'`.
  { id: 'value', keywords: NAME_KEYWORDS, regex: new RegExp(named((pass) => `(?:^|[^A-Za-z0-9_-])${pass ? PASS_NAME : NAME}\\s*=\\s*["']`, { pass: `${PASS_VALUE}(?=["'])`, other: `${VALUE}(?=["'])` }), 'gim') },
  { id: 'value', keywords: NAME_KEYWORDS, regex: new RegExp(named((pass) => `"${pass ? PASS_NAME : NAME}"\\s*:\\s*"`), 'gi') },
  // An unquoted key (YAML, printed objects).
  { id: 'value', keywords: NAME_KEYWORDS, regex: new RegExp(named((pass) => `(?:^|[\\s{,])${pass ? PASS_NAME : NAME}\\s*:\\s*["']?`, { pass: BARE(8, false, 1), other: BARE(16, true, 2) }), 'gim') },
];

/** Header names whose values the header shapes recognize by their name: a structured value under one is scanned as that header's line. */
const HEADER_NAME = /^(?:(?:set-)?cookie|(?:proxy-)?authorization)$/i;
/**
 * How a string under a property name is checked, in every walk over structured values (uploads and the review page):
 * a header's value as that header's line (`prefix` + value), a value under a secret-holding name whole, anything else as text.
 */
export type NamedScan = { kind: 'line'; prefix: string } | { kind: 'whole' } | { kind: 'text' };
export function namedScan(name: string | undefined, value: string): NamedScan {
  if (name === undefined) return { kind: 'text' };
  if (HEADER_NAME.test(name)) return { kind: 'line', prefix: `${name}: ` };
  return SECRET_NAME.test(name) && namedSecret(value, name) ? { kind: 'whole' } : { kind: 'text' };
}

const MAX_SANITIZE_DEPTH = 100;
const MIN_SECRET_LENGTH = 8;
const TRUNCATED_PLACEHOLDER = '[TRUNCATED:max-depth]';

const SAFE_KEYS = new Set([
  'uuid',
  'parentUuid',
  'sessionId',
  'tool_use_id',
  'sourceToolUseID',
  'id',
  'type',
  'role',
  'subtype',
  'level',
  'stop_reason',
  'timestamp',
  'version',
  'model',
  'media_type',
  'name',
  'cwd',
  'gitBranch',
]);

/**
 * Safety net for secrets no gitleaks rule names: any long high-entropy token that is not hex
 * (hashes) and does not look like a path, URL, identifier or ephemeral API id.
 */
const HIGH_ENTROPY_RULE: SecretRule = {
  id: 'high-entropy-secret',
  regex: new RegExp(`(?<![A-Za-z0-9_\\-./+=])([A-Za-z0-9_\\-./+=]{20,200})(?![A-Za-z0-9_\\-./+=])`, 'g'),
  entropy: 4.0,
};

const ALL_RULES: Array<SecretRule> = [...SECRET_SHAPES, ...SECRET_RULES, HIGH_ENTROPY_RULE];
/**
 * What a SAFE_KEYS value is still scanned with: every rule that names a secret, except the link shape, whose
 * `/auth/<segment>` match misfires on the paths and branch names these keys hold (`packages/auth/oauth2-module`).
 */
const SAFE_KEY_RULES: Array<SecretRule> = [...SECRET_SHAPES.filter((shape) => shape.id !== 'link'), ...SECRET_RULES];
/** A record or tool id. Under a SAFE_KEYS name it is checked against credential env values only: gitleaks' `EAAA` prefix rule would take about 1 random uuid in 30,000. */
const UUID_VALUE = new RegExp(`^${UUID}$`, 'i');

const ENV_VALUE_ID = 'env-value';
/** Shorter values are too likely to be test stubs or ordinary words to redact everywhere they appear. */
const MIN_ENV_SECRET_LENGTH = 16;
/** The values of `env`'s credential vars, 16 characters or longer, longest first. */
export function envSecretValues(env: Record<string, string | undefined>): Array<string> {
  const values = new Set<string>();
  for (const [name, value] of Object.entries(env)) {
    if (value !== undefined && value.length >= MIN_ENV_SECRET_LENGTH && ENV_SECRET_NAME.test(name)) values.add(value);
  }
  return [...values].sort((a, b) => b.length - a.length);
}
/** Built on first use from process.env; null when no var qualifies. The values never leave the process and are never logged. */
let envValueRule: SecretRule | null | undefined;
function envRule(): SecretRule | null {
  if (envValueRule === undefined) {
    const values = envSecretValues(process.env);
    envValueRule = values.length === 0 ? null : { id: ENV_VALUE_ID, regex: new RegExp(values.map((v) => v.replace(/[\\^$.*+?()[\]{}|/-]/g, '\\$&')).join('|'), 'g') };
  }
  return envValueRule;
}
/** For tests: the next scan reads process.env again. */
export function resetEnvSecretValues(): void {
  envValueRule = undefined;
}
/**
 * `rules` plus an exact match for each credential env var's value, when there is one. Additive: the env at scan time
 * may not be the one the session ran under, so the other rules still carry the load.
 */
export function withEnvValues(rules: Array<SecretRule>): Array<SecretRule> {
  const rule = envRule();
  return rule ? [...rules, rule] : rules;
}

export interface SecretMatch {
  ruleId: string;
  start: number;
  end: number;
  /** The secret itself: the rule's first captured group, else the whole match. */
  valueStart: number;
  valueEnd: number;
}

function shannonEntropy(data: string): number {
  const charCounts = new Map<string, number>();
  for (const char of data) {
    charCounts.set(char, (charCounts.get(char) || 0) + 1);
  }

  let entropy = 0;
  const len = data.length;
  for (const count of charCounts.values()) {
    const freq = count / len;
    entropy -= freq * Math.log2(freq);
  }

  return entropy;
}

/**
 * Heuristics that keep the high-entropy safety net from flagging paths, URLs, code identifiers
 * and ephemeral API ids; tuned against real session files, so only add exclusions with evidence.
 */
function looksLikeNonSecret(s: string): boolean {
  if (s.endsWith('/')) return true;

  let slashCount = 0;
  for (const c of s) {
    if (c === '/') slashCount++;
  }

  // 2+ slashes: file paths, URLs, import paths — but NOT base64 (which uses / as a character)
  // Require path-like structure: starts with / or // (absolute/protocol-relative),
  // or has a dotted segment (domain name like github.com, or file extension)
  if (slashCount >= 2) {
    if (s.startsWith('/')) return true;
    if (s.split('/').some((seg) => seg.includes('.'))) return true;
  }

  if (slashCount === 1 && /\.\w+$/.test(s)) return true;

  // Single slash where both segments are word-like (model paths, MIME types)
  if (slashCount === 1) {
    const parts = s.split('/');
    if (parts.length === 2 && parts.every((p) => /^[a-zA-Z0-9][\w.-]*$/.test(p))) return true;
  }

  if (slashCount > 0) return false;

  // Dot-separated identifiers with 2+ segments (process.env, block.source.media_type)
  if (s.includes('.')) {
    const dotParts = s.split('.');
    if (dotParts.length >= 2 && dotParts.every((p) => /^[a-zA-Z_$][\w$]*$/.test(p))) return true;
  }

  if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s)) return true;

  // Anthropic API IDs — ephemeral, not secrets
  if (/^(msg|req|toolu|chatcmpl|resp|agent_msg)_[A-Za-z0-9]+$/.test(s)) return true;

  // Hyphen-separated lowercase words (plan names, branch names)
  if (s.includes('-') && !s.includes('.') && !s.includes('_')) {
    const parts = s.split('-');
    if (parts.length >= 2 && parts.every((p) => /^[a-z]{2,}$/.test(p))) return true;
  }

  return false;
}

/** Non-overlapping secret spans in content, earliest first; overlapping matches merge into one span. */
export function detectSecrets(content: string): Array<SecretMatch> {
  return secretSpans(content, withEnvValues(ALL_RULES));
}

/** `detectSecrets` over a chosen rule list; the review page drops the entropy net and the fuzzy gitleaks rule. */
export function secretSpans(content: string, rules: Array<SecretRule>): Array<SecretMatch> {
  if (content.length < MIN_SECRET_LENGTH) return [];

  const matches: Array<SecretMatch> = [];
  const lowerContent = content.toLowerCase();

  for (const rule of rules) {
    if (rule.keywords?.length && !rule.keywords.some((k) => lowerContent.includes(k))) continue;
    rule.regex.lastIndex = 0;

    let match: RegExpExecArray | null;
    while ((match = rule.regex.exec(content)) !== null) {
      // The secret is the first captured group that matched, else the whole match.
      const secretValue = (match.slice(1) as Array<string | undefined>).find((part) => part) ?? match[0];
      if (rule.entropy && shannonEntropy(secretValue) < rule.entropy) continue;
      if (rule === HIGH_ENTROPY_RULE && (/^[0-9a-fA-F]+$/.test(secretValue) || looksLikeNonSecret(secretValue))) {
        continue;
      }
      const valueStart = match.index + match[0].lastIndexOf(secretValue);
      matches.push({
        ruleId: rule.id,
        start: match.index,
        end: match.index + match[0].length,
        valueStart,
        valueEnd: valueStart + secretValue.length,
      });
      if (match[0].length === 0) rule.regex.lastIndex++;
    }
  }

  matches.sort((a, b) => a.start - b.start);

  // Overlapping matches merge into one span covering both, under the earliest's rule: keeping only
  // one would leave the rest of a longer match (a token's tail past what a narrower rule knows).
  const merged: Array<SecretMatch> = [];
  for (const m of matches) {
    const last = merged.at(-1);
    if (last === undefined || m.start >= last.end) {
      merged.push(m);
    } else {
      last.end = Math.max(last.end, m.end);
      last.valueStart = Math.min(last.valueStart, m.valueStart);
      last.valueEnd = Math.max(last.valueEnd, m.valueEnd);
    }
  }
  return merged;
}

export function sanitizeString(content: string): string {
  return redact(content, ALL_RULES);
}

function redact(content: string, rules: Array<SecretRule>): string {
  const secrets = secretSpans(content, withEnvValues(rules));
  if (secrets.length === 0) return content;

  let result = content;
  for (let i = secrets.length - 1; i >= 0; i--) {
    const secret = secrets[i];
    result = `${result.slice(0, secret.start)}[REDACTED:${secret.ruleId}]${result.slice(secret.end)}`;
  }
  return result;
}

/** An API beta flag name: lowercase words and digits joined by hyphens, e.g. `context-1m-2025-08-07`. */
const BETA_FLAG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

/**
 * sanitizeDeep for one uploaded transcript record. A request record's own `params.betas` stays
 * whole when every entry is a flag name: the high-entropy rule takes the longer flags for secrets.
 * Only that path of a top-level `api-request` record; a `betas` anywhere else is scanned as usual.
 */
export function sanitizeRecord(record: Record<string, unknown>): Record<string, unknown> {
  const sanitized = sanitizeDeep(record);
  const params = record.type === 'api-request' ? record.params : undefined;
  const betas = params !== null && typeof params === 'object' ? (params as Record<string, unknown>).betas : undefined;
  if (!Array.isArray(betas) || !betas.every((b) => typeof b === 'string' && BETA_FLAG.test(b))) return sanitized;
  return { ...sanitized, params: { ...(sanitized.params as Record<string, unknown>), betas } };
}

/**
 * Redact secrets in every string of a value. Transcript entries are schema-shaped (keys are
 * field names; SAFE_KEYS values are ids/paths), so the default walk leaves keys alone and scans
 * SAFE_KEYS values with SAFE_KEY_RULES only, sparing them the high-entropy net: the same names occur
 * inside tool inputs and results, which carry arbitrary values. `strict` also scans keys and gives SAFE_KEYS values the entropy
 * net: arbitrary script-built structures like workflow run blobs can carry a secret in either.
 * Subtrees below MAX_SANITIZE_DEPTH are dropped, not passed through.
 */
export function sanitizeDeep<T>(value: T, strict = false, depth = 0): T {
  if (depth > MAX_SANITIZE_DEPTH) return TRUNCATED_PLACEHOLDER as T;
  if (value === null || value === undefined) return value;
  if (typeof value === 'string') return sanitizeString(value) as T;
  if (Array.isArray(value)) return value.map((item) => sanitizeDeep(item, strict, depth + 1)) as T;

  if (typeof value === 'object') {
    const result: Record<string, unknown> = {};
    for (const [key, val] of Object.entries(value)) {
      if (!strict && SAFE_KEYS.has(key) && typeof val === 'string') {
        result[key] = redact(val, UUID_VALUE.test(val) ? [] : SAFE_KEY_RULES);
        continue;
      }
      const scan = typeof val === 'string' ? namedScan(key, val) : undefined;
      if (typeof val === 'string' && scan !== undefined && scan.kind !== 'text') {
        result[strict ? sanitizeString(key) : key] = scan.kind === 'whole' ? '[REDACTED:secret-value]' : sanitizeString(scan.prefix + val).slice(scan.prefix.length);
      } else {
        // In strict mode two keys redacting to the same placeholder collide (last one wins) —
        // acceptable: secrets must not survive as keys in the first place.
        result[strict ? sanitizeString(key) : key] = sanitizeDeep(val, strict, depth + 1);
      }
    }
    return result as T;
  }

  return value;
}
