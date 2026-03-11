export interface HookInput {
  transcriptPath?: string;
  cwd?: string;
}

async function readStdin(): Promise<string | null> {
  if (process.stdin.isTTY) return null;

  return new Promise((resolve) => {
    let data = '';
    process.stdin.setEncoding('utf-8');
    process.stdin.on('data', (chunk) => {
      data += chunk;
    });
    process.stdin.on('end', () => {
      resolve(data || null);
    });
    process.stdin.on('error', () => {
      resolve(null);
    });
    process.stdin.resume();
  });
}

export async function readHookInput(): Promise<HookInput> {
  const input = await readStdin();
  if (!input) return {};

  try {
    const data = JSON.parse(input) as Record<string, unknown>;
    return {
      transcriptPath: typeof data.transcript_path === 'string' ? data.transcript_path : undefined,
      cwd: typeof data.cwd === 'string' ? data.cwd : undefined,
    };
  } catch {
    return {};
  }
}
