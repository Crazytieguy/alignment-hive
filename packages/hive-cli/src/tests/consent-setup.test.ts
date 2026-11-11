import { existsSync } from 'node:fs';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import * as realPrompts from '@inquirer/prompts';
import * as realAuth from '../lib/auth';
import * as realConvex from '../lib/convex';
import * as realDiscovery from '../lib/transcript-discovery';
import { mockForFile } from './file-mock';

const updates: Array<{ identifier: unknown; sessionSharing: boolean }> = [];
let ticked: Array<string> = [];
let project = '';

mockForFile('../lib/auth', realAuth, { getAuthData: () => Promise.resolve({ accessToken: 'test' }) });
mockForFile('../lib/convex', realConvex, {
  getConsentStatus: () => Promise.resolve({ hasConsent: true, sessionSharing: true }),
  getProjectSharing: () => Promise.resolve([]),
  updateProjectSharing: (changes: typeof updates) => {
    updates.push(...changes);
    return Promise.resolve();
  },
});
mockForFile('../lib/transcript-discovery', realDiscovery, { extractCwd: () => project });
mockForFile('@inquirer/prompts', realPrompts, { checkbox: () => Promise.resolve(ticked) });

const { consentSetup } = await import('../commands/consent-setup');
const { statePaths } = await import('../lib/config');

let home = '';
let originalHome: Record<string, string | undefined> = {};

beforeAll(async () => {
  home = await realpath(await mkdtemp(join(tmpdir(), 'hive-consent-home-')));
  project = join(home, 'work', 'proj');
  await mkdir(project, { recursive: true });
  await mkdir(join(home, '.claude', 'projects', '-work-proj'), { recursive: true });
  originalHome = { HOME: process.env.HOME, USERPROFILE: process.env.USERPROFILE };
  process.env.HOME = home;
  process.env.USERPROFILE = home;
});

afterAll(async () => {
  for (const [name, value] of Object.entries(originalHome)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  await rm(home, { recursive: true, force: true });
});

describe('consent setup', () => {
  test('re-enabling a project clears the local sharing-disabled marker', async () => {
    const marker = statePaths(join(project, '.claude', 'hive')).sharingDisabled;
    await mkdir(join(project, '.claude', 'hive'), { recursive: true });
    await writeFile(marker, '');
    ticked = [project];

    expect(await consentSetup()).toBe(0);

    expect(updates).toEqual([{ identifier: expect.anything(), sessionSharing: true }]);
    expect(existsSync(marker)).toBe(false);
  });
});
