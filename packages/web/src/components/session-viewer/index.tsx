import { Link } from "@tanstack/react-router";
import { SessionViewerFromUrl, formatSessionId } from "@alignment-hive/ui";

interface SessionViewerProps {
  url: string;
}

export function SessionViewer({ url }: SessionViewerProps) {
  return (
    <SessionViewerFromUrl
      url={url}
      renderAgentLink={(agentId) => (
        <Link
          to="/authorized/sessions/$sessionId"
          // Agent session rows are keyed `agent-<id>`; the transcript may give the bare id.
          params={{
            sessionId: agentId.startsWith("agent-")
              ? agentId
              : `agent-${agentId}`,
          }}
          className="font-mono text-xs text-primary hover:underline"
        >
          {formatSessionId(agentId)}
        </Link>
      )}
    />
  );
}
