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
  help: boolean;
}

/** A malformed call: an unknown flag, a value flag without a value, or a value given to a bool flag. */
export class ArgsError extends Error {}

export interface ArgsMessages {
  unknownFlag: (flag: string) => string;
  needsValue: (flag: string) => string;
  noValue: (flag: string) => string;
}

/**
 * Flags anywhere, `--flag=value`, short-flag clusters (`-ic`, `-m5`), and `--` before leading-dash
 * positionals; `-h`/`--help` sets `help`. Throws ArgsError on a malformed call.
 */
export function parseFlags(spec: FlagSpec, args: Array<string>, messages: ArgsMessages = errors): ParsedArgs {
  const flags = new Map<string, string | true>();
  const positional: Array<string> = [];
  let help = false;
  const set = (name: string, inline: string | undefined, next: () => string | undefined) => {
    if (spec.bool.includes(name)) {
      if (inline !== undefined) throw new ArgsError(messages.noValue(name));
      flags.set(name, true);
    } else if (spec.value.includes(name)) {
      const value = inline ?? next();
      if (!value) throw new ArgsError(messages.needsValue(name));
      flags.set(name, value);
    } else throw new ArgsError(messages.unknownFlag(name));
  };
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    // A dash-led next argument is the next flag, never this one's value.
    const next = () => (args[i + 1]?.startsWith('-') ? undefined : args[++i]);
    if (a === '--') {
      positional.push(...args.slice(i + 1));
      break;
    }
    if (a === '--help' || a === '-h') help = true;
    else if (a.startsWith('--')) {
      const eq = a.indexOf('=');
      set(eq < 0 ? a : a.slice(0, eq), eq < 0 ? undefined : a.slice(eq + 1), next);
    } else if (a.startsWith('-') && a.length > 1 && !/^-\d/.test(a)) {
      for (let j = 1; j < a.length; j++) {
        const flag = `-${a[j]}`;
        if (flag === '-h') help = true;
        else if (spec.value.includes(flag)) {
          set(flag, a.slice(j + 1) || undefined, next);
          break;
        } else set(flag, undefined, () => undefined);
      }
    } else positional.push(a);
  }
  return { flags, positional, help };
}

/**
 * parseFlags for the upload commands: `-h`/`--help` prints `usage` and returns 0, and a malformed
 * call is a usage error. A number return means: exit with it, run nothing.
 */
export function parseCommandArgs(spec: FlagSpec, args: Array<string>, usage: string): ParsedArgs | number {
  try {
    const parsed = parseFlags(spec, args);
    if (!parsed.help) return parsed;
    console.log(usage);
    return 0;
  } catch (error) {
    if (error instanceof ArgsError) return usageError(error.message, usage);
    throw error;
  }
}

/** Prints a usage error, then the usage, to stderr; returns the usage-error exit code. */
export function usageError(message: string, usage: string): number {
  printError(message);
  console.error(usage);
  return 2;
}
