import { runCommand } from '../lib/spawn';
import { reviewCliMessages } from '../lib/messages';
import { resolveReviewContext } from './git';

function version(value: string): Array<number> {
  const match = /^(?:hive\s+)?(\d+)\.(\d+)\.(\d+)$/.exec(value.trim());
  if (!match) throw new Error(reviewCliMessages.invalidVersion(value));
  const parts = match.slice(1).map(Number);
  if (parts.some((part) => !Number.isSafeInteger(part))) {
    throw new Error(reviewCliMessages.invalidVersion(value));
  }
  return parts;
}

export function meetsMinimum(actual: string, minimum: string): boolean {
  const actualParts = version(actual);
  const minimumParts = version(minimum);
  for (let index = 0; index < 3; index++) {
    if (actualParts[index] !== minimumParts[index]) {
      return actualParts[index] > minimumParts[index];
    }
  }
  return true;
}

export async function reviewPreflight(options: {
  minimumVersion: string;
  session: string;
  cwd?: string;
}): Promise<{ repository: boolean }> {
  version(options.minimumVersion);
  const binary = Bun.which('hive');
  if (!binary) throw new Error(reviewCliMessages.binaryMissing);
  const result = await runCommand([binary, '--version'], { cwd: options.cwd });
  const stdout = result.stdout.toString(), stderr = result.stderr.toString(), exit = result.exit;
  if (exit !== 0) throw new Error(reviewCliMessages.versionFailed(stderr.trim() || stdout.trim()));
  if (!meetsMinimum(stdout, options.minimumVersion)) {
    throw new Error(reviewCliMessages.binaryOld(stdout.trim(), options.minimumVersion));
  }
  const context = await resolveReviewContext({ session: options.session, cwd: options.cwd });
  return { repository: context.git !== null };
}
