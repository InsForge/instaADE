import { setStartAtLogin, signOut, switchOrg } from "./api";
import { CheckT3Button } from "./Setup";
import { cap, openExternal } from "./common";
import type { Run } from "./common";
import type { AppState } from "./types";

export default function Settings({ state, run }: { state: AppState; run: Run }) {
  const update = state.update;
  return (
    <div className="scroll">
      <div className="stack">
        {update && (
          <div className="update">
            <span className="small soft">A new version of Instabox is ready.</span>
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
              <CheckT3Button run={run} />
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
      </div>
    </div>
  );
}
