import { errors } from './messages';
import { printError } from './output';

/** A whole non-negative integer, or null: "500oops", "1e3" and "1.5" are all rejected. */
export function parseWholeNumber(value: string): number | null {
  if (!/^\d+$/.test(value)) return null;
  const n = Number(value);
  return Number.isSafeInteger(n) ? n : null;
}

export interface FlagSpec {
  bool: Array<string>;
  value: Array<string>;
}

export interface ParsedArgs {
  flags: Map<string, string | true>;
  positional: Array<string>;
}

/**
 * Strict parsing for the upload commands. `-h`/`--help` prints `usage` and returns 0; an unknown
 * flag or a value flag without a value is a usage error. A number return means: exit with it,
 * run nothing.
 */
export function parseCommandArgs(spec: FlagSpec, args: Array<string>, usage: string): ParsedArgs | number {
  const flags = new Map<string, string | true>();
  const positional: Array<string> = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === '--help' || a === '-h') {
      console.log(usage);
      return 0;
    }
    if (!a.startsWith('-')) {
      positional.push(a);
      continue;
    }
    const eq = a.indexOf('=');
    const name = eq < 0 ? a : a.slice(0, eq);
    const inline = eq < 0 ? undefined : a.slice(eq + 1);
    if (spec.bool.includes(name)) {
      if (inline !== undefined) return usageError(errors.noValue(name), usage);
      flags.set(name, true);
    } else if (spec.value.includes(name)) {
      // A dash-led next argument is the next flag, never this one's value.
      const value = inline ?? (args[i + 1]?.startsWith('-') ? undefined : args[++i]);
      if (!value) return usageError(errors.needsValue(name), usage);
      flags.set(name, value);
    } else return usageError(errors.unknownFlag(name), usage);
  }
  return { flags, positional };
}

/** Prints a usage error, then the usage, to stderr; returns the usage-error exit code. */
export function usageError(message: string, usage: string): number {
  printError(message);
  console.error(usage);
  return 2;
}
