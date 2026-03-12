import { config } from 'dotenv';
import { join } from 'node:path';

/**
 * Load .env and .env.local from CWD into process.env.
 * Earlier files in the array take priority.
 * Existing process.env vars are never overridden.
 *
 * Needed for compiled binaries which don't get bun's auto env loading.
 * In user projects these files won't exist (no-op).
 */
export function loadEnvFiles(): void {
  const cwd = process.cwd();
  config({ path: [join(cwd, '.env.local'), join(cwd, '.env')], quiet: true });
}
