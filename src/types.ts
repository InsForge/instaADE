// Mirrors the Rust `AppState` (src-tauri/src/app.rs). Both test suites read src/fixtures/state.json.

export type ViewState =
  | "creating"
  | "running"
  | "paused"
  | "stopped"
  | "waking"
  | "pausing"
  | "error"
  | "deleting";

export type ErrorKind =
  | "freeLimit"
  | "cooldown"
  | "t3Busy"
  | "setup"
  | "signedOut"
  | "timeout"
  | "api"
  | "other";

/** Every command error, state.error and progress.error. */
export interface ErrorView {
  kind: ErrorKind;
  message: string;
  /** cooldown: minutes left. */
  minutes?: number;
  /** api: the API's error code. */
  code?: string;
}

export interface OrgView {
  id: string;
  name: string;
  tier: string;
  personal: boolean;
}

export interface SandboxView {
  id: string;
  name: string;
  state: ViewState;
  size: string;
  connected: boolean;
  t3Version: string | null;
  free: boolean;
  autoSleep: { seconds: number | null; label: string };
  runLeftMinutes: number | null;
  cooldownMinutes: number | null;
  /** The sandbox's T3 is behind the desktop's and the update was deferred or failed: offer [Update]. */
  updatePending: boolean;
  /** The sandbox's T3 is newer than the desktop's: offer "Update T3 Code on this computer", never [Update]. */
  desktopBehind: boolean;
}

export interface SizeChoice {
  vcpus: number;
  memMiB: number;
  label: string;
}

export interface SleepChoice {
  seconds: number;
  label: string;
}

export interface SnapshotView {
  id: string;
  name: string;
}

export interface McpStatus {
  /** null: the claude CLI is not installed. */
  claudeCode: boolean | null;
  codex: boolean | null;
}

/** step: connecting | creating | waking | installing | pairing | done */
export interface ProgressView {
  /** create: a new sandbox. connect: one that exists. */
  kind: "create" | "connect";
  step: string;
  /** Set once the sandbox exists; Retry calls connect_sandbox with it. */
  machineId: string | null;
  error: ErrorView | null;
}

export interface AppState {
  signedIn: boolean;
  login: string | null;
  signInCode: string | null;
  orgs: OrgView[];
  orgId: string | null;
  tier: string;
  /** The desktop T3 Code version. */
  t3: string | null;
  sandboxes: SandboxView[];
  sizes: SizeChoice[];
  sleepChoices: SleepChoice[];
  snapshots: SnapshotView[];
  mcp: McpStatus;
  progress: ProgressView | null;
  error: ErrorView | null;
  startAtLogin: boolean;
  update: { version: string; url: string } | null;
}
