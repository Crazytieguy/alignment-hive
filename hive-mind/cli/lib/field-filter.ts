/**
 * Field filtering for read and grep commands.
 *
 * Field hierarchy:
 *   user, assistant, thinking, system, summary (entry types)
 *   tool (all tools)
 *   tool:<name> (specific tool, e.g., tool:Bash)
 *   tool:<name>:input (tool input parameters)
 *   tool:<name>:result (tool output/result)
 *
 * More specific selectors override less specific ones.
 */

/**
 * Parse a comma-separated list of field specifiers.
 * Example: "user,tool:Bash:result" -> ["user", "tool:Bash:result"]
 */
export function parseFieldList(input: string): Array<string> {
  return input
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/**
 * Check if a field path matches the target.
 *
 * Handles special cases for shorthand patterns:
 * - "tool:result" matches all tool results (tool:Bash:result, tool:Edit:result, etc.)
 * - "tool:input" matches all tool inputs (tool:Bash:input, tool:Edit:input, etc.)
 *
 * Examples:
 *   matches("tool", "tool:Bash:result") -> true (tool is parent)
 *   matches("tool:Bash", "tool:Bash:result") -> true
 *   matches("tool:Bash:result", "tool:Bash") -> false
 *   matches("tool:Edit", "tool:Bash:result") -> false
 *   matches("tool:result", "tool:Bash:result") -> true (shorthand)
 *   matches("tool:input", "tool:Edit:input") -> true (shorthand)
 */
function matches(pattern: string, target: string): boolean {
  if (pattern === target) return true;

  // Standard parent-child matching
  if (target.startsWith(pattern + ':')) return true;

  // Handle shorthand patterns: tool:result, tool:input
  if (pattern === 'tool:result') {
    return target.endsWith(':result') && target.startsWith('tool:');
  }
  if (pattern === 'tool:input') {
    return target.endsWith(':input') && target.startsWith('tool:');
  }

  return false;
}

/**
 * Get the specificity (depth) of a field path.
 * More specific = higher number.
 */
function specificity(field: string): number {
  return field.split(':').length;
}

interface FieldRule {
  field: string;
  action: 'show' | 'hide';
  specificity: number;
}

/**
 * Default fields shown for read command.
 * Note: thinking shows word count only by default (handled separately).
 */
export const READ_DEFAULT_SHOWN = new Set([
  'user',
  'assistant',
  'thinking', // Word count only unless explicitly shown
  'tool',
  'system',
  'summary',
]);

/**
 * Default fields searched for grep command.
 * Excludes tool:result (can be added with --in).
 */
export const GREP_DEFAULT_SEARCH = new Set([
  'user',
  'assistant',
  'thinking',
  'tool:input',
  'system',
  'summary',
]);

/**
 * Filter for read command (show/hide semantics).
 */
export class ReadFieldFilter {
  private rules: Array<FieldRule>;

  constructor(show: Array<string>, hide: Array<string>) {
    this.rules = [];

    for (const field of show) {
      this.rules.push({ field, action: 'show', specificity: specificity(field) });
    }
    for (const field of hide) {
      this.rules.push({ field, action: 'hide', specificity: specificity(field) });
    }

    // Sort by specificity descending (most specific first)
    // For equal specificity, hide takes precedence over show
    this.rules.sort((a, b) => {
      if (b.specificity !== a.specificity) {
        return b.specificity - a.specificity;
      }
      // For equal specificity, hide comes before show
      return a.action === 'hide' ? -1 : b.action === 'hide' ? 1 : 0;
    });
  }

  /**
   * Check if a field should be shown.
   * Returns the action from the most specific matching rule, or default visibility.
   */
  shouldShow(field: string): boolean {
    for (const rule of this.rules) {
      if (matches(rule.field, field)) {
        return rule.action === 'show';
      }
    }
    // Check default visibility
    return this.isDefaultShown(field);
  }

  /**
   * Check if thinking should show full content (not just word count).
   * True if 'thinking' was explicitly in --show.
   */
  showFullThinking(): boolean {
    return this.rules.some((r) => r.field === 'thinking' && r.action === 'show');
  }

  private isDefaultShown(field: string): boolean {
    // Check if field or any parent is in defaults
    for (const def of READ_DEFAULT_SHOWN) {
      if (matches(def, field)) return true;
    }
    return false;
  }
}

/**
 * Filter for grep command (search scope semantics).
 */
export class GrepFieldFilter {
  private searchFields: Set<string>;

  constructor(searchIn: Array<string> | null) {
    if (searchIn === null || searchIn.length === 0) {
      // Use defaults
      this.searchFields = new Set(GREP_DEFAULT_SEARCH);
    } else {
      this.searchFields = new Set(searchIn);
    }
  }

  /**
   * Check if a field should be searched.
   */
  isSearchable(field: string): boolean {
    // Check if field matches any search field (exact or as child)
    for (const searchField of this.searchFields) {
      if (matches(searchField, field) || matches(field, searchField)) {
        return true;
      }
    }
    return false;
  }
}
