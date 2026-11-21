import { useCallback, useEffect, useRef, useState } from "react";
import { connectSandbox, createSandbox, getState, onState, openInAde, toErrorView } from "./api";
import { AdeBadge, confirmText, hasAde, modeName, pairs, skinStyle } from "./ades";
import type { Mode } from "./ades";
import AdeSwitcher from "./AdeSwitcher";
import { agentKey } from "./common";
import type { AgentCache, OnAgentStatus } from "./common";
import Details from "./Details";
import ErrorNotice from "./ErrorNotice";
import { Back, Dots, Gear } from "./icons";
import { useMode } from "./mode";
import NewSandbox from "./NewSandbox";
import type { CreateArgs } from "./NewSandbox";
import SandboxList from "./SandboxList";
import type { PendingCreate } from "./SandboxList";
import SandboxMenu from "./SandboxMenu";
import Settings from "./Settings";
import Setup from "./Setup";
import type { AdeId, AppState, ErrorView } from "./types";

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
  const [switcher, setSwitcher] = useState(false);
  // The ADE just switched to, while its hint shows.
  const [hint, setHint] = useState<AdeId | null>(null);
  const root = useRef<HTMLElement>(null);
  const { mode, picked, choose } = useMode(state);

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
  // Only sign-in is a screen of its own: an ADE is offered in the list, never required.
  const needSetup = signedOut;
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
    setSwitcher(false);
    setView(v);
  };
  const back = () => go({ name: "list" });
  /** Sets the ADE up on the sandbox, then opens it: confirm there. */
  const connect = (id: string, ade: AdeId) => {
    void run(() => connectSandbox(id, ade)).then((ok) => ok && setNotice(confirmText(state, ade)));
  };
  /** Opens the sandbox in the ADE. A link ADE (cmux) asks to confirm every time; T3 only when it paired just now. */
  const open = (id: string, ade: AdeId) => {
    const s = state.sandboxes.find((x) => x.id === id);
    const ready = !!s && hasAde(s, ade) && (!pairs(ade) || s.connected);
    void run(() => openInAde(id, ade)).then((ok) => ok && (!pairs(ade) || !ready) && setNotice(confirmText(state, ade)));
  };
  const create = (args: CreateArgs, size: string) => {
    setPending({ name: args.name, size });
    go({ name: "list" });
    void run(() => createSandbox(args)).then((ok) => ok && args.ade && setNotice(confirmText(state, args.ade)));
  };
  const toggleMenu = (id: string, anchor: HTMLElement) =>
    setMenu((m) => (m && m.id === id && m.anchor === anchor ? null : { id, anchor }));
  const pickMode = (m: Mode) => {
    choose(m);
    setHint(m === "instabox" ? null : m);
    setFailure(null);
    setNotice(null);
  };

  // Retry belongs to the progress error only: another setup error (a reinstall, say) gets none.
  const retryId = p?.machineId;
  const retryAde = p?.ade ?? (mode === "instabox" ? null : mode);
  const onRetry = retryId && retryAde && sameError(pe, error) ? () => connect(retryId, retryAde) : undefined;
  const showError = !!error && error.kind !== "signedOut" && !progressOwnsError;
  const showNotice = !!notice && !needSetup;

  // Sign-in has no header: it draws its own brand block.
  let header = null;
  if (!needSetup && current.name === "list") {
    header = (
      <header className="top">
        <AdeSwitcher state={state} mode={mode} open={switcher} onOpenChange={setSwitcher} onChoose={pickMode} />
        <span className="grow" />
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
        {sandbox && mode !== "instabox" && (
          <span className="mode-chip">
            <AdeBadge id={mode} size={16} />
            {modeName(state, mode)} mode
          </span>
        )}
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
    body = <Setup state={state} run={run} />;
  } else if (current.name === "new") {
    body = <NewSandbox state={state} run={run} mode={mode} onConnect={connect} onCreate={create} onBack={back} />;
  } else if (current.name === "settings") {
    body = <Settings state={state} run={run} />;
  } else if (sandbox) {
    body = (
      <Details
        key={sandbox.id}
        state={state}
        sandbox={sandbox}
        run={run}
        mode={mode}
        agents={agents}
        onAgentStatus={onAgentStatus}
        onOpen={open}
        onConnect={connect}
      />
    );
  } else {
    body = (
      <SandboxList
        state={state}
        run={run}
        mode={mode}
        agents={agents}
        pending={pending}
        menuFor={menu?.id ?? null}
        firstRun={!picked}
        hint={hint}
        onNew={() => go({ name: "new" })}
        onOpen={open}
        onConnect={connect}
        onDetails={(id) => go({ name: "details", id })}
        onMenu={toggleMenu}
        onAgentStatus={onAgentStatus}
        onPickAde={() => setSwitcher(true)}
        onNotNow={() => choose("instabox")}
        onDismissHint={() => setHint(null)}
      />
    );
  }

  const menuSandbox = menu && !needSetup && (current.name === "list" || current.name === "details") ? state.sandboxes.find((s) => s.id === menu.id) : undefined;
  return (
    <main className="popover" ref={root} data-mode={needSetup ? "instabox" : mode} style={needSetup ? undefined : skinStyle(mode)}>
      {header}
      {(showError || showNotice) && (
        <div className="banners">
          {showError && <ErrorNotice error={error!} state={state} mode={mode} onConnect={connect} onRetry={onRetry} />}
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
          mode={mode}
          anchor={menu.anchor}
          container={root.current}
          onOpen={open}
          onConnect={connect}
          onCopied={() => setNotice(SSH_COPIED)}
          onClose={closeMenu}
        />
      )}
    </main>
  );
}
