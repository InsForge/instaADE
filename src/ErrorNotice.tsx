import { adeName, hasAde } from "./ades";
import type { Mode } from "./ades";
import type { AdeId, AppState, ErrorView, SandboxView } from "./types";

interface Props {
  error: ErrorView;
  state: AppState;
  mode: Mode;
  onConnect: (id: string, ade: AdeId) => void;
  /** Present when a retry makes sense for this error. */
  onRetry?: () => void;
}

/** One place that turns an ErrorView into copy, by kind and never by message text. */
export function errorCopy(error: ErrorView): string {
  switch (error.kind) {
    case "cooldown":
      return `Cooling down, ready in about ${error.minutes ?? "a few"} min`;
    case "freeLimit":
      return "The free plan has one sandbox";
    case "t3Busy":
      return "T3 is busy on this sandbox. Try again when the agent is idle.";
    default:
      return error.message;
  }
}

export function errorTone(error: ErrorView): "info" | "warn" | "danger" {
  if (error.kind === "cooldown") return "info";
  if (error.kind === "freeLimit" || error.kind === "t3Busy") return "warn";
  return "danger";
}

/** In an ADE mode, the free-limit error offers Connect on the sandbox that does not have the ADE yet. */
export const unconnectedFor = (error: ErrorView, sandboxes: SandboxView[], mode: Mode) =>
  error.kind === "freeLimit" && mode !== "instabox" ? sandboxes.find((s) => !hasAde(s, mode)) : undefined;

/** The compact banner under the header. */
export default function ErrorNotice({ error, state, mode, onConnect, onRetry }: Props) {
  const tone = errorTone(error);
  const unconnected = unconnectedFor(error, state.sandboxes, mode);
  return (
    <div className={`banner ${tone}`} role={tone === "info" ? "status" : "alert"}>
      {error.kind === "setup" ? <pre className="banner-text">{error.message}</pre> : <span className="banner-text">{errorCopy(error)}</span>}
      {unconnected && mode !== "instabox" && (
        <button className="btn sm" onClick={() => onConnect(unconnected.id, mode)}>
          Connect to {adeName(state, mode)}
        </button>
      )}
      {error.kind === "setup" && onRetry && (
        <button className="btn sm" onClick={onRetry}>
          Retry
        </button>
      )}
    </div>
  );
}
