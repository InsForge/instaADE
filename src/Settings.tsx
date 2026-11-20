import { addMcp, refreshT3, setStartAtLogin, signOut, switchOrg } from "./api";
import type { McpClient } from "./api";
import { cap, openExternal } from "./common";
import type { Run } from "./common";
import { Check } from "./icons";
import type { AppState } from "./types";

const CLIENTS: { id: McpClient; label: string }[] = [
  { id: "claudeCode", label: "Claude Code" },
  { id: "codex", label: "Codex" },
];

export default function Settings({ state, run }: { state: AppState; run: Run }) {
  const update = state.update;
  return (
    <div className="scroll">
      <div className="stack">
        {update && (
          <div className="update">
            <span className="small soft">A new version of Instabox Bridge is ready.</span>
            <button className="btn primary" onClick={() => openExternal(update.url)}>
              Download
            </button>
          </div>
        )}

        <section className="section">
          <h2 className="section-label">Account</h2>
          <div className="group">
            <div className="group-row">
              <span className="ellipsis">{state.login ?? "Signed in"}</span>
              <button className="btn text muted" onClick={() => run(() => signOut())}>
                Sign out
              </button>
            </div>
            {state.orgs.length > 0 && (
              <div className="group-row">
                <label htmlFor="org">Organization</label>
                <select id="org" className="select" value={state.orgId ?? ""} onChange={(e) => run(() => switchOrg(e.target.value))}>
                  {state.orgs.map((o) => (
                    <option key={o.id} value={o.id}>
                      {o.name} · {cap(o.tier)}
                    </option>
                  ))}
                </select>
              </div>
            )}
          </div>
        </section>

        <section className="section">
          <h2 className="section-label">This Mac</h2>
          <div className="group">
            <div className="group-row">
              <span>
                T3 Code <span className="muted">{state.t3 ?? "Not found"}</span>
              </span>
              <button className="btn" onClick={() => run(() => refreshT3())}>
                Check again
              </button>
            </div>
            <div className="group-row plain">
              <label htmlFor="login-switch">Start at login</label>
              <button
                id="login-switch"
                role="switch"
                aria-checked={state.startAtLogin}
                className="switch"
                onClick={() => run(() => setStartAtLogin(!state.startAtLogin))}
              >
                <span />
              </button>
            </div>
          </div>
        </section>

        <section className="section">
          <h2 className="section-label">Let agents on this Mac manage sandboxes</h2>
          <div className="group">
            {CLIENTS.map((c) => {
              const v = state.mcp[c.id];
              return (
                <div key={c.id} className={`group-row${v === false ? "" : " plain"}`}>
                  <span>{c.label}</span>
                  {v === null ? (
                    <span className="small muted">Not installed</span>
                  ) : v ? (
                    <span className="ok">
                      <Check />
                      Added
                    </span>
                  ) : (
                    <button className="btn" onClick={() => run(() => addMcp(c.id))}>
                      Add
                    </button>
                  )}
                </div>
              );
            })}
          </div>
        </section>
      </div>
    </div>
  );
}
