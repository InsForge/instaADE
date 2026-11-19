import { useEffect, useState } from "react";
import {
  agentSignIn,
  agentStatus,
  deleteSandbox,
  openInT3,
  openSsh,
  pairAgain,
  pause,
  reinstallT3,
  resize,
  resume,
  setAutoSleep,
  sshCommand,
  toErrorView,
} from "./api";
import type { Agent } from "./api";
import { StateDot, stateLabel, T3Line } from "./SandboxList";
import type { Run } from "./SandboxList";
import type { AppState, SandboxView } from "./types";

interface Props {
  state: AppState;
  sandbox: SandboxView;
  run: Run;
  onConnect: (id: string) => void;
  onBack: () => void;
}

const AGENTS: { id: Agent; label: string }[] = [
  { id: "claude", label: "Claude Code" },
  { id: "codex", label: "Codex" },
];

export function formatMinutes(m: number): string {
  const h = Math.floor(m / 60);
  const r = m % 60;
  if (h === 0) return `${r} m`;
  return r === 0 ? `${h} h` : `${h} h ${r} m`;
}

function AgentRow({ id, running, agent, label, run }: { id: string; running: boolean; agent: Agent; label: string; run: Run }) {
  // null: not checked yet (or the sandbox is not running).
  const [signedIn, setSignedIn] = useState<boolean | null>(null);
  const check = () =>
    // One check per open or sign-in. It runs a command on the sandbox, so it never polls.
    agentStatus(id, agent).then(setSignedIn, () => setSignedIn(false));
  useEffect(() => {
    if (running) void check();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, running, agent]);
  return (
    <span className="agent">
      {label}{" "}
      {signedIn ? (
        <span className="success">✓ Signed in</span>
      ) : (
        <button
          className="btn small"
          disabled={!running}
          onClick={() => run(() => agentSignIn(id, agent).then(check))}
        >
          Sign in
        </button>
      )}
    </span>
  );
}

/** The SSH row: nothing asked yet, the login command, or why it could not be made. */
type Ssh = { command: string } | { error: string } | null;

export default function Details({ state, sandbox: s, run, onConnect, onBack }: Props) {
  const free = s.free || state.tier === "free";
  const running = s.state === "running";
  const [ssh, setSsh] = useState<Ssh>(null);
  const [copied, setCopied] = useState(false);
  const [editSize, setEditSize] = useState(false);
  const [editSleep, setEditSleep] = useState(false);
  // Resize starts at the sandbox's own size, so Apply does nothing until another is picked.
  const [newSize, setNewSize] = useState(s.size);
  const [sleep, setSleep] = useState(s.autoSleep.seconds ?? 1800);
  const [confirmDelete, setConfirmDelete] = useState(false);

  // Asked only when the user opens the row: each new command may mint a 30-day token.
  const showSsh = () =>
    sshCommand(s.id).then(
      (command) => setSsh({ command }),
      (e) => setSsh({ error: toErrorView(e).message }),
    );

  const copy = () => {
    if (!ssh || !("command" in ssh)) return;
    void navigator.clipboard?.writeText(ssh.command).then(() => setCopied(true), () => {});
  };

  return (
    <section className="card details">
      <button className="btn small back" onClick={onBack}>
        ← Sandboxes
      </button>
      <h1 className="mono">
        <StateDot state={s.state} /> {s.name}
      </h1>
      <p className="muted">
        {stateLabel(s.state)} · {s.size} · <T3Line s={s} desktop={state.t3} run={run} />
        {s.t3Version && !s.updatePending && !s.desktopBehind && s.t3Version === state.t3 && " (matches T3 Code)"}
      </p>
      {s.t3CodeConnected && <p className="success">T3 Code connected · won't auto sleep</p>}

      <div className="row-actions">
        {running && (
          <button className="btn" onClick={() => run(() => pause(s.id))}>
            Pause
          </button>
        )}
        {(s.state === "paused" || s.state === "stopped") && (
          <button className="btn" onClick={() => run(() => resume(s.id))}>
            Resume
          </button>
        )}
        {s.connected ? (
          <button className="btn" onClick={() => run(() => openInT3(s.id))}>
            Open in T3 Code
          </button>
        ) : (
          <button className="btn" onClick={() => onConnect(s.id)}>
            Connect to T3 Code
          </button>
        )}
        {s.connected && (
          <button className="btn" onClick={() => run(() => pairAgain(s.id))}>
            Pair again
          </button>
        )}
        {s.connected && !s.desktopBehind && (
          <button className="btn" onClick={() => run(() => reinstallT3(s.id))}>
            Reinstall T3
          </button>
        )}
        <button className="btn" disabled={free} onClick={() => setEditSize(!editSize)}>
          Resize
        </button>
        {confirmDelete ? (
          <button className="btn danger" onClick={() => run(() => deleteSandbox(s.id))}>
            Confirm delete
          </button>
        ) : (
          <button className="btn danger" onClick={() => setConfirmDelete(true)}>
            Delete
          </button>
        )}
      </div>

      {editSize && !free && (
        <div className="inline">
          <select aria-label="New size" value={newSize} onChange={(e) => setNewSize(e.target.value)}>
            {!state.sizes.some((z) => z.label === s.size) && (
              <option value={s.size} disabled>
                {s.size}
              </option>
            )}
            {state.sizes.map((z) => (
              <option key={z.label} value={z.label}>
                {z.label}
              </option>
            ))}
          </select>
          <button
            className="btn small"
            disabled={newSize === s.size}
            onClick={() => {
              const z = state.sizes.find((x) => x.label === newSize);
              if (z) run(() => resize(s.id, z.vcpus, z.memMiB));
              setEditSize(false);
            }}
          >
            Apply
          </button>
          <span className="muted hint">Restarts the sandbox</span>
        </div>
      )}

      <dl className="facts">
        <dt>Auto sleep</dt>
        <dd>
          {s.autoSleep.label}{" "}
          <button className="btn small" disabled={free} onClick={() => setEditSleep(!editSleep)}>
            Change
          </button>
          {editSleep && !free && (
            <span className="inline">
              <select aria-label="New auto sleep" value={sleep} onChange={(e) => setSleep(Number(e.target.value))}>
                {state.sleepChoices.map((c) => (
                  <option key={c.seconds} value={c.seconds}>
                    {c.label}
                  </option>
                ))}
              </select>
              <button
                className="btn small"
                onClick={() => {
                  run(() => setAutoSleep(s.id, sleep));
                  setEditSleep(false);
                }}
              >
                Apply
              </button>
            </span>
          )}
        </dd>

        <dt>Agents</dt>
        <dd className="agents">
          {AGENTS.map((a) => (
            <AgentRow key={a.id} id={s.id} running={running} agent={a.id} label={a.label} run={run} />
          ))}
        </dd>

        <dt>SSH</dt>
        <dd>
          {ssh && "command" in ssh ? (
            <>
              <code className="mono">{ssh.command}</code>{" "}
              <button className="btn small" onClick={copy}>
                {copied ? "Copied" : "Copy"}
              </button>
            </>
          ) : (
            <>
              {ssh && (
                <span className="error-text" role="alert">
                  {ssh.error}
                </span>
              )}{" "}
              <button className="btn small" onClick={() => void showSsh()}>
                Show command
              </button>
            </>
          )}{" "}
          <button className="btn small" onClick={() => run(() => openSsh(s.id))}>
            Open in Terminal
          </button>
        </dd>

        <dt>GitHub</dt>
        <dd>Sign in from T3 Code: Settings → Source control</dd>

        {free && (
          <>
            <dt>Free plan</dt>
            <dd>
              {s.cooldownMinutes !== null
                ? `Cooling down, ready in about ${s.cooldownMinutes} min`
                : s.runLeftMinutes !== null
                  ? `Can run ${formatMinutes(s.runLeftMinutes)} more`
                  : "Runs up to 8 h at a time"}
            </dd>
          </>
        )}
      </dl>
    </section>
  );
}
