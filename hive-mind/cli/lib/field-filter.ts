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
  action: 'show' | 'hide';
  specificity: number;
}

export const READ_DEFAULT_SHOWN = new Set([
  'user',
  'assistant',
  'thinking',
  'tool',
  'system',
  'summary',
]);

export const GREP_DEFAULT_SEARCH = new Set([
  'user',
  'assistant',
  'thinking',
  'tool:input',
  'system',
  'summary',
]);

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

    this.rules.sort((a, b) => {
      if (b.specificity !== a.specificity) return b.specificity - a.specificity;
      if (a.action === 'hide' && b.action !== 'hide') return -1;
      if (b.action === 'hide' && a.action !== 'hide') return 1;
      return 0;
    });
  }

  shouldShow(field: string): boolean {
    for (const rule of this.rules) {
      if (matches(rule.field, field)) {
        return rule.action === 'show';
      }
    }
    return this.isDefaultShown(field);
  }

  showFullThinking(): boolean {
    return this.rules.some((r) => r.field === 'thinking' && r.action === 'show');
  }

  private isDefaultShown(field: string): boolean {
    for (const def of READ_DEFAULT_SHOWN) {
      if (matches(def, field)) return true;
    }
    return false;
  }
}

export class GrepFieldFilter {
  private searchFields: Set<string>;

  constructor(searchIn: Array<string> | null) {
    if (searchIn === null || searchIn.length === 0) {
      this.searchFields = new Set(GREP_DEFAULT_SEARCH);
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
