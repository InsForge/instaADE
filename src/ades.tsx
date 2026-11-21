import { adeLogoUrl } from "./hero/ade-logo";
import { MarkTile } from "./icons";
import type { AdeId, AppState, SandboxView } from "./types";

/** What the popover is for: plain Instabox (sandboxes only) or opening sandboxes in an ADE. */
export type Mode = "instabox" | AdeId;

/** An ADE the switcher lists but the app does not support yet. */
export type SoonId = "orca";

/** The ADEs the switcher shows as coming soon, after the ones the app supports. */
export const COMING_SOON: { id: SoonId; name: string }[] = [{ id: "orca", name: "Orca" }];

/** The mark of an ADE (its logo on a tile), or the instabox mark for plain Instabox. */
export function AdeBadge({ id, size = 20 }: { id: Mode | SoonId; size?: number }) {
  if (id === "instabox") return <MarkTile size={size} />;
  const logo = adeLogoUrl(id);
  return (
    <span className="ade-logo" data-ade={id} aria-hidden="true" style={{ width: size, height: size }}>
      {logo && <img src={logo} alt="" />}
    </span>
  );
}

const NAMES: Record<AdeId, string> = { t3: "T3 Code", cmux: "cmux", paradev: "Paradev" };

/** The desktop app's name, such as "T3 Code". */
export const adeName = (state: AppState, id: AdeId) => state.ades.find((a) => a.id === id)?.name ?? NAMES[id];

/** The ADE's desktop app on this computer, if it runs here. */
export const localAde = (state: AppState, id: AdeId) => state.ades.find((a) => a.id === id);

/** The name of a mode, for the switcher. */
export const modeName = (state: AppState, mode: Mode) => (mode === "instabox" ? "InstaBox" : adeName(state, mode));

/** True when the ADE is set up on the sandbox (its mark lists it). */
export const hasAde = (s: SandboxView, id: AdeId) => s.ades.includes(id);

/** T3 pairs with a server it installs, and keeps its version in step; cmux and Paradev open a link. */
export const pairs = (id: AdeId) => id === "t3";

/**
 * What to do in an ADE whose own app finishes the setup, once InstaBox copied the SSH address
 * and opened the app (the connect ended with finishSetupInAde). Only Paradev does this.
 */
export const finishSetupText = (state: AppState, id: AdeId) =>
  `The SSH address is copied. In ${adeName(state, id)}, click Set up a host over SSH and paste it.`;

/** True when the ADE's app is on this computer, so it can open sandboxes here. */
export const onThisComputer = (state: AppState, id: AdeId) => localAde(state, id)?.version != null;

/** True when this computer can open sandbox `s` in `id` now: the app is here, the sandbox has
 * the ADE, and a pairing ADE was paired from here. */
export const connectedHere = (state: AppState, s: SandboxView, id: AdeId) =>
  onThisComputer(state, id) && hasAde(s, id) && (!pairs(id) || s.connected);

/** The line under a row that has the ADE. */
export function readyText(state: AppState, s: SandboxView, id: AdeId): string {
  if (id === "t3") return s.t3Version ? `T3 Code ${s.t3Version}` : "T3 Code";
  return `Ready for ${adeName(state, id)}`;
}
