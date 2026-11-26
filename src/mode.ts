import { useCallback, useState } from "react";
import type { Mode } from "./ades";
import type { AppState } from "./types";

/** Where the mode a user picked is kept on this computer, per instabox login. */
export const modeKey = (login: string) => `instabox.ade.${login}`;

const MODES: Mode[] = ["instabox", "t3", "cmux", "orca", "paradev", "paseo"];

/** The mode `login` picked on this computer; null when they never picked one. */
export function savedMode(login: string): Mode | null {
  try {
    const v = localStorage.getItem(modeKey(login));
    return MODES.find((m) => m === v) ?? null;
  } catch {
    return null;
  }
}

function saveMode(login: string, mode: Mode) {
  try {
    localStorage.setItem(modeKey(login), mode);
  } catch {
    // Kept for this run only.
  }
}

/**
 * The mode before any pick, so v0.1.x users lose nothing: T3 Code when it is on this computer
 * or a sandbox has T3, else plain Instabox.
 */
export function defaultMode(state: AppState): Mode {
  const t3 = state.ades.find((a) => a.id === "t3");
  if (!t3) return "instabox";
  const t3Here = t3.version !== null;
  const t3OnSandbox = state.sandboxes.some((s) => s.ades.includes("t3"));
  return t3Here || t3OnSandbox ? "t3" : "instabox";
}

/** The mode to show: the pick, while its ADE runs on this computer, else the default. */
export function currentMode(state: AppState, saved: Mode | null): Mode {
  if (saved === null) return defaultMode(state);
  if (saved === "instabox") return saved;
  return state.ades.some((a) => a.id === saved) ? saved : "instabox";
}

/**
 * The mode of the signed-in user. `picked` is false until they pick one (or dismiss the
 * first-run card, which picks plain Instabox).
 */
export function useMode(state: AppState | null) {
  const login = state?.login ?? null;
  // Picks of this run, so a pick holds even when the storage cannot be written.
  const [picks, setPicks] = useState<Record<string, Mode>>({});
  const saved = login === null ? null : (picks[login] ?? savedMode(login));
  const choose = useCallback(
    (mode: Mode) => {
      if (login === null) return;
      saveMode(login, mode);
      setPicks((p) => ({ ...p, [login]: mode }));
    },
    [login],
  );
  const mode: Mode = state ? currentMode(state, saved) : "instabox";
  return { mode, picked: saved !== null, choose };
}
