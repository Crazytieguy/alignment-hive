import { execSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, join } from "node:path";

export const WORKOS_CLIENT_ID =
  process.env.HIVE_MIND_CLIENT_ID ?? "client_01KE10CZ6FFQB9TR2NVBQJ4AKV";

export const AUTH_DIR = join(homedir(), ".claude", "hive-mind");
export const AUTH_FILE = join(AUTH_DIR, "auth.json");

export async function getOrCreateCheckoutId(hiveMindDir: string) {
  const checkoutIdFile = join(hiveMindDir, "checkout-id");
  try {
    const id = await readFile(checkoutIdFile, "utf-8");
    return id.trim();
  } catch {
    const id = randomUUID();
    await mkdir(hiveMindDir, { recursive: true });
    await writeFile(checkoutIdFile, id);
    const gitignorePath = join(hiveMindDir, ".gitignore");
    try {
      const existing = await readFile(gitignorePath, "utf-8");
      if (!existing.includes("checkout-id")) {
        await writeFile(gitignorePath, `${existing.trimEnd()}\ncheckout-id\n`);
      }
    } catch {
      await writeFile(gitignorePath, "checkout-id\nsessions/\n");
    }
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

/**
 * Get a canonical project identifier from git remote.
 * Returns a normalized identifier like "github.com/user/repo" or falls back to directory basename.
 */
export function getCanonicalProjectName(cwd: string): string {
  try {
    const remoteUrl = execSync("git remote get-url origin", {
      cwd,
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
    }).trim();

    const canonical = remoteUrl
      .replace(/^git@/, "")
      .replace(/^https?:\/\//, "")
      .replace(":", "/")
      .replace(/\.git$/, "");

    return canonical;
  } catch {
    return basename(cwd);
  }
}

function getTranscriptsDirFile(hiveMindDir: string): string {
  return join(hiveMindDir, "transcripts-dir");
}

export async function saveTranscriptsDir(hiveMindDir: string, dir: string): Promise<void> {
  const file = getTranscriptsDirFile(hiveMindDir);
  await mkdir(hiveMindDir, { recursive: true });
  await writeFile(file, dir, "utf-8");
}

export async function loadTranscriptsDir(hiveMindDir: string): Promise<string | null> {
  try {
    const content = await readFile(getTranscriptsDirFile(hiveMindDir), "utf-8");
    return content.trim();
  } catch {
    return null;
  }
}