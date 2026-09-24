import { describe, expect, test } from 'bun:test';
import { fileURLToPath } from 'node:url';

const hook = fileURLToPath(new URL('./session-start.sh', import.meta.url));
const session = '00000000-0000-4000-8000-000000000001';

function run(input: unknown) {
  return Bun.spawnSync(['bash', hook], { stdin: Buffer.from(JSON.stringify(input)) });
}

describe('debrief SessionStart', () => {
  for (const source of ['startup', 'resume', 'compact', 'fork']) {
    test(`returns the caller id on ${source}`, () => {
      const result = run({ session_id: session, source });
      expect(result.exitCode).toBe(0);
      expect(JSON.parse(result.stdout.toString())).toEqual({
        hookSpecificOutput: {
          hookEventName: 'SessionStart',
          additionalContext: `Debrief parent session id: ${session}`,
        },
      });
    });
  }

  for (const input of [{}, { session_id: '' }, { session_id: 'not-an-id' }, { session_id: [] }]) {
    test(`ignores malformed input ${JSON.stringify(input)}`, () => {
      const result = run(input);
      expect(result.exitCode).toBe(0);
      expect(result.stdout.toString()).toBe('');
      expect(result.stderr.toString()).toBe('');
    });
  }
});
