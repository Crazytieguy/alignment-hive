import { describe, expect, test } from 'bun:test';
import {
  READ_DEFAULT_SHOWN,
  ReadFieldFilter,
  SEARCH_DEFAULT_FIELDS,
  SearchFieldFilter,
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

    test('empty filter shows defaults', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.shouldShow('user')).toBe(true);
      expect(filter.shouldShow('assistant')).toBe(true);
      expect(filter.shouldShow('thinking')).toBe(true);
      expect(filter.shouldShow('tool')).toBe(true);
      expect(filter.shouldShow('system')).toBe(true);
      expect(filter.shouldShow('summary')).toBe(true);
    });

    test('tool children inherit from tool default', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.shouldShow('tool:Bash')).toBe(true);
      expect(filter.shouldShow('tool:Bash:input')).toBe(true);
      expect(filter.shouldShow('tool:Bash:result')).toBe(true);
    });

    test('non-defaults are not shown', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.shouldShow('unknown')).toBe(false);
    });
  });

  describe('show rules', () => {
    test('show adds field to visibility', () => {
      const filter = new ReadFieldFilter(['tool:result'], []);
      expect(filter.shouldShow('tool:result')).toBe(true);
      expect(filter.shouldShow('tool:Bash:result')).toBe(true);
    });

    test('showFullThinking returns true when thinking in show', () => {
      const filter = new ReadFieldFilter(['thinking'], []);
      expect(filter.showFullThinking()).toBe(true);
    });

    test('showFullThinking returns false when thinking not in show', () => {
      const filter = new ReadFieldFilter([], []);
      expect(filter.showFullThinking()).toBe(false);
    });
  });

  describe('hide rules', () => {
    test('hide removes field from visibility', () => {
      const filter = new ReadFieldFilter([], ['user']);
      expect(filter.shouldShow('user')).toBe(false);
    });

    test('hide tool hides all tool children', () => {
      const filter = new ReadFieldFilter([], ['tool']);
      expect(filter.shouldShow('tool')).toBe(false);
      expect(filter.shouldShow('tool:Bash')).toBe(false);
      expect(filter.shouldShow('tool:Bash:input')).toBe(false);
    });

    test('hide specific tool only hides that tool', () => {
      const filter = new ReadFieldFilter([], ['tool:Edit']);
      expect(filter.shouldShow('tool')).toBe(true);
      expect(filter.shouldShow('tool:Edit')).toBe(false);
      expect(filter.shouldShow('tool:Bash')).toBe(true);
    });
  });

  describe('specificity resolution', () => {
    test('more specific show overrides less specific hide', () => {
      const filter = new ReadFieldFilter(['tool:Bash:result'], ['tool:result']);
      expect(filter.shouldShow('tool:result')).toBe(false);
      expect(filter.shouldShow('tool:Bash:result')).toBe(true);
      expect(filter.shouldShow('tool:Edit:result')).toBe(false);
    });

    test('more specific hide overrides less specific show', () => {
      const filter = new ReadFieldFilter(['tool:result'], ['tool:Bash:result']);
      expect(filter.shouldShow('tool:result')).toBe(true);
      expect(filter.shouldShow('tool:Edit:result')).toBe(true);
      expect(filter.shouldShow('tool:Bash:result')).toBe(false);
    });

    test('equal specificity uses last rule', () => {
      // hide comes after show in constructor, so hide should win for same specificity
      const filter = new ReadFieldFilter(['user'], ['user']);
      expect(filter.shouldShow('user')).toBe(false);
    });
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
