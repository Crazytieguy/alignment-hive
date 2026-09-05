import { SECRET_RULES } from './secret-rules';
import type { SecretRule } from './secret-rules';

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

const ALL_RULES: Array<SecretRule> = [...SECRET_RULES, HIGH_ENTROPY_RULE];

export interface SecretMatch {
  ruleId: string;
  start: number;
  end: number;
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

/** Non-overlapping secret spans in content, earliest first; when rules overlap the earliest span wins. */
export function detectSecrets(content: string): Array<SecretMatch> {
  if (content.length < MIN_SECRET_LENGTH) return [];

  const matches: Array<SecretMatch> = [];
  const lowerContent = content.toLowerCase();

  for (const rule of ALL_RULES) {
    if (rule.keywords?.length && !rule.keywords.some((k) => lowerContent.includes(k))) continue;
    rule.regex.lastIndex = 0;

    let match: RegExpExecArray | null;
    while ((match = rule.regex.exec(content)) !== null) {
      const secretValue = match[1] || match[0];
      if (rule.entropy && shannonEntropy(secretValue) < rule.entropy) continue;
      if (rule === HIGH_ENTROPY_RULE && (/^[0-9a-fA-F]+$/.test(secretValue) || looksLikeNonSecret(secretValue))) {
        continue;
      }
      matches.push({ ruleId: rule.id, start: match.index, end: match.index + match[0].length });
      if (match[0].length === 0) rule.regex.lastIndex++;
    }
  }

  matches.sort((a, b) => a.start - b.start);

  const deduped: Array<SecretMatch> = [];
  for (const m of matches) {
    const last = deduped.at(-1);
    if (last === undefined || m.start >= last.end) deduped.push(m);
  }
  return deduped;
}

export function sanitizeString(content: string): string {
  const secrets = detectSecrets(content);
  if (secrets.length === 0) return content;

  let result = content;
  for (let i = secrets.length - 1; i >= 0; i--) {
    const secret = secrets[i];
    result = `${result.slice(0, secret.start)}[REDACTED:${secret.ruleId}]${result.slice(secret.end)}`;
  }
  return result;
}

/**
 * Redact secrets in every string of a value. Transcript entries are schema-shaped (keys are
 * field names; SAFE_KEYS values are ids/paths), so the default walk skips both. `strict` also
 * scans keys and SAFE_KEYS values: arbitrary script-built structures like workflow run blobs can
 * carry a secret in either. Subtrees below MAX_SANITIZE_DEPTH are dropped, not passed through.
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
        result[key] = val;
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
