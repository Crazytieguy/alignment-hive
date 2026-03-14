import { useState } from "react";
import { createFileRoute, Link } from "@tanstack/react-router";

export const Route = createFileRoute("/_authenticated/install")({
  component: InstallPage,
});

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  return (
    <button
      type="button"
      onClick={() => {
        navigator.clipboard.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
      }}
      className="absolute top-2 right-2 px-2 py-1 text-xs rounded bg-background/80 border border-border text-muted-foreground hover:text-foreground transition-colors"
    >
      {copied ? "Copied!" : "Copy"}
    </button>
  );
}

function InstallPage() {
  return (
    <div className="min-h-screen flex flex-col items-center pt-16 pb-24 px-4">
      <div className="w-full max-w-lg space-y-8">
        <div>
          <h1 className="text-3xl font-semibold tracking-tight">
            Install alignment-hive
          </h1>
          <p className="text-muted-foreground mt-2">
            The install script adds the plugin marketplace, authenticates you,
            and lets you choose which projects to share.
          </p>
        </div>

        <div className="space-y-4">
          <h2 className="text-lg font-medium">1. Install Claude Code</h2>
          <p className="text-sm text-muted-foreground">
            Skip this if you already have Claude Code installed.
          </p>
          <pre className="relative p-3 bg-muted rounded-lg text-sm overflow-x-auto">
            curl -fsSL https://claude.ai/install.sh | bash
            <CopyButton text="curl -fsSL https://claude.ai/install.sh | bash" />
          </pre>
        </div>

        <div className="space-y-4">
          <h2 className="text-lg font-medium">2. Run the install script</h2>
          <pre className="relative p-3 bg-muted rounded-lg text-sm overflow-x-auto">
            curl -fsSL https://alignment-hive.com/install.sh | bash
            <CopyButton text="curl -fsSL https://alignment-hive.com/install.sh | bash" />
          </pre>
          <p className="text-sm text-muted-foreground">
            This will authenticate you and let you select which projects to
            share sessions from.
          </p>
        </div>

        <div className="space-y-4">
          <h2 className="text-lg font-medium">3. Set up your project</h2>
          <p className="text-sm text-muted-foreground">
            Open Claude Code in a project directory and run:
          </p>
          <pre className="relative p-3 bg-muted rounded-lg text-sm overflow-x-auto">
            /hive:align
            <CopyButton text="/hive:align" />
          </pre>
          <p className="text-sm text-muted-foreground">
            This walks you through tooling recommendations and enables
            per-project session sharing.
          </p>
        </div>

        <div className="pt-4 border-t text-sm text-muted-foreground space-y-2">
          <p>
            Questions? Contact <strong>Yoav Tzfati</strong> on Slack in{" "}
            <code className="bg-muted px-1 rounded text-xs">#ai-tools</code>.
          </p>
          <p>
            <Link
              to="/consent"
              className="text-primary underline underline-offset-4 hover:text-primary/80 transition-colors"
            >
              Manage data sharing preferences
            </Link>
          </p>
        </div>
      </div>
    </div>
  );
}
