import { join } from 'node:path';
import { config } from 'dotenv';

/**
 * Load env files from CWD into process.env.
 *
 * Only loads when ALIGNMENT_HIVE_DEV is set (embedded at build time in the dev
 * binary via --define). This prevents the production binary from accidentally
 * picking up staging config when running from the repo root.
 *
 * Loads .env.local first (per-dev overrides like CONVEX_URL), then .env
 * (shared staging defaults). Existing process.env vars are never overridden.
 */
export function loadEnvFiles(): void {
  if (!process.env.ALIGNMENT_HIVE_DEV) return;
  const cwd = process.cwd();
  config({ path: [join(cwd, '.env.local'), join(cwd, '.env')], quiet: true });
}
