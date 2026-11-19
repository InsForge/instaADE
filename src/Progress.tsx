import { dismissProgress } from "./api";
import ErrorNotice from "./ErrorNotice";
import type { Run } from "./SandboxList";
import type { AppState } from "./types";

// The back end's steps, by the place they take in the list below.
const PLACE: Record<string, number> = { connecting: 0, creating: 0, waking: 0, installing: 1, pairing: 2, done: 3 };

interface Props {
  state: AppState;
  run: Run;
  /** Connects a sandbox that exists: Retry, and Connect on a free-limit error. */
  onConnect: (id: string) => void;
}

export default function Progress({ state, run, onConnect }: Props) {
  const p = state.progress;
  if (!p) return null;
  const current = PLACE[p.step] ?? 0;
  // A sandbox that exists is not created again: its first step is to wake it.
  const first = p.kind === "connect" ? "Wake sandbox" : "Create sandbox";
  const labels = [first, `Install T3 ${state.t3 ?? ""}`.trim(), "Pair", "Confirm in T3 Code"];
  const machineId = p.machineId;
  return (
    <div className="progress">
      <ol className="steps">
        {labels.map((l, i) => (
          <li key={l} className={p.step === "done" || i < current ? "done" : i === current ? "now" : ""}>
            {l}
          </li>
        ))}
      </ol>
      {p.error && (
        <ErrorNotice
          error={p.error}
          sandboxes={state.sandboxes}
          onConnect={onConnect}
          // Retry connects the sandbox that already exists. It never creates another.
          onRetry={machineId ? () => onConnect(machineId) : undefined}
          onDismiss={() => run(() => dismissProgress())}
        />
      )}
    </div>
  );
}
