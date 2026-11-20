import type { MouseEvent } from "react";
import { agentSignIn, reinstallT3, T3_DOWNLOAD_URL } from "./api";
import type { Agent } from "./api";
import { AGENTS, agentKey, BUSY, cap, Line, openExternal, StateDot, stateLabel } from "./common";
import type { AgentCache, OnAgentStatus, Run } from "./common";
import { Dots, Plus } from "./icons";
import Progress, { ProgressBar, stepText } from "./Progress";
import { T3Missing } from "./Setup";
import type { AppState, SandboxView } from "./types";

/** What a create that has no sandbox yet looks like: the name and size the form sent. */
export interface PendingCreate {
  name: string;
  size: string;
}

/** The one line under a row, by priority. Agent lines come only from statuses the details view fetched. */
export type RowLine =
  | { kind: "progress" }
  | { kind: "update" }
  | { kind: "desktopBehind" }
  | { kind: "connect" }
  | { kind: "agent"; agent: Agent; label: string }
  | null;

export function rowLine(s: SandboxView, state: AppState, agents: AgentCache): RowLine {
  if (state.progress && state.progress.machineId === s.id) return { kind: "progress" };
  // A sandbox ahead of this Mac never offers Update.
  if (s.updatePending && !s.desktopBehind) return { kind: "update" };
  if (s.desktopBehind) return { kind: "desktopBehind" };
  if (!s.connected) return { kind: "connect" };
  if (s.state === "running") {
    const a = AGENTS.find((x) => agents[agentKey(s.id, x.id)] === false);
    if (a) return { kind: "agent", agent: a.id, label: a.label };
  }
  return null;
}

export function metaText(s: SandboxView) {
  return s.t3Version ? `${s.size} · T3 ${s.t3Version}` : s.size;
}

interface RowProps {
  s: SandboxView;
  state: AppState;
  run: Run;
  agents: AgentCache;
  menuOpen: boolean;
  onConnect: (id: string) => void;
  onDetails: (id: string) => void;
  onMenu: (id: string, anchor: HTMLElement) => void;
  onAgentStatus: OnAgentStatus;
}

function Row({ s, state, run, agents, menuOpen, onConnect, onDetails, onMenu, onAgentStatus }: RowProps) {
  const line = rowLine(s, state, agents);
  // A click anywhere on the row opens details, except on its buttons.
  const open = (e: MouseEvent) => {
    if ((e.target as HTMLElement).closest("button, a, select, input")) return;
    onDetails(s.id);
  };
  return (
    <li className="row" onClick={open}>
      <div className="row-top">
        <button className="row-open" onClick={() => onDetails(s.id)}>
          <StateDot state={s.state} />
          <span className="row-text">
            <span className="row-title">
              <span className="name">{s.name}</span>
              <span className="state">{stateLabel(s.state)}</span>
            </span>
            <span className="row-meta">{metaText(s)}</span>
          </span>
        </button>
        <button
          className="icon-btn row-more"
          aria-label={`More actions for ${s.name}`}
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          onClick={(e) => onMenu(s.id, e.currentTarget)}
        >
          <Dots />
        </button>
      </div>
      {line?.kind === "progress" && <Progress state={state} run={run} onConnect={onConnect} />}
      {line?.kind === "update" && (
        <Line tone="amber" text={state.t3 ? `T3 is older than this Mac's ${state.t3}` : "T3 is older than T3 Code on this Mac"}>
          <button className="btn sm" onClick={() => run(() => reinstallT3(s.id))}>
            Update
          </button>
        </Line>
      )}
      {line?.kind === "desktopBehind" && (
        <Line tone="amber" text="T3 Code on this Mac is older">
          <button className="btn sm" onClick={() => openExternal(T3_DOWNLOAD_URL)}>
            Download
          </button>
        </Line>
      )}
      {line?.kind === "connect" && (
        <Line text="Not in T3 Code yet">
          <button className="btn sm" disabled={BUSY.includes(s.state)} onClick={() => onConnect(s.id)}>
            Connect
          </button>
        </Line>
      )}
      {line?.kind === "agent" && (
        <Line tone="amber" text={`${line.label} needs a sign-in`}>
          <button
            className="btn sm"
            // No status check from the list: it would run a command on the sandbox.
            onClick={() => run(() => agentSignIn(s.id, line.agent)).then((ok) => ok && onAgentStatus(s.id, line.agent, null))}
          >
            Sign in
          </button>
        </Line>
      )}
    </li>
  );
}

/** A create in flight before its sandbox is in the list. */
function PendingRow({ state, run, pending, onConnect }: { state: AppState; run: Run; pending: PendingCreate | null; onConnect: (id: string) => void }) {
  const p = state.progress!;
  const name = p.kind === "create" && pending ? pending.name : "New sandbox";
  const size = p.kind === "create" ? pending?.size : undefined;
  const meta = p.error ? size : [size, stepText(p, state.t3)].filter(Boolean).join(" · ");
  return (
    <li className="row pending">
      <div className="row-top">
        <div className="row-open">
          <StateDot state={p.error ? "error" : "creating"} />
          <div className="row-text">
            <span className="row-title">
              <span className="name">{name}</span>
              <span className="state">{p.error ? "Failed" : p.kind === "create" ? "Creating" : "Connecting"}</span>
            </span>
            {meta && <span className="row-meta">{meta}</span>}
            {!p.error && <ProgressBar p={p} />}
          </div>
        </div>
      </div>
      {p.error && <Progress state={state} run={run} onConnect={onConnect} />}
    </li>
  );
}

function counts(sandboxes: SandboxView[]) {
  const n = (f: (s: SandboxView) => boolean) => sandboxes.filter(f).length;
  const parts: [number, string][] = [
    [n((s) => s.state === "running"), "running"],
    [n((s) => s.state === "paused"), "paused"],
    [n((s) => s.state === "stopped"), "stopped"],
  ];
  return parts
    .filter(([c]) => c > 0)
    .map(([c, l]) => `${c} ${l}`)
    .join(" · ");
}

interface Props {
  state: AppState;
  run: Run;
  agents: AgentCache;
  pending: PendingCreate | null;
  menuFor: string | null;
  onNew: () => void;
  onConnect: (id: string) => void;
  onDetails: (id: string) => void;
  onMenu: (id: string, anchor: HTMLElement) => void;
  onAgentStatus: OnAgentStatus;
}

export default function SandboxList({ state, run, agents, pending, menuFor, onNew, onConnect, onDetails, onMenu, onAgentStatus }: Props) {
  const p = state.progress;
  const showPending = p !== null && !state.sandboxes.some((s) => s.id === p.machineId);
  const empty = state.sandboxes.length === 0 && !showPending;
  return (
    <>
      <div className="list-head">
        <h1 className="list-title">
          Sandboxes <span className="muted">{state.sandboxes.length}</span>
        </h1>
        <button className="btn primary" onClick={onNew}>
          <Plus />
          New sandbox
        </button>
      </div>
      <div className="scroll">
        {state.t3 === null && (
          <div className="list-card">
            <T3Missing run={run} />
          </div>
        )}
        {empty ? (
          <p className="empty">No sandboxes yet. Create one and open it in T3 Code.</p>
        ) : (
          <ul className="rows">
            {showPending && <PendingRow state={state} run={run} pending={pending} onConnect={onConnect} />}
            {state.sandboxes.map((s) => (
              <Row
                key={s.id}
                s={s}
                state={state}
                run={run}
                agents={agents}
                menuOpen={menuFor === s.id}
                onConnect={onConnect}
                onDetails={onDetails}
                onMenu={onMenu}
                onAgentStatus={onAgentStatus}
              />
            ))}
          </ul>
        )}
      </div>
      <div className="foot">
        <span>
          {state.login ?? "you"} · {cap(state.tier)}
        </span>
        <span>{counts(state.sandboxes)}</span>
      </div>
    </>
  );
}
