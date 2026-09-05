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
    // Redact first so the stable sort keeps 'redact wins' among equal specificity.
    this.rules = [
      ...redact.map((field) => ({ field, action: 'redact' as const, specificity: specificity(field) })),
      ...expand.map((field) => ({ field, action: 'expand' as const, specificity: specificity(field) })),
    ].sort((a, b) => b.specificity - a.specificity);
  }

  private firstRule(field: string): FieldRule | undefined {
    return this.rules.find((rule) => matches(rule.field, field));
  }

  /** Returns true when the field should be collapsed to a word count / redactedForm. */
  isRedacted(field: string, defaultRedacted?: boolean): boolean {
    const rule = this.firstRule(field);
    return rule ? rule.action === 'redact' : (defaultRedacted ?? false);
  }

  /** Returns true only when an explicit --expand rule matches. Defaults don't count. */
  hasExplicitExpandRule(field: string): boolean {
    return this.firstRule(field)?.action === 'expand';
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
    this.searchFields = new Set(searchIn === null || searchIn.length === 0 ? SEARCH_DEFAULT_FIELDS : searchIn);
  }

  /** Whether a probed field falls under one of the requested scopes (a scope never widens to its parents). */
  isSearchable(field: string): boolean {
    for (const searchField of this.searchFields) {
      if (matches(searchField, field)) return true;
    }
    return false;
  }
}
