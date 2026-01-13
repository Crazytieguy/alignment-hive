import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

export const WORKOS_CLIENT_ID =
  process.env.HIVE_MIND_CLIENT_ID ?? "client_01KE10CYZ10VVZPJVRQBJESK1A";

export const AUTH_DIR = join(homedir(), ".claude", "hive-mind");
export const AUTH_FILE = join(AUTH_DIR, "auth.json");

export async function getCheckoutId(hiveMindDir: string) {
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
      await writeFile(gitignorePath, "checkout-id\n");
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
