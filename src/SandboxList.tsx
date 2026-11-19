import { openInT3, pause, reinstallT3, resume, T3_DOWNLOAD_URL } from "./api";
import { ExternalLink } from "./ErrorNotice";
import type { AppState, SandboxView } from "./types";

/** Runs a command; a failure shows in the app. Resolves true when the command succeeded. */
export type Run = (f: () => Promise<unknown>) => Promise<boolean>;

const BUSY = ["creating", "waking", "pausing", "deleting"];
const STATE_LABEL: Record<string, string> = {
  creating: "Creating",
  running: "Running",
  paused: "Paused",
  stopped: "Stopped",
  waking: "Waking",
  pausing: "Pausing",
  error: "Error",
  deleting: "Deleting",
};

export const stateLabel = (s: string) => STATE_LABEL[s] ?? s;

/** The version line, shared by the list rows and the details view. */
export function T3Line({ s, desktop, run }: { s: SandboxView; desktop: string | null; run: Run }) {
  if (s.desktopBehind) {
    return (
      <span className="muted">
        This sandbox runs a newer T3. <ExternalLink url={T3_DOWNLOAD_URL}>Update T3 Code on this computer.</ExternalLink>
      </span>
    );
  }
  if (s.updatePending) {
    return (
      <span className="muted">
        T3 {s.t3Version ?? "unknown"} · T3 Code {desktop ?? "unknown"}{" "}
        <button className="btn small" onClick={() => run(() => reinstallT3(s.id))}>
          Update
        </button>
      </span>
    );
  }
  return s.t3Version ? <span className="muted">T3 {s.t3Version}</span> : null;
}

export function StateDot({ state }: { state: SandboxView["state"] }) {
  return <span className={`dot ${state}`} aria-hidden="true" />;
}

interface RowProps {
  s: SandboxView;
  state: AppState;
  run: Run;
  onConnect: (id: string) => void;
  onDetails: (id: string) => void;
}

function Row({ s, state, run, onConnect, onDetails }: RowProps) {
  const busy = BUSY.includes(s.state);
  return (
    <li className="row">
      <div className="row-main">
        <StateDot state={s.state} />
        <strong className="mono">{s.name}</strong>
        <span className="muted">{stateLabel(s.state)}</span>
        <span className="muted">{s.size}</span>
        <T3Line s={s} desktop={state.t3} run={run} />
        {s.t3CodeConnected && <span className="success">T3 Code connected · won't auto sleep</span>}
      </div>
      <div className="row-actions">
        {s.state === "running" && (
          <button className="btn" disabled={busy} onClick={() => run(() => pause(s.id))}>
            Pause
          </button>
        )}
        {(s.state === "paused" || s.state === "stopped") && (
          <button className="btn" disabled={busy} onClick={() => run(() => resume(s.id))}>
            Resume
          </button>
        )}
        {s.connected ? (
          <button className="btn" disabled={busy} onClick={() => run(() => openInT3(s.id))}>
            Open in T3 Code
          </button>
        ) : (
          <button className="btn" disabled={busy} onClick={() => onConnect(s.id)}>
            Connect to T3 Code
          </button>
        )}
        <button className="btn icon" aria-label={`Details for ${s.name}`} onClick={() => onDetails(s.id)}>
          ⋯
        </button>
      </div>
    </li>
  );
}

interface Props {
  state: AppState;
  run: Run;
  onNew: () => void;
  onConnect: (id: string) => void;
  onDetails: (id: string) => void;
}

export default function SandboxList({ state, run, onNew, onConnect, onDetails }: Props) {
  if (state.sandboxes.length === 0) {
    return (
      <div className="card empty">
        <p>No sandboxes yet. Create one and open it in T3 Code.</p>
        <button className="btn primary" onClick={onNew}>
          Create sandbox
        </button>
      </div>
    );
  }
  return (
    <ul className="card list">
      {state.sandboxes.map((s) => (
        <Row key={s.id} s={s} state={state} run={run} onConnect={onConnect} onDetails={onDetails} />
      ))}
    </ul>
  );
}
