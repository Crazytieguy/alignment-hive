import { execSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { access, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, join } from "node:path";

export const WORKOS_CLIENT_ID =
  process.env.HIVE_MIND_CLIENT_ID ?? "client_01KE10CZ6FFQB9TR2NVBQJ4AKV";

export const AUTH_DIR = join(homedir(), ".claude", "hive-mind");
export const AUTH_FILE = join(AUTH_DIR, "auth.json");

export async function ensureHiveMindDir(hiveMindDir: string) {
  await mkdir(hiveMindDir, { recursive: true });
  const gitignorePath = join(hiveMindDir, ".gitignore");
  try {
    await access(gitignorePath);
  } catch {
    await writeFile(gitignorePath, "*\n");
  }
}

export async function getOrCreateCheckoutId(hiveMindDir: string) {
  const checkoutIdFile = join(hiveMindDir, "checkout-id");
  try {
    const id = await readFile(checkoutIdFile, "utf-8");
    return id.trim();
  } catch {
    const id = randomUUID();
    await ensureHiveMindDir(hiveMindDir);
    await writeFile(checkoutIdFile, id);
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

/**
 * Check if the given directory is a git worktree (vs main repo).
 * In a worktree, .git is a file pointing to the main repo's .git/worktrees/<name>.
 * In a main repo, .git is a directory.
 */
export async function isWorktree(cwd: string): Promise<boolean> {
  try {
    const gitPath = join(cwd, ".git");
    const gitStat = await stat(gitPath);
    return gitStat.isFile();
  } catch {
    return false;
  }
}

/**
 * Get the main worktree path from `git worktree list`.
 * Returns null if not in a git repo or if git command fails.
 */
export function getMainWorktreePath(cwd: string): string | null {
  try {
    const output = execSync("git worktree list --porcelain", {
      cwd,
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
    });

    // First "worktree <path>" line is always the main worktree
    const match = output.match(/^worktree (.+)$/m);
    return match?.[1] ?? null;
  } catch {
    return null;
  }
}

function getTranscriptsDirsFile(hiveMindDir: string): string {
  return join(hiveMindDir, "transcripts-dirs");
}

/**
 * Load all transcripts directories from the transcripts-dirs file.
 * Returns empty array if file doesn't exist.
 */
export async function loadTranscriptsDirs(hiveMindDir: string): Promise<Array<string>> {
  try {
    const content = await readFile(getTranscriptsDirsFile(hiveMindDir), "utf-8");
    return content
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
  } catch {
    return [];
  }
}

/**
 * Add a transcripts directory to the transcripts-dirs file.
 * Deduplicates entries.
 */
export async function addTranscriptsDir(hiveMindDir: string, dir: string): Promise<void> {
  await ensureHiveMindDir(hiveMindDir);
  const existing = await loadTranscriptsDirs(hiveMindDir);
  if (!existing.includes(dir)) {
    existing.push(dir);
    await writeFile(getTranscriptsDirsFile(hiveMindDir), existing.join("\n") + "\n", "utf-8");
  }
}

