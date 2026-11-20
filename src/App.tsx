import { useCallback, useEffect, useRef, useState } from "react";
import { connectSandbox, createSandbox, getState, onState, toErrorView } from "./api";
import { agentKey } from "./common";
import type { AgentCache, OnAgentStatus } from "./common";
import Details from "./Details";
import ErrorNotice from "./ErrorNotice";
import { Back, Dots, Gear, Mark } from "./icons";
import NewSandbox from "./NewSandbox";
import type { CreateArgs } from "./NewSandbox";
import SandboxList from "./SandboxList";
import type { PendingCreate } from "./SandboxList";
import SandboxMenu from "./SandboxMenu";
import Settings from "./Settings";
import Setup from "./Setup";
import type { AppState, ErrorView } from "./types";

type View = { name: "list" } | { name: "new" } | { name: "settings" } | { name: "details"; id: string };
type Menu = { id: string; anchor: HTMLElement } | null;

const sameError = (a: ErrorView | null | undefined, b: ErrorView | null | undefined) =>
  !!a && !!b && a.kind === b.kind && a.message === b.message;

/** Shown once after a create or connect: the pairing dialog waits in T3 Code. */
export const CONFIRM_IN_T3 = "Confirm the new environment in T3 Code";
export const SSH_COPIED = "SSH command copied";

export default function App() {
  const [state, setState] = useState<AppState | null>(null);
  const [view, setView] = useState<View>({ name: "list" });
  // The error of the last rejected command. state.error covers the rest.
  const [failure, setFailure] = useState<ErrorView | null>(null);
  // A one-time message; the next command or screen change clears it.
  const [notice, setNotice] = useState<string | null>(null);
  const [menu, setMenu] = useState<Menu>(null);
  const [agents, setAgents] = useState<AgentCache>({});
  const [pending, setPending] = useState<PendingCreate | null>(null);
  const root = useRef<HTMLElement>(null);

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

  const onAgentStatus: OnAgentStatus = useCallback((id, agent, signedIn) => {
    const k = agentKey(id, agent);
    setAgents((c) => {
      if (signedIn === null) {
        if (!(k in c)) return c;
        const next = { ...c };
        delete next[k];
        return next;
      }
      return c[k] === signedIn ? c : { ...c, [k]: signedIn };
    });
  }, []);

  const closeMenu = useCallback(() => setMenu(null), []);

  // A sandbox that is gone takes its details and its menu with it.
  const has = (id: string) => !!state?.sandboxes.some((s) => s.id === id);
  const detailsGone = !!state && view.name === "details" && !has(view.id);
  const menuGone = !!state && !!menu && !has(menu.id);
  useEffect(() => {
    if (detailsGone) setView({ name: "list" });
  }, [detailsGone]);
  useEffect(() => {
    if (menuGone) setMenu(null);
  }, [menuGone]);

  if (!state) return <main className="popover" ref={root} />;

  const error = failure ?? state.error;
  const signedOut = !state.signedIn || error?.kind === "signedOut";
  const needSetup = signedOut || (state.t3 === null && state.sandboxes.length === 0);
  const current: View = detailsGone ? { name: "list" } : view;
  const sandbox = current.name === "details" ? state.sandboxes.find((s) => s.id === current.id) : undefined;

  // The back end sets progress.error and state.error together, and the failed command rejects
  // with it too. The progress line shows it with Retry, so the banner hides when it is the same.
  const p = state.progress;
  const pe = p?.error;
  const progressShown =
    !needSetup && p !== null && (current.name === "list" || current.name === "new" || (current.name === "details" && p.machineId === current.id));
  const progressOwnsError = progressShown && sameError(pe, error);

  const go = (v: View) => {
    setFailure(null);
    setNotice(null);
    setMenu(null);
    setView(v);
  };
  const back = () => go({ name: "list" });
  const connect = (id: string) => {
    void run(() => connectSandbox(id)).then((ok) => ok && setNotice(CONFIRM_IN_T3));
  };
  const create = (args: CreateArgs, size: string) => {
    setPending({ name: args.name, size });
    go({ name: "list" });
    void run(() => createSandbox(args)).then((ok) => ok && setNotice(CONFIRM_IN_T3));
  };
  const toggleMenu = (id: string, anchor: HTMLElement) =>
    setMenu((m) => (m && m.id === id && m.anchor === anchor ? null : { id, anchor }));

  // Retry belongs to the progress error only: another setup error (a reinstall, say) gets none.
  const retryId = p?.machineId;
  const onRetry = retryId && sameError(pe, error) ? () => connect(retryId) : undefined;
  const showError = !!error && error.kind !== "signedOut" && !progressOwnsError;
  const showNotice = !!notice && !needSetup;

  // First run has no header: setup draws its own brand block.
  let header = null;
  if (!needSetup && current.name === "list") {
    header = (
      <header className="top">
        <Mark />
        <span className="top-title">Instabox</span>
        <button className="icon-btn hd" aria-label="Settings" title={state.update ? "Update available" : undefined} onClick={() => go({ name: "settings" })}>
          <Gear />
          {state.update && <span className="badge" aria-hidden="true" />}
        </button>
      </header>
    );
  } else if (!needSetup) {
    const title = current.name === "new" ? "New sandbox" : current.name === "settings" ? "Settings" : (sandbox?.name ?? "");
    header = (
      <header className="top sub">
        <button className="icon-btn hd" aria-label="Back to sandboxes" onClick={back}>
          <Back />
        </button>
        <span className="top-title">{title}</span>
        {sandbox && (
          <button
            className="icon-btn hd"
            aria-label={`More actions for ${sandbox.name}`}
            aria-haspopup="menu"
            aria-expanded={menu?.id === sandbox.id}
            onClick={(e) => toggleMenu(sandbox.id, e.currentTarget)}
          >
            <Dots />
          </button>
        )}
      </header>
    );
  }

  let body;
  if (needSetup) {
    body = <Setup state={{ ...state, signedIn: !signedOut }} run={run} />;
  } else if (current.name === "new") {
    body = <NewSandbox state={state} run={run} onConnect={connect} onCreate={create} onBack={back} />;
  } else if (current.name === "settings") {
    body = <Settings state={state} run={run} />;
  } else if (sandbox) {
    body = (
      <Details key={sandbox.id} state={state} sandbox={sandbox} run={run} agents={agents} onAgentStatus={onAgentStatus} onConnect={connect} />
    );
  } else {
    body = (
      <SandboxList
        state={state}
        run={run}
        agents={agents}
        pending={pending}
        menuFor={menu?.id ?? null}
        onNew={() => go({ name: "new" })}
        onConnect={connect}
        onDetails={(id) => go({ name: "details", id })}
        onMenu={toggleMenu}
        onAgentStatus={onAgentStatus}
      />
    );
  }

  const menuSandbox = menu && !needSetup && (current.name === "list" || current.name === "details") ? state.sandboxes.find((s) => s.id === menu.id) : undefined;
  return (
    <main className="popover" ref={root}>
      {header}
      {(showError || showNotice) && (
        <div className="banners">
          {showError && <ErrorNotice error={error!} sandboxes={state.sandboxes} onConnect={connect} onRetry={onRetry} />}
          {showNotice && (
            <div className="banner info" role="status">
              <span className="banner-text">{notice}</span>
            </div>
          )}
        </div>
      )}
      {body}
      {menu && menuSandbox && root.current && (
        <SandboxMenu
          key={menu.id}
          s={menuSandbox}
          state={state}
          run={run}
          anchor={menu.anchor}
          container={root.current}
          onConnect={connect}
          onCopied={() => setNotice(SSH_COPIED)}
          onClose={closeMenu}
        />
      )}
    </main>
  );
}
