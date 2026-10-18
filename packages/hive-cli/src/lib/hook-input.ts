export interface HookInput {
  cwd?: string;
}

/** A hook's stdin JSON, as far as the CLI reads it; empty in a terminal or on unparseable input. */
export async function readHookInput(): Promise<HookInput> {
  // A manual `hive session-start` in a terminal must not block on stdin.
  if (process.stdin.isTTY) return {};
  let data: Record<string, unknown>;
  try {
    data = JSON.parse(await Bun.stdin.text());
  } catch {
    return {};
  }
  return {
    cwd: typeof data.cwd === 'string' ? data.cwd : undefined,
  };
}
