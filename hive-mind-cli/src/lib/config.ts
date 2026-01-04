import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

export const WORKOS_CLIENT_ID =
  process.env.HIVE_MIND_CLIENT_ID ?? "client_01KE10CYZ10VVZPJVRQBJESK1A";

export const AUTH_DIR = join(homedir(), ".claude", "hive-mind");
export const AUTH_FILE = join(AUTH_DIR, "auth.json");
export const MACHINE_ID_FILE = join(AUTH_DIR, "machine-id");

/**
 * Get or create a persistent machine ID for anonymous tracking.
 * Generated on first call, persisted in ~/.claude/hive-mind/machine-id
 */
export async function getMachineId() {
  try {
    const id = await readFile(MACHINE_ID_FILE, "utf-8");
    return id.trim();
  } catch {
    const id = randomUUID();
    await mkdir(AUTH_DIR, { recursive: true });
    await writeFile(MACHINE_ID_FILE, id);
    return id;
  }
}

export function getShellConfig(): { file: string; sourceCmd: string } {
  const shell = process.env.SHELL ?? "/bin/bash";
  if (shell.includes("zsh")) {
    return { file: "~/.zshrc", sourceCmd: "source ~/.zshrc" };
  }
  if (shell.includes("bash")) {
    return { file: "~/.bashrc", sourceCmd: "source ~/.bashrc" };
  }
  if (shell.includes("fish")) {
    return {
      file: "~/.config/fish/config.fish",
      sourceCmd: "source ~/.config/fish/config.fish",
    };
  }
  return { file: "~/.profile", sourceCmd: "source ~/.profile" };
}
