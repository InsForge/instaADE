import { setStartAtLogin, signOut, switchOrg } from "./api";
import { CheckAdesButton } from "./AdeCards";
import { AdeBadge, COMING_SOON } from "./ades";
import { cap, openExternal, thisComputer } from "./common";
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
          <div className="section-head">
            <h2 className="section-label">ADEs on {thisComputer()}</h2>
            <CheckAdesButton run={run} text />
          </div>
          <ul className="group" aria-label={`ADEs on ${thisComputer()}`}>
            {state.ades.map((a) => (
              <li key={a.id} className="group-row plain ade-row">
                <AdeBadge id={a.id} />
                <span className="grow">{a.name}</span>
                <span className="small muted">{a.version ?? "Not found"}</span>
              </li>
            ))}
            {COMING_SOON.map((a) => (
              <li key={a.id} className="group-row plain ade-row">
                <AdeBadge id={a.id} />
                <span className="grow muted">{a.name}</span>
                <span className="small muted">Coming soon</span>
              </li>
            ))}
          </ul>
        </section>

        <section className="section">
          <h2 className="section-label">General</h2>
          <div className="group">
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
