import type { ReactNode } from "react";
import { openUrl } from "./api";
import type { Agent } from "./api";
import type { SandboxView, ViewState } from "./types";

/** Runs a command; a failure shows in the app. Resolves true when the command succeeded. */
export type Run = (f: () => Promise<unknown>) => Promise<boolean>;

export const BUSY: ViewState[] = ["creating", "waking", "pausing", "deleting"];

const STATE_LABEL: Record<ViewState, string> = {
  creating: "Creating",
  running: "Running",
  paused: "Paused",
  stopped: "Stopped",
  waking: "Waking",
  pausing: "Pausing",
  error: "Error",
  deleting: "Deleting",
};

export const stateLabel = (s: ViewState) => STATE_LABEL[s] ?? s;

export const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

export const isAsleep = (s: SandboxView) => s.state === "paused" || s.state === "stopped";

/** The free plan fixes size and auto sleep. */
export const isFree = (s: SandboxView, tier: string) => s.free || tier === "free";

export function StateDot({ state }: { state: ViewState }) {
  return <span className={`dot ${state}`} aria-hidden="true" />;
}

export const AGENTS: { id: Agent; label: string }[] = [
  { id: "claude", label: "Claude Code" },
  { id: "codex", label: "Codex" },
];

/** Agent sign-in results the details view fetched, by agentKey. The list only reads them. */
export type AgentCache = Record<string, boolean>;
export const agentKey = (id: string, agent: Agent) => `${id}:${agent}`;
export type OnAgentStatus = (id: string, agent: Agent, signedIn: boolean | null) => void;

export function openExternal(url: string) {
  void openUrl(url).catch(() => {});
}

/** Copies text that may still be loading. WebKit allows this only inside the click, so the write starts now. */
export function copyText(text: Promise<string>): Promise<void> {
  const cb = navigator.clipboard;
  if (!cb) return Promise.reject(new Error("The clipboard is not available"));
  if (typeof ClipboardItem !== "undefined" && typeof cb.write === "function") {
    const blob = text.then((t) => new Blob([t], { type: "text/plain" }));
    return cb.write([new ClipboardItem({ "text/plain": blob })]);
  }
  return text.then((t) => cb.writeText(t));
}

/** One line of status with an optional button on the right. */
export function Line({ tone, text, children }: { tone?: "amber" | "red" | "muted"; text: ReactNode; children?: ReactNode }) {
  return (
    <div className={`line ${tone ?? "muted"}`}>
      <span className="line-text">{text}</span>
      {children && <span className="line-buttons">{children}</span>}
    </div>
  );
}
