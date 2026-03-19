export const colors = {
  red: (s: string) => `\x1b[31m${s}\x1b[0m`,
  green: (s: string) => `\x1b[32m${s}\x1b[0m`,
  yellow: (s: string) => `\x1b[33m${s}\x1b[0m`,
  blue: (s: string) => `\x1b[34m${s}\x1b[0m`,
};

/** ANSI formatting for hook systemMessage output. Uses \x1b codes which
 *  JSON.stringify escapes to \u001b — the only form that survives in systemMessage. */
export const hookColors = {
  bold: (s: string) => `\x1b[1m${s}\x1b[0m`,
  dim: (s: string) => `\x1b[2m${s}\x1b[0m`,
  magenta: (s: string) => `\x1b[35m${s}\x1b[0m`,
  boldMagenta: (s: string) => `\x1b[1;35m${s}\x1b[0m`,
  boldBlue: (s: string) => `\x1b[1;34m${s}\x1b[0m`,
  cyan: (s: string) => `\x1b[36m${s}\x1b[0m`,
  green: (s: string) => `\x1b[32m${s}\x1b[0m`,
};

/**
 * Calculate the padding string for continuation lines in hook output.
 * Aligns continuation content with the first line's content after the
 * "EventName:source says: " prefix that Claude Code prepends.
 */
export function hookContinuationPad(hookEventName?: string, source?: string): string {
  const prefix = source
    ? `${hookEventName ?? 'SessionStart'}:${source} says: `
    : `${hookEventName ?? 'SessionStart'} says: `;
  return ' '.repeat(prefix.length);
}

export function hookOutput(message: string): void {
  console.log(JSON.stringify({ systemMessage: message }));
}

export function printError(message: string): void {
  console.error(`${colors.red('Error:')} ${message}`);
}

export function printSuccess(message: string): void {
  console.log(colors.green(message));
}

export function printInfo(message: string): void {
  console.log(colors.blue(message));
}

export function printWarning(message: string): void {
  console.log(colors.yellow(message));
}
