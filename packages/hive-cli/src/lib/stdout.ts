import { writeSync } from 'node:fs';
import { requestContext } from './request-context';

/**
 * Output lines for a piped reader. process.stdout buffers what a full pipe cannot take yet, and
 * process.exit drops that buffer (a slow reader such as jq got a cut-off line), so each line goes
 * to fd 1 whole, synchronously, waiting while the pipe is full. A reader that goes away (`| head`)
 * ends the command quietly. Within a request (request-context.ts), lines go to the request.
 */
export const stdout = {
  write(text: string): void {
    const request = requestContext();
    if (request) {
      request.stdout.push(text);
      return;
    }
    const bytes = Buffer.from(text);
    for (let at = 0; at < bytes.length; ) {
      try {
        at += writeSync(1, bytes, at);
      } catch (error) {
        const code = (error as NodeJS.ErrnoException).code;
        if (code === 'EPIPE') process.exit(0);
        if (code !== 'EAGAIN') throw error;
        Bun.sleepSync(1);
      }
    }
  },
};

export function writeLine(line: string): void {
  stdout.write(`${line}\n`);
}

/** One JSON object per stderr line, so that `2>&1 | jq` still parses; never colored (console.error is, under FORCE_COLOR). */
export function stderrRow(row: Record<string, string>): void {
  const line = `${JSON.stringify(row)}\n`;
  const request = requestContext();
  if (request) request.stderr.push(line);
  else process.stderr.write(line);
}
