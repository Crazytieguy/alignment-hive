import { homedir } from "os";
import { join } from "path";

export const WORKOS_CLIENT_ID =
  process.env.HIVE_MIND_CLIENT_ID ?? "client_01KE10CYZ10VVZPJVRQBJESK1A";

export const AUTH_DIR = join(homedir(), ".claude", "hive-mind");
export const AUTH_FILE = join(AUTH_DIR, "auth.json");

export function getShellConfig(): { file: string; sourceCmd: string } {
  const shell = process.env.SHELL ?? "/bin/bash";
  if (shell.includes("zsh")) {
    return { file: "~/.zshrc", sourceCmd: "source ~/.zshrc" };
  }
  if (shell.includes("bash")) {
    return { file: "~/.bashrc", sourceCmd: "source ~/.bashrc" };
  }
  if (shell.includes("fish")) {
    return { file: "~/.config/fish/config.fish", sourceCmd: "source ~/.config/fish/config.fish" };
  }
  return { file: "~/.profile", sourceCmd: "source ~/.profile" };
}
