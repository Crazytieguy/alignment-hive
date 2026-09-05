export interface HookInput {
  cwd?: string;
  hookEventName?: string;
  source?: string;
}

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
    hookEventName: typeof data.hook_event_name === 'string' ? data.hook_event_name : undefined,
    source: typeof data.source === 'string' ? data.source : undefined,
  };
}
