import { dismissProgress } from "./api";
import type { Run } from "./common";
import { errorCopy, errorTone, unconnectedFor } from "./ErrorNotice";
import type { AppState, ProgressView } from "./types";

// The back end's steps, by the place they take in the flow.
const PLACE: Record<string, number> = { connecting: 0, creating: 0, waking: 0, installing: 1, pairing: 2, done: 3 };
const WIDTH = [20, 62, 88, 100];

export function stepText(p: ProgressView, t3: string | null): string {
  switch (p.step) {
    case "installing":
      return `Installing T3 ${t3 ?? ""}`.trim();
    case "pairing":
      return "Pairing with T3 Code";
    case "done":
      return "Confirm in T3 Code";
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
  /** Connects a sandbox that exists: Retry, and Connect on a free-limit error. */
  onConnect: (id: string) => void;
}

/** The progress of a create or connect: the step and a bar, or the error with Retry and Dismiss. */
export default function Progress({ state, run, onConnect }: Props) {
  const p = state.progress;
  if (!p) return null;
  if (!p.error) {
    return (
      <div className="progress">
        <span className="line-text">{stepText(p, state.t3)}</span>
        <ProgressBar p={p} />
      </div>
    );
  }
  const machineId = p.machineId;
  const unconnected = unconnectedFor(p.error, state.sandboxes);
  const tone = errorTone(p.error);
  return (
    <div className={`progress line wrap ${tone === "danger" ? "red" : tone === "warn" ? "amber" : "muted"}`} role="alert">
      <span className={`line-text${p.error.kind === "setup" ? " pre" : ""}`}>{errorCopy(p.error)}</span>
      <span className="line-buttons">
        {machineId && (
          // Retry connects the sandbox that already exists. It never creates another.
          <button className="btn sm" onClick={() => onConnect(machineId)}>
            Retry
          </button>
        )}
        {unconnected && (
          <button className="btn sm" onClick={() => onConnect(unconnected.id)}>
            Connect
          </button>
        )}
        <button className="btn sm" onClick={() => run(() => dismissProgress())}>
          Dismiss
        </button>
      </span>
    </div>
  );
}
