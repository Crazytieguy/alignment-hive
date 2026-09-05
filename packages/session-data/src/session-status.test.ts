import { describe, expect, test } from 'bun:test';
import {
  canExclude,
  canUpload,
  formatRemaining,
  formatSessionStatus,
  getStatusColor,
  isEligibleForAutoUpload,
} from './session-status';

describe('eligibility rules', () => {
  test('canExclude', () => {
    expect(canExclude({ type: 'ready' }, false)).toBe(true);
    expect(canExclude({ type: 'pending', remainingMs: 1000 }, false)).toBe(true);
    expect(canExclude({ type: 'snoozed' }, false)).toBe(true);
    expect(canExclude({ type: 'excluded' }, false)).toBe(false);
    expect(canExclude({ type: 'uploaded' }, false)).toBe(false);
  });

  test('canExclude refuses any state with a partial upload (data may already be on the server)', () => {
    expect(canExclude({ type: 'ready' }, true)).toBe(false);
    expect(canExclude({ type: 'pending', remainingMs: 1000 }, true)).toBe(false);
    expect(canExclude({ type: 'snoozed' }, true)).toBe(false);
  });

  test('canUpload', () => {
    expect(canUpload({ type: 'ready' })).toBe(true);
    expect(canUpload({ type: 'pending', remainingMs: 1000 })).toBe(true);
    expect(canUpload({ type: 'snoozed' })).toBe(false);
    expect(canUpload({ type: 'excluded' })).toBe(false);
    expect(canUpload({ type: 'uploaded' })).toBe(false);
  });

  test('isEligibleForAutoUpload', () => {
    expect(isEligibleForAutoUpload({ type: 'ready' })).toBe(true);
    expect(isEligibleForAutoUpload({ type: 'pending', remainingMs: 1000 })).toBe(false);
    expect(isEligibleForAutoUpload({ type: 'snoozed' })).toBe(false);
    expect(isEligibleForAutoUpload({ type: 'excluded' })).toBe(false);
    expect(isEligibleForAutoUpload({ type: 'uploaded' })).toBe(false);
  });
});

describe('labels', () => {
  test('a partial upload overrides the label and colour for pre-upload states only', () => {
    expect(formatSessionStatus({ type: 'ready' }, true)).toBe('partially uploaded');
    expect(formatSessionStatus({ type: 'pending', remainingMs: 1000 }, true)).toBe('partially uploaded');
    expect(formatSessionStatus({ type: 'snoozed' }, true)).toBe('partially uploaded');
    expect(formatSessionStatus({ type: 'uploaded' }, true)).toBe('uploaded');
    expect(formatSessionStatus({ type: 'excluded' }, true)).toBe('excluded');
    expect(getStatusColor({ type: 'ready' }, true)).toBe('yellow');
  });

  test('remaining time rounds minutes up', () => {
    expect(formatRemaining(90 * 60 * 1000)).toBe('1h 30m');
    expect(formatRemaining(29 * 60 * 1000 + 1)).toBe('30m');
  });
});
