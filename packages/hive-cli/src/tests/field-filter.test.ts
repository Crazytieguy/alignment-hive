import { describe, expect, test } from 'bun:test';
import {
  READ_DEFAULT_SHOWN,
  ReadFieldFilter,
  SEARCH_DEFAULT_FIELDS,
  SearchFieldFilter,
  SelectFilter,
  parseFieldList,
} from '../lib/field-filter';

describe('parseFieldList', () => {
  test('empty string returns empty array', () => {
    expect(parseFieldList('')).toEqual([]);
  });

  test('single field', () => {
    expect(parseFieldList('user')).toEqual(['user']);
  });

  test('multiple fields', () => {
    expect(parseFieldList('user,assistant,thinking')).toEqual(['user', 'assistant', 'thinking']);
  });

  test('trims whitespace', () => {
    expect(parseFieldList(' user , assistant ')).toEqual(['user', 'assistant']);
  });

  test('filters empty entries', () => {
    expect(parseFieldList('user,,assistant')).toEqual(['user', 'assistant']);
  });

  test('handles tool field paths', () => {
    expect(parseFieldList('tool:Bash:result,tool:Edit')).toEqual(['tool:Bash:result', 'tool:Edit']);
  });
});

describe('ReadFieldFilter', () => {
  describe('default visibility', () => {
    test('defaults are correct', () => {
      expect(READ_DEFAULT_SHOWN).toEqual(new Set(['user', 'assistant', 'thinking', 'tool', 'system', 'summary']));
    });

    test('empty filter does not redact defaults', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.isRedacted('user')).toBe(false);
      expect(filter.isRedacted('assistant')).toBe(false);
      expect(filter.isRedacted('thinking')).toBe(false);
      expect(filter.isRedacted('tool')).toBe(false);
      expect(filter.isRedacted('system')).toBe(false);
      expect(filter.isRedacted('summary')).toBe(false);
    });

    test('tool children inherit from tool default', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.isRedacted('tool:Bash')).toBe(false);
      expect(filter.isRedacted('tool:Bash:input')).toBe(false);
      expect(filter.isRedacted('tool:Bash:result')).toBe(false);
    });

    test('non-defaults are redacted', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.isRedacted('unknown')).toBe(true);
    });
  });

  describe('expand rules', () => {
    test('expand marks field as not redacted', () => {
      const filter = new ReadFieldFilter(['tool:result'], []);
      expect(filter.isRedacted('tool:result')).toBe(false);
      expect(filter.isRedacted('tool:Bash:result')).toBe(false);
    });

    test('hasExplicitExpandRule returns true for explicit expand', () => {
      const filter = new ReadFieldFilter(['thinking'], []);
      expect(filter.hasExplicitExpandRule('thinking')).toBe(true);
    });

    test('hasExplicitExpandRule returns false without explicit expand', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.hasExplicitExpandRule('thinking')).toBe(false);
    });

    test('hasExplicitExpandRule returns false for default-shown fields', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.hasExplicitExpandRule('tool:Bash:result')).toBe(false);
    });
  });

  describe('redact rules', () => {
    test('redact collapses field', () => {
      const filter = new ReadFieldFilter([], ['user']);
      expect(filter.isRedacted('user')).toBe(true);
    });

    test('redact tool redacts all tool children', () => {
      const filter = new ReadFieldFilter([], ['tool']);
      expect(filter.isRedacted('tool')).toBe(true);
      expect(filter.isRedacted('tool:Bash')).toBe(true);
      expect(filter.isRedacted('tool:Bash:input')).toBe(true);
    });

    test('redact specific tool only redacts that tool', () => {
      const filter = new ReadFieldFilter([], ['tool:Edit']);
      expect(filter.isRedacted('tool')).toBe(false);
      expect(filter.isRedacted('tool:Edit')).toBe(true);
      expect(filter.isRedacted('tool:Bash')).toBe(false);
    });
  });

  describe('specificity resolution', () => {
    test('more specific expand overrides less specific redact', () => {
      const filter = new ReadFieldFilter(['tool:Bash:result'], ['tool:result']);
      expect(filter.isRedacted('tool:result')).toBe(true);
      expect(filter.isRedacted('tool:Bash:result')).toBe(false);
      expect(filter.isRedacted('tool:Edit:result')).toBe(true);
    });

    test('more specific redact overrides less specific expand', () => {
      const filter = new ReadFieldFilter(['tool:result'], ['tool:Bash:result']);
      expect(filter.isRedacted('tool:result')).toBe(false);
      expect(filter.isRedacted('tool:Edit:result')).toBe(false);
      expect(filter.isRedacted('tool:Bash:result')).toBe(true);
    });

    test('equal specificity: redact wins', () => {
      // redact comes after expand in constructor, so redact should win for same specificity
      const filter = new ReadFieldFilter(['user'], ['user']);
      expect(filter.isRedacted('user')).toBe(true);
    });
  });
});

describe('SelectFilter', () => {
  test('includes matching block type', () => {
    const filter = new SelectFilter(['user', 'tool']);
    expect(filter.includes('user')).toBe(true);
    expect(filter.includes('tool:Bash')).toBe(true);
    expect(filter.includes('assistant')).toBe(false);
  });

  test('includes specific tool', () => {
    const filter = new SelectFilter(['tool:Bash']);
    expect(filter.includes('tool:Bash')).toBe(true);
    expect(filter.includes('tool:Edit')).toBe(false);
    expect(filter.includes('tool')).toBe(false);
  });

  test('includes with broad tool match', () => {
    const filter = new SelectFilter(['tool']);
    expect(filter.includes('tool:Bash')).toBe(true);
    expect(filter.includes('tool:Edit')).toBe(true);
    expect(filter.includes('user')).toBe(false);
  });
});

describe('SearchFieldFilter', () => {
  describe('default search fields', () => {
    test('defaults are correct', () => {
      expect(SEARCH_DEFAULT_FIELDS).toEqual(
        new Set(['user', 'assistant', 'thinking', 'tool:input', 'system', 'summary']),
      );
    });

    test('null searchIn uses defaults', () => {
      const filter = new SearchFieldFilter(null);
      expect(filter.isSearchable('user')).toBe(true);
      expect(filter.isSearchable('assistant')).toBe(true);
      expect(filter.isSearchable('thinking')).toBe(true);
      expect(filter.isSearchable('tool:input')).toBe(true);
      expect(filter.isSearchable('system')).toBe(true);
      expect(filter.isSearchable('summary')).toBe(true);
    });

    test('tool:result not searchable by default', () => {
      const filter = new SearchFieldFilter(null);
      expect(filter.isSearchable('tool:result')).toBe(false);
    });
  });

  describe('custom search fields', () => {
    test('empty array uses defaults', () => {
      const filter = new SearchFieldFilter([]);
      expect(filter.isSearchable('user')).toBe(true);
    });

    test('explicit fields replaces defaults', () => {
      const filter = new SearchFieldFilter(['user', 'assistant']);
      expect(filter.isSearchable('user')).toBe(true);
      expect(filter.isSearchable('assistant')).toBe(true);
      expect(filter.isSearchable('thinking')).toBe(false);
      expect(filter.isSearchable('system')).toBe(false);
    });

    test('can search tool:result when specified', () => {
      const filter = new SearchFieldFilter(['tool:result']);
      expect(filter.isSearchable('tool:result')).toBe(true);
      expect(filter.isSearchable('user')).toBe(false);
    });

    test('tool:Bash:input matches when tool:input specified', () => {
      const filter = new SearchFieldFilter(['tool:input']);
      expect(filter.isSearchable('tool:Bash:input')).toBe(true);
    });

    test('tool:input matches when tool:Bash:input specified', () => {
      const filter = new SearchFieldFilter(['tool:Bash:input']);
      expect(filter.isSearchable('tool:input')).toBe(true);
    });

    test('bare tool matches both inputs and results', () => {
      const filter = new SearchFieldFilter(['tool']);
      expect(filter.isSearchable('tool:input')).toBe(true);
      expect(filter.isSearchable('tool:result')).toBe(true);
      expect(filter.isSearchable('tool:Bash:input')).toBe(true);
      expect(filter.isSearchable('tool:Bash:result')).toBe(true);
      // Should not match non-tool fields
      expect(filter.isSearchable('user')).toBe(false);
      expect(filter.isSearchable('assistant')).toBe(false);
    });
  });
});
