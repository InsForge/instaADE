import type { ReactNode } from "react";
import { openUrl } from "./api";
import type { ErrorView, SandboxView } from "./types";

interface Props {
  error: ErrorView;
  sandboxes: SandboxView[];
  onConnect: (id: string) => void;
  /** Present when a retry makes sense for this error. */
  onRetry?: () => void;
  /** Present when the notice can be closed. */
  onDismiss?: () => void;
}

/** One place that turns an ErrorView into copy, by kind and never by message text. */
export default function ErrorNotice({ error, sandboxes, onConnect, onRetry, onDismiss }: Props) {
  const dismiss = onDismiss && (
    <button className="btn icon" aria-label="Dismiss" onClick={onDismiss}>
      ×
    </button>
  );
  switch (error.kind) {
    case "cooldown":
      return (
        <div className="notice info" role="status">
          <span>Cooling down, ready in about {error.minutes ?? "a few"} min</span>
          {dismiss}
        </div>
      );
    case "freeLimit": {
      const unconnected = sandboxes.find((s) => !s.connected);
      return (
        <div className="notice warn" role="alert">
          <span>The free plan has one sandbox</span>
          {unconnected && (
            <button className="btn" onClick={() => onConnect(unconnected.id)}>
              Connect to T3 Code
            </button>
          )}
          {dismiss}
        </div>
      );
    }
    case "t3Busy":
      return (
        <div className="notice warn" role="alert">
          <span>T3 is busy on this sandbox. Try again when the agent is idle.</span>
          {dismiss}
        </div>
      );
    case "setup":
      return (
        <div className="notice danger" role="alert">
          <pre className="mono">{error.message}</pre>
          {onRetry && (
            <button className="btn" onClick={onRetry}>
              Retry
            </button>
          )}
          {dismiss}
        </div>
      );
    default:
      return (
        <div className="notice danger" role="alert">
          <span>{error.message}</span>
          {dismiss}
        </div>
      );
  }
}

export function ExternalLink({ url, children }: { url: string; children: ReactNode }) {
  return (
    <a
      href={url}
      onClick={(e) => {
        e.preventDefault();
        void openUrl(url).catch(() => {});
      }}
    >
      {children}
    </a>
  );
}
