/**
 * Secret detection and sanitization using patterns ported from gitleaks.
 * Detected secrets are replaced with [REDACTED:<rule-id>].
 *
 * Based on: https://github.com/gitleaks/gitleaks
 * See: https://lookingatcomputer.substack.com/p/regex-is-almost-all-you-need
 */

import { ALL_KEYWORDS, SECRET_RULES } from "./secret-rules";

// Maximum recursion depth for sanitizeDeep to prevent stack overflow
const MAX_SANITIZE_DEPTH = 100;

// Minimum string length to scan for secrets
const MIN_SECRET_LENGTH = 8;

// Keys whose string values are structurally safe and never contain secrets.
// These are skipped during sanitization for performance.
const SAFE_KEYS = new Set([
  // UUIDs and IDs
  "uuid",
  "parentUuid",
  "sessionId",
  "tool_use_id",
  "sourceToolUseID",
  "id",
  // Type discriminators and enums
  "type",
  "role",
  "subtype",
  "level",
  "stop_reason",
  // Metadata
  "timestamp",
  "version",
  "model",
  "media_type",
  "name",
  // Paths (may contain usernames but not API keys)
  "cwd",
  "gitBranch",
]);

/**
 * Quick check if a string might contain secrets.
 * Returns false if string contains none of the keywords.
 */
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

/**
 * Calculate Shannon entropy of a string.
 * Higher entropy = more random = more likely to be a real secret.
 * Typical thresholds: 3.0-4.0 bits per character.
 */
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

// Timing stats for DEBUG mode
let _stats = { calls: 0, keywordHits: 0, regexRuns: 0, totalMs: 0 };
export function getDetectSecretsStats() {
  return _stats;
}
export function resetDetectSecretsStats() {
  _stats = { calls: 0, keywordHits: 0, regexRuns: 0, totalMs: 0 };
}

/**
 * Detect secrets in a string using gitleaks patterns.
 * Returns all matches with their positions and entropy.
 */
export function detectSecrets(content: string): Array<SecretMatch> {
  const t0 = process.env.DEBUG ? performance.now() : 0;
  _stats.calls++;

  if (content.length < MIN_SECRET_LENGTH) {
    return [];
  }

  const matches: Array<SecretMatch> = [];

  // Quick pre-filter: skip strings that don't contain any keywords
  // This is an optimization - rules without keywords will still run
  const lowerContent = content.toLowerCase();
  const hasAnyKeyword = mightContainSecrets(content);
  if (hasAnyKeyword) _stats.keywordHits++;

  for (const rule of SECRET_RULES) {
    // Per-rule keyword filter: skip rules whose keywords don't match
    // Rules without keywords always run (e.g., AWS access tokens)
    if (rule.keywords && rule.keywords.length > 0) {
      // If no keywords matched globally, skip rules with keywords
      if (!hasAnyKeyword) continue;
      const hasKeyword = rule.keywords.some((k) => lowerContent.includes(k));
      if (!hasKeyword) continue;
    }
    _stats.regexRuns++;
    // Reset regex state for global matching
    rule.regex.lastIndex = 0;

    let match: RegExpExecArray | null;
    while ((match = rule.regex.exec(content)) !== null) {
      // Get the captured group (if any) or full match
      const secretValue = match[1] || match[0];
      const start = match.index;
      const end = start + match[0].length;

      // Calculate entropy if rule has threshold
      const entropy = rule.entropy ? shannonEntropy(secretValue) : undefined;

      // Skip if entropy is below threshold
      if (rule.entropy && entropy !== undefined && entropy < rule.entropy) {
        continue;
      }

      matches.push({
        ruleId: rule.id,
        match: match[0],
        start,
        end,
        entropy,
      });

      // Prevent infinite loops on zero-width matches
      if (match[0].length === 0) {
        rule.regex.lastIndex++;
      }
    }
  }

  // Sort by position (for consistent replacement order)
  matches.sort((a, b) => a.start - b.start);

  // Dedupe overlapping matches (keep the first/longest)
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

/**
 * Sanitize a string by replacing detected secrets with [REDACTED:<rule-id>].
 */
export function sanitizeString(content: string): string {
  if (content.length < MIN_SECRET_LENGTH) {
    return content;
  }

  const secrets = detectSecrets(content);

  if (secrets.length === 0) {
    return content;
  }

  // Replace from end to start to preserve positions
  let result = content;
  for (let i = secrets.length - 1; i >= 0; i--) {
    const secret = secrets[i];
    result = `${result.slice(0, secret.start)}[REDACTED:${secret.ruleId}]${result.slice(secret.end)}`;
  }

  return result;
}

/**
 * Recursively sanitize all strings in an object or array.
 * Returns a new object/array with sanitized strings.
 * Skips known-safe keys (UUIDs, timestamps, type fields) for performance.
 */
export function sanitizeDeep<T>(value: T, depth = 0): T {
  // Prevent stack overflow from deeply nested or circular structures
  if (depth > MAX_SANITIZE_DEPTH) {
    return value;
  }

  if (value === null || value === undefined) {
    return value;
  }

  if (typeof value === "string") {
    return sanitizeString(value) as T;
  }

  if (Array.isArray(value)) {
    return value.map((item) => sanitizeDeep(item, depth + 1)) as T;
  }

  if (typeof value === "object") {
    const result: Record<string, unknown> = {};
    for (const [key, val] of Object.entries(value)) {
      // Skip sanitization for known-safe string fields
      if (SAFE_KEYS.has(key) && typeof val === "string") {
        result[key] = val;
      } else {
        result[key] = sanitizeDeep(val, depth + 1);
      }
    }
    return result as T;
  }

  // Primitives (numbers, booleans) pass through unchanged
  return value;
}
