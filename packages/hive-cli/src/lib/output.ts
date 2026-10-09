// NO_COLOR (no-color.org) leaves the text plain: the hive plugin's band sets it, since it shows
// the CLI's words in a toast.
const paint =
  (code: string) =>
  (s: string): string =>
    process.env.NO_COLOR ? s : `\x1b[${code}m${s}\x1b[0m`;

export const colors = {
  red: paint('31'),
  green: paint('32'),
  yellow: paint('33'),
  blue: paint('34'),
  dim: paint('2'),
  boldMagenta: paint('1;35'),
  boldBlue: paint('1;34'),
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
