// The command line of an entry point (bench.ts, climb/climb.ts, build/corpus.ts): `--name value` flags and switches.

/** The arguments after the script. */
export const args = process.argv.slice(2);

/** The value after `name`, else `fallback` when `name` is absent. */
export function flag(name: string): string | undefined;
export function flag(name: string, fallback: string): string;
export function flag(name: string, fallback?: string): string | undefined {
  return args.includes(name) ? args[args.indexOf(name) + 1] : fallback;
}

/** Whether the switch `name` is given. */
export const has = (name: string): boolean => args.includes(name);
