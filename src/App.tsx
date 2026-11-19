import { useCallback, useEffect, useState } from "react";
import { connectSandbox, getState, onState, openUrl, refreshT3, T3_DOWNLOAD_URL, toErrorView } from "./api";
import Details from "./Details";
import ErrorNotice, { ExternalLink } from "./ErrorNotice";
import NewSandbox from "./NewSandbox";
import Progress from "./Progress";
import SandboxList from "./SandboxList";
import Settings from "./Settings";
import Setup from "./Setup";
import type { AppState, ErrorView } from "./types";

type View = { name: "list" } | { name: "new" } | { name: "settings" } | { name: "details"; id: string };

const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const sameError = (a: ErrorView | null | undefined, b: ErrorView | null | undefined) =>
  !!a && !!b && a.kind === b.kind && a.message === b.message;

/** Shown once after a create or connect: the pairing dialog waits in T3 Code. */
export const CONFIRM_IN_T3 = "Confirm the new environment in T3 Code";

export default function App() {
  const [state, setState] = useState<AppState | null>(null);
  const [view, setView] = useState<View>({ name: "list" });
  // The error of the last rejected command. state.error covers the rest.
  const [failure, setFailure] = useState<ErrorView | null>(null);
  // A one-time message; the next command or screen change clears it.
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let off: (() => void) | undefined;
    let gone = false;
    onState(setState).then((u) => (gone ? u() : (off = u)));
    getState().then((s) => setState((cur) => cur ?? s), () => {});
    return () => {
      gone = true;
      off?.();
    };
  }, []);

  const run = useCallback((f: () => Promise<unknown>) => {
    setFailure(null);
    setNotice(null);
    return f().then(
      () => true,
      (e) => {
        setFailure(toErrorView(e));
        return false;
      },
    );
  }, []);

  if (!state) return <main className="page" />;

  const error = failure ?? state.error;
  const signedOut = !state.signedIn || error?.kind === "signedOut";
  const needSetup = signedOut || (state.t3 === null && state.sandboxes.length === 0);

  // The back end sets progress.error and state.error together, and the failed command rejects
  // with it too. Progress renders it with Retry, so the App-level copy hides when it is the same.
  const pe = state.progress?.error;
  const progressShown = !needSetup && state.progress !== null;
  const progressOwnsError = progressShown && sameError(pe, error);

  const go = (v: View) => {
    setFailure(null);
    setNotice(null);
    setView(v);
  };
  const connect = (id: string) => {
    void run(() => connectSandbox(id)).then((ok) => ok && setNotice(CONFIRM_IN_T3));
  };
  const created = () => {
    go({ name: "list" });
    setNotice(CONFIRM_IN_T3);
  };
  const list = (
    <SandboxList
      state={state}
      run={run}
      onNew={() => go({ name: "new" })}
      onConnect={connect}
      onDetails={(id) => go({ name: "details", id })}
    />
  );

  let body;
  if (needSetup) {
    body = <Setup state={{ ...state, signedIn: !signedOut }} run={run} onCreate={() => go({ name: "new" })} />;
  } else if (view.name === "new") {
    body = <NewSandbox state={state} run={run} onConnect={connect} onCreated={created} onBack={() => go({ name: "list" })} />;
  } else if (view.name === "settings") {
    body = <Settings state={state} run={run} />;
  } else if (view.name === "details") {
    const s = state.sandboxes.find((x) => x.id === view.id);
    body = s ? <Details key={s.id} state={state} sandbox={s} run={run} onConnect={connect} onBack={() => go({ name: "list" })} /> : list;
  } else {
    body = (
      <>
        {state.t3 === null && (
          <div className="notice warn">
            <span>
              T3 Code was not found. <ExternalLink url={T3_DOWNLOAD_URL}>Download T3 Code</ExternalLink>
            </span>
            <button className="btn small" onClick={() => run(() => refreshT3())}>
              Check again
            </button>
          </div>
        )}
        {list}
      </>
    );
  }

  // Retry belongs to the progress error only: another setup error (a reinstall, say) gets none.
  const retryId = state.progress?.machineId;
  const onRetry = retryId && sameError(pe, error) ? () => connect(retryId) : undefined;
  return (
    <main className="page">
      <header className="top">
        <strong className="mono brand">Instabox Bridge</strong>
        {!needSetup && (
          <>
            <nav>
              <button className="btn" onClick={() => go({ name: "new" })}>
                + New sandbox
              </button>
              <button className={`btn ghost${view.name === "list" ? " on" : ""}`} onClick={() => go({ name: "list" })}>
                Sandboxes
              </button>
              <button className={`btn ghost${view.name === "settings" ? " on" : ""}`} onClick={() => go({ name: "settings" })}>
                Settings
              </button>
            </nav>
            <span className="muted who">
              {state.login ?? "you"} · {cap(state.tier)}
            </span>
          </>
        )}
        {state.update && (
          <a
            href={state.update.url}
            onClick={(e) => {
              e.preventDefault();
              void openUrl(state.update!.url).catch(() => {});
            }}
          >
            New version {state.update.version}
          </a>
        )}
      </header>
      {error && error.kind !== "signedOut" && !progressOwnsError && (
        <ErrorNotice error={error} sandboxes={state.sandboxes} onConnect={connect} onRetry={onRetry} />
      )}
      {notice && !needSetup && (
        <div className="notice info" role="status">
          {notice}
        </div>
      )}
      {view.name !== "new" && progressShown && <Progress state={state} run={run} onConnect={connect} />}
      {body}
    </main>
  );
}
