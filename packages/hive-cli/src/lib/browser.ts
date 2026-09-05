/** Open a URL in the default browser. False when no opener succeeded (the caller has printed the URL). */
export async function openBrowser(url: string): Promise<boolean> {
  const commands =
    process.platform === 'darwin' ? ['open'] : process.platform === 'linux' ? ['xdg-open', 'wslview'] : [];
  for (const cmd of commands) {
    try {
      if ((await Bun.spawn([cmd, url], { stdout: 'ignore', stderr: 'ignore' }).exited) === 0) return true;
    } catch {
      // command not installed: try the next one
    }
  }
  return false;
}
