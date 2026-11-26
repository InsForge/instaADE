import { checkForUpdates, installUpdate } from "./api";
import type { UpdateStep, UpdateView } from "./types";

// Failures show in state.update, so the rejections need no banner of their own.
const check = () => {
  void checkForUpdates().catch(() => {});
};
const install = () => {
  void installUpdate().catch(() => {});
};

const OFFERED: UpdateStep[] = ["available", "downloading", "installing", "failed"];

/** A newer version is on offer: the Settings button gets a dot. */
export const updateOffered = (update: UpdateView | null) => !!update && OFFERED.includes(update.step);

function CheckButton({ disabled }: { disabled?: boolean }) {
  return (
    <button className="btn sm" disabled={disabled} onClick={check}>
      Check for updates
    </button>
  );
}

/** The row under the version: what a check found or how the update goes, and its action. */
function Status({ update }: { update: UpdateView }) {
  const { enabled, step, version, percent, error } = update;
  if (!enabled) {
    return (
      <div className="group-row update-status">
        <span className="muted">Updates are only for released builds</span>
        <CheckButton disabled />
      </div>
    );
  }
  switch (step) {
    case "checking":
      return (
        <div className="group-row update-status" role="status">
          <span className="muted">Checking for updates…</span>
          <CheckButton disabled />
        </div>
      );
    case "upToDate":
      return (
        <div className="group-row update-status" role="status">
          <span className="muted">Up to date</span>
          <CheckButton />
        </div>
      );
    case "available":
      return (
        <div className="group-row update-status" role="status">
          <span>{version} is available</span>
          <button className="btn sm primary" onClick={install}>
            Update
          </button>
        </div>
      );
    case "downloading":
    case "installing": {
      const installing = step === "installing";
      const done = installing ? 100 : (percent ?? 0);
      return (
        <div className="group-row update-status update-progress" role="status">
          <span>
            {installing
              ? `Installing ${version}, then restarting`
              : `Downloading ${version}${percent === null ? "" : ` (${percent}%)`}`}
          </span>
          <div
            className="bar"
            role="progressbar"
            aria-label="Update progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={installing || percent !== null ? done : undefined}
          >
            <span style={{ width: `${done}%` }} />
          </div>
        </div>
      );
    }
    case "checkFailed":
      return (
        <div className="group-row update-status" role="alert">
          <span className="red">{error?.message ?? "Could not check for updates"}</span>
          <CheckButton />
        </div>
      );
    case "failed":
      return (
        <div className="group-row update-status" role="alert">
          <span className="red">{error?.message ?? `Could not update to ${version}`}</span>
          <button className="btn sm" onClick={install}>
            Retry
          </button>
        </div>
      );
    default:
      return (
        <div className="group-row update-status">
          <span className="muted">Checks every few hours</span>
          <CheckButton />
        </div>
      );
  }
}

/** On the sign-in page, which has no Settings: an update on offer and its action, so a signed-out app still updates. */
export function SignedOutUpdate({ update }: { update: UpdateView | null }) {
  if (!update || !updateOffered(update)) return null;
  return (
    <div className="group signed-out-update" role="group" aria-label="Update">
      <Status update={update} />
    </div>
  );
}

/** Settings > Updates: this build's version, then what a check found and its action. */
export default function UpdatesSection({ update }: { update: UpdateView }) {
  return (
    <section className="section">
      <h2 className="section-label">Updates</h2>
      <div className="group">
        <div className="group-row">
          <span>Version</span>
          <span className="small muted">{update.current}</span>
        </div>
        <Status update={update} />
      </div>
    </section>
  );
}
