import { addMcp, setStartAtLogin, signOut, switchOrg } from "./api";
import type { McpClient } from "./api";
import type { Run } from "./SandboxList";
import type { AppState } from "./types";

const CLIENTS: { id: McpClient; label: string }[] = [
  { id: "claudeCode", label: "Claude Code" },
  { id: "codex", label: "Codex" },
];

export default function Settings({ state, run }: { state: AppState; run: Run }) {
  return (
    <section className="card">
      <h1>Settings</h1>

      <h2>Instabox MCP</h2>
      <p className="muted">Lets your coding agents manage instabox sandboxes.</p>
      <ul className="plain">
        {CLIENTS.map((c) => {
          const v = state.mcp[c.id];
          return (
            <li key={c.id} className="line">
              <span>{c.label}</span>
              {v === null ? (
                <span className="muted">Not installed</span>
              ) : v ? (
                <span className="success">Added</span>
              ) : (
                <button className="btn small" onClick={() => run(() => addMcp(c.id))}>
                  Add
                </button>
              )}
            </li>
          );
        })}
      </ul>

      <h2>Account</h2>
      <div className="line">
        <span>{state.login ?? "Signed in"}</span>
        <button className="btn small" onClick={() => run(() => signOut())}>
          Sign out
        </button>
      </div>
      {state.orgs.length > 0 && (
        <div className="line">
          <label htmlFor="org">Org</label>
          <select id="org" value={state.orgId ?? ""} onChange={(e) => run(() => switchOrg(e.target.value))}>
            {state.orgs.map((o) => (
              <option key={o.id} value={o.id}>
                {o.name} · {o.tier}
              </option>
            ))}
          </select>
        </div>
      )}

      <h2>General</h2>
      <label className="line">
        <span>Start at login</span>
        <input type="checkbox" checked={state.startAtLogin} onChange={(e) => run(() => setStartAtLogin(e.target.checked))} />
      </label>
    </section>
  );
}
