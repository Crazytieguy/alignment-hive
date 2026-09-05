// ESC escapes survive both a plain terminal and JSON.stringify (escaped as a \u sequence) in a hook systemMessage.
export const colors = {
  red: (s: string) => `\x1b[31m${s}\x1b[0m`,
  green: (s: string) => `\x1b[32m${s}\x1b[0m`,
  yellow: (s: string) => `\x1b[33m${s}\x1b[0m`,
  blue: (s: string) => `\x1b[34m${s}\x1b[0m`,
  dim: (s: string) => `\x1b[2m${s}\x1b[0m`,
  boldMagenta: (s: string) => `\x1b[1;35m${s}\x1b[0m`,
  boldBlue: (s: string) => `\x1b[1;34m${s}\x1b[0m`,
};

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
