import { dismissProgress } from "./api";
import { adeName } from "./ades";
import type { Mode } from "./ades";
import type { Run } from "./common";
import { errorCopy, errorTone, unconnectedFor } from "./ErrorNotice";
import type { AdeId, AppState, ProgressView } from "./types";

// The back end's steps, by the place they take in the flow.
const PLACE: Record<string, number> = { connecting: 0, creating: 0, waking: 0, installing: 1, pairing: 2, done: 3 };
const WIDTH = [20, 62, 88, 100];

export function stepText(p: ProgressView, state: AppState): string {
  const t3 = p.ade === null || p.ade === "t3";
  const name = p.ade ? adeName(state, p.ade) : "";
  switch (p.step) {
    case "installing":
      return t3 ? `Installing T3 ${state.t3 ?? ""}`.trim() : `Installing ${name}, then opening`;
    case "pairing":
      return `Pairing with ${name || "T3 Code"}`;
    case "done":
      return t3 ? "Confirm in T3 Code" : `Opening ${name}`;
    case "creating":
      return "Creating sandbox";
    default:
      // A sandbox that exists is not created again: its first step is to wake it.
      return p.kind === "connect" || p.step === "waking" ? "Waking sandbox" : "Creating sandbox";
  }
}

export function ProgressBar({ p }: { p: ProgressView }) {
  const pct = WIDTH[PLACE[p.step] ?? 0];
  return (
    <div className="bar" role="progressbar" aria-label="Setup progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
      <span style={{ width: `${pct}%` }} />
    </div>
  );
}

interface Props {
  state: AppState;
  run: Run;
  mode: Mode;
  /** Sets an ADE up on a sandbox that exists: Retry, and Connect on a free-limit error. */
  onConnect: (id: string, ade: AdeId) => void;
}

/** The progress of a create or connect: the step and a bar, or the error with Retry and Dismiss. */
export default function Progress({ state, run, mode, onConnect }: Props) {
  const p = state.progress;
  if (!p) return null;
  if (!p.error) {
    return (
      <div className="progress">
        <span className="line-text">{stepText(p, state)}</span>
        <ProgressBar p={p} />
      </div>
    );
  }
  const machineId = p.machineId;
  // Retry sets up the ADE the failed flow set up.
  const retryAde = p.ade ?? (mode === "instabox" ? null : mode);
  const unconnected = unconnectedFor(p.error, state.sandboxes, mode);
  const tone = errorTone(p.error);
  return (
    <div
      className={`progress line wrap ${tone === "danger" ? "red" : tone === "warn" ? "amber" : "muted"}`}
      role={tone === "info" ? "status" : "alert"}
    >
      <span className={`line-text${p.error.kind === "setup" ? " pre" : ""}`}>{errorCopy(p.error)}</span>
      {/* type="button": the New sandbox form shows this too, and these must not submit it. */}
      <span className="line-buttons">
        {machineId && retryAde && (
          // Retry connects the sandbox that already exists. It never creates another.
          <button type="button" className="btn sm" onClick={() => onConnect(machineId, retryAde)}>
            Retry
          </button>
        )}
        {unconnected && mode !== "instabox" && (
          <button type="button" className="btn sm" onClick={() => onConnect(unconnected.id, mode)}>
            Connect
          </button>
        )}
        <button type="button" className="btn sm" onClick={() => run(() => dismissProgress())}>
          Dismiss
        </button>
      </span>
    </div>
  );
}
