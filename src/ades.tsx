import type { CSSProperties } from "react";
import { MarkTile } from "./icons";
import type { AdeId, AppState, SandboxView } from "./types";

/** What the popover is for: plain Instabox (sandboxes only) or opening sandboxes in an ADE. */
export type Mode = "instabox" | AdeId;

/** An ADE the switcher lists but the app does not support yet. */
export type SoonId = "orca" | "superset";

/** The colors of an ADE mode. Plain Instabox keeps the neutral look of styles.css. */
export interface Skin {
  page: string;
  header: string;
  raised: string;
  frame: string;
  line: string;
  muted: string;
  idle: string;
  /** Primary buttons and the progress bar. */
  accent: string;
  accentHover: string;
  /** Text on the accent. */
  ink: string;
  /** Accent-colored text on the page. */
  accentText: string;
  accentSoft: string;
  accentLine: string;
}

export const SKINS: Record<AdeId, Skin> = {
  t3: {
    page: "#14121B",
    header: "#1B1726",
    raised: "#1E1A29",
    frame: "rgba(167, 139, 250, 0.24)",
    line: "rgba(167, 139, 250, 0.2)",
    muted: "#A9A3BA",
    idle: "#57536A",
    accent: "#A78BFA",
    accentHover: "#B9A2FB",
    ink: "#140F22",
    accentText: "#C4B5FD",
    accentSoft: "rgba(167, 139, 250, 0.12)",
    accentLine: "rgba(167, 139, 250, 0.45)",
  },
  cmux: {
    page: "#0F131A",
    header: "#121A24",
    raised: "#172030",
    frame: "rgba(56, 189, 248, 0.24)",
    line: "rgba(56, 189, 248, 0.2)",
    muted: "#A0ABBA",
    idle: "#4F5A68",
    accent: "#38BDF8",
    accentHover: "#5CCBFA",
    ink: "#04141F",
    accentText: "#7DD3FC",
    accentSoft: "rgba(56, 189, 248, 0.12)",
    accentLine: "rgba(56, 189, 248, 0.45)",
  },
};

/** The CSS variables of a mode, set on the popover. */
export function skinStyle(mode: Mode): CSSProperties | undefined {
  if (mode === "instabox") return undefined;
  const s = SKINS[mode];
  return {
    "--page": s.page,
    "--header": s.header,
    "--raised": s.raised,
    "--frame": s.frame,
    "--line": s.line,
    "--muted": s.muted,
    "--idle": s.idle,
    "--accent": s.accent,
    "--accent-hover": s.accentHover,
    "--accent-ink": s.ink,
    "--accent-text": s.accentText,
    "--accent-soft": s.accentSoft,
    "--accent-line": s.accentLine,
  } as CSSProperties;
}

interface Badge {
  label: string;
  bg: string;
  fg: string;
}

const BADGES: Record<AdeId | SoonId, Badge> = {
  t3: { label: "T3", bg: "#A78BFA", fg: "#140F22" },
  cmux: { label: "C", bg: "#38BDF8", fg: "#04141F" },
  orca: { label: "O", bg: "rgba(45, 212, 191, 0.22)", fg: "#5EEAD4" },
  superset: { label: "S", bg: "rgba(244, 114, 182, 0.22)", fg: "#F9A8D4" },
};

/** The ADEs the switcher shows as coming soon, after the ones the app supports. */
export const COMING_SOON: { id: SoonId; name: string }[] = [
  { id: "orca", name: "Orca" },
  { id: "superset", name: "Superset" },
];

/** The square mark of an ADE, or the instabox mark for plain Instabox. */
export function AdeBadge({ id, size = 20 }: { id: Mode | SoonId; size?: number }) {
  if (id === "instabox") return <MarkTile size={size} />;
  const b = BADGES[id];
  return (
    <span
      className="ade-badge"
      aria-hidden="true"
      style={{ width: size, height: size, background: b.bg, color: b.fg, fontSize: b.label.length > 1 ? size * 0.45 : size * 0.5 }}
    >
      {b.label}
    </span>
  );
}

const NAMES: Record<AdeId, string> = { t3: "T3 Code", cmux: "cmux" };

/** The desktop app's name, such as "T3 Code". */
export const adeName = (state: AppState, id: AdeId) => state.ades.find((a) => a.id === id)?.name ?? NAMES[id];

/** The ADE's desktop app on this computer, if it runs here. */
export const localAde = (state: AppState, id: AdeId) => state.ades.find((a) => a.id === id);

/** The name of a mode, for the switcher. */
export const modeName = (state: AppState, mode: Mode) => (mode === "instabox" ? "Instabox" : adeName(state, mode));

/** True when the ADE is set up on the sandbox (its mark lists it). */
export const hasAde = (s: SandboxView, id: AdeId) => s.ades.includes(id);

/** T3 pairs with a server it installs, and keeps its version in step; cmux opens a link. */
export const pairs = (id: AdeId) => id === "t3";

/** The line under a row that has the ADE. */
export function readyText(state: AppState, s: SandboxView, id: AdeId): string {
  if (id === "t3") return s.t3Version ? `T3 Code ${s.t3Version}` : "T3 Code";
  return `Ready for ${adeName(state, id)}`;
}

/** Shown once after a connect or create opens the ADE: what to confirm there. */
export function confirmText(state: AppState, id: AdeId): string {
  return id === "t3" ? "Confirm the new environment in T3 Code" : `Confirm the workspace in ${adeName(state, id)}`;
}
