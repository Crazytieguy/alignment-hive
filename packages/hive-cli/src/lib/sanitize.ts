import { ALL_KEYWORDS, SECRET_RULES } from './secret-rules';

const MAX_SANITIZE_DEPTH = 100;
const MIN_SECRET_LENGTH = 8;

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

function mightContainSecrets(content: string): boolean {
  const lower = content.toLowerCase();
  for (const keyword of ALL_KEYWORDS) {
    if (lower.includes(keyword)) {
      return true;
    }
  }
  return false;
}

export interface SecretMatch {
  ruleId: string;
  match: string;
  start: number;
  end: number;
  entropy?: number;
}

function shannonEntropy(data: string): number {
  if (!data) return 0;

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
 * Check if a high-entropy string is likely a non-secret (file path, URL, code identifier, etc.)
 * rather than an actual secret. Used to reduce false positives from the high-entropy safety net.
 *
 * Conservative: only excludes patterns that are clearly not secrets.
 * Data-driven: validated against ~6K real session files with 94.8% false positive reduction.
 */
function looksLikeNonSecret(s: string): boolean {
  // Path with trailing slash
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

  // Single slash with file extension of any length (e.g., path/file.jsonl)
  if (slashCount === 1 && /\.\w+$/.test(s)) return true;

  // Single slash where both segments are word-like (model paths, MIME types)
  if (slashCount === 1) {
    const parts = s.split('/');
    if (parts.length === 2 && parts.every((p) => /^[a-zA-Z0-9][\w.-]*$/.test(p))) return true;
  }

  // No slashes from here
  if (slashCount > 0) return false;

  // Dot-separated identifiers with 2+ segments (process.env, block.source.media_type)
  if (s.includes('.')) {
    const dotParts = s.split('.');
    if (dotParts.length >= 2 && dotParts.every((p) => /^[a-zA-Z_$][\w$]*$/.test(p))) return true;
  }

  // UUID pattern (8-4-4-4-12 hex)
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

let _stats = { calls: 0, keywordHits: 0, regexRuns: 0, totalMs: 0 };
export function getDetectSecretsStats() {
  return _stats;
}
export function resetDetectSecretsStats() {
  _stats = { calls: 0, keywordHits: 0, regexRuns: 0, totalMs: 0 };
}

export function detectSecrets(content: string): Array<SecretMatch> {
  const t0 = process.env.DEBUG ? performance.now() : 0;
  _stats.calls++;

  if (content.length < MIN_SECRET_LENGTH) {
    return [];
  }

  const matches: Array<SecretMatch> = [];

  const lowerContent = content.toLowerCase();
  const hasAnyKeyword = mightContainSecrets(content);
  if (hasAnyKeyword) _stats.keywordHits++;

  for (const rule of SECRET_RULES) {
    if (rule.keywords && rule.keywords.length > 0) {
      if (!hasAnyKeyword) continue;
      const hasKeyword = rule.keywords.some((k) => lowerContent.includes(k));
      if (!hasKeyword) continue;
    }
    _stats.regexRuns++;
    rule.regex.lastIndex = 0;

    let match: RegExpExecArray | null;
    while ((match = rule.regex.exec(content)) !== null) {
      const secretValue = match[1] || match[0];
      const start = match.index;
      const end = start + match[0].length;

      const entropy = rule.entropy ? shannonEntropy(secretValue) : undefined;
      if (rule.entropy && entropy !== undefined && entropy < rule.entropy) {
        continue;
      }

      // Skip hex-only strings if the rule requires it (e.g., high-entropy safety net)
      if (rule.notHexOnly && /^[0-9a-fA-F]+$/.test(secretValue)) {
        continue;
      }

      if (rule.id === 'high-entropy-secret' && looksLikeNonSecret(secretValue)) {
        continue;
      }

      matches.push({
        ruleId: rule.id,
        match: match[0],
        start,
        end,
        entropy,
      });

      if (match[0].length === 0) {
        rule.regex.lastIndex++;
      }
    }
  }

  matches.sort((a, b) => a.start - b.start);

  const deduped: Array<SecretMatch> = [];
  for (const m of matches) {
    const last = deduped.at(-1);
    if (last === undefined || m.start >= last.end) {
      deduped.push(m);
    }
  }

  if (process.env.DEBUG) {
    _stats.totalMs += performance.now() - t0;
  }

  return deduped;
}

export function sanitizeString(content: string): string {
  if (content.length < MIN_SECRET_LENGTH) {
    return content;
  }

  const secrets = detectSecrets(content);

  if (secrets.length === 0) {
    return content;
  }

  let result = content;
  for (let i = secrets.length - 1; i >= 0; i--) {
    const secret = secrets[i];
    result = `${result.slice(0, secret.start)}[REDACTED:${secret.ruleId}]${result.slice(secret.end)}`;
  }

  return result;
}

export function sanitizeDeep<T>(value: T, depth = 0): T {
  if (depth > MAX_SANITIZE_DEPTH) return value;
  if (value === null || value === undefined) return value;
  if (typeof value === 'string') return sanitizeString(value) as T;
  if (Array.isArray(value)) return value.map((item) => sanitizeDeep(item, depth + 1)) as T;

  if (typeof value === 'object') {
    const result: Record<string, unknown> = {};
    for (const [key, val] of Object.entries(value)) {
      if (SAFE_KEYS.has(key) && typeof val === 'string') {
        result[key] = val;
      } else {
        result[key] = sanitizeDeep(val, depth + 1);
      }
    }
    return result as T;
  }

  return value;
}
