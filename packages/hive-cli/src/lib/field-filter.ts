export function parseFieldList(input: string): Array<string> {
  return input
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

function matches(pattern: string, target: string): boolean {
  if (pattern === target) return true;
  if (target.startsWith(pattern + ':')) return true;

  if (pattern === 'tool:result') {
    return target.endsWith(':result') && target.startsWith('tool:');
  }
  if (pattern === 'tool:input') {
    return target.endsWith(':input') && target.startsWith('tool:');
  }

  return false;
}

function specificity(field: string): number {
  return field.split(':').length;
}

interface FieldRule {
  field: string;
  action: 'expand' | 'redact';
  specificity: number;
}

export const SEARCH_DEFAULT_FIELDS = new Set(['user', 'assistant', 'thinking', 'tool:input', 'system', 'summary']);

export class ReadFieldFilter {
  private rules: Array<FieldRule>;

  constructor(expand: Array<string>, redact: Array<string>) {
    this.rules = [];

    for (const field of expand) {
      this.rules.push({ field, action: 'expand', specificity: specificity(field) });
    }
    for (const field of redact) {
      this.rules.push({ field, action: 'redact', specificity: specificity(field) });
    }

    this.rules.sort((a, b) => {
      if (b.specificity !== a.specificity) return b.specificity - a.specificity;
      if (a.action === 'redact' && b.action !== 'redact') return -1;
      if (b.action === 'redact' && a.action !== 'redact') return 1;
      return 0;
    });
  }

  /** Returns true when the field should be collapsed to a word count / redactedForm. */
  isRedacted(field: string, defaultRedacted?: boolean): boolean {
    for (const rule of this.rules) {
      if (matches(rule.field, field)) {
        return rule.action === 'redact';
      }
    }
    if (defaultRedacted !== undefined) return defaultRedacted;
    // Non-tool block types (user, assistant, thinking, system, summary) are shown by default.
    // Tool fields always provide defaultRedacted, so this fallback only applies to block types.
    return false;
  }

  /** Returns true only when an explicit --expand rule matches. Defaults don't count. */
  hasExplicitExpandRule(field: string): boolean {
    for (const rule of this.rules) {
      if (matches(rule.field, field)) {
        return rule.action === 'expand';
      }
    }
    return false;
  }
}

export class SelectFilter {
  private patterns: Array<string>;

  constructor(patterns: Array<string>) {
    this.patterns = patterns;
  }

  includes(blockType: string): boolean {
    return this.patterns.some((p) => matches(p, blockType));
  }
}

export class SearchFieldFilter {
  private searchFields: Set<string>;

  constructor(searchIn: Array<string> | null) {
    if (searchIn === null || searchIn.length === 0) {
      this.searchFields = new Set(SEARCH_DEFAULT_FIELDS);
    } else {
      this.searchFields = new Set(searchIn);
    }
  }

  isSearchable(field: string): boolean {
    for (const searchField of this.searchFields) {
      if (matches(searchField, field) || matches(field, searchField)) {
        return true;
      }
    }
    return false;
  }
}
