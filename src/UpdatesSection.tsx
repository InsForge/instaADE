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

/** What a check found, or how the update goes, under the version. */
function Status({ update }: { update: UpdateView }) {
  const { enabled, step, version, percent, error } = update;
  if (!enabled) {
    return (
      <div className="group-row update-status">
        <span className="small muted">Updates are only for released builds</span>
      </div>
    );
  }
  switch (step) {
    case "checking":
      return (
        <div className="group-row update-status" role="status">
          <span className="small muted">Checking for updates…</span>
        </div>
      );
    case "upToDate":
      return (
        <div className="group-row update-status" role="status">
          <span className="small muted">InstaBox is up to date</span>
        </div>
      );
    case "available":
      return (
        <div className="group-row update-status" role="status">
          <span className="small">InstaBox {version} is available</span>
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
          <span className="small">
            {installing
              ? `Installing InstaBox ${version}, then restarting`
              : `Downloading InstaBox ${version}${percent === null ? "" : ` (${percent}%)`}`}
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
          <span className="small red">{error?.message ?? "Could not check for updates"}</span>
        </div>
      );
    case "failed":
      return (
        <div className="group-row update-status" role="alert">
          <span className="small red">{error?.message ?? `Could not update to ${version}`}</span>
          <button className="btn sm" onClick={install}>
            Retry
          </button>
        </div>
      );
    default:
      return null;
  }
}

/** Settings > Updates: this build's version, Check for updates, and the update on offer. */
export default function UpdatesSection({ update }: { update: UpdateView }) {
  const offered = updateOffered(update);
  return (
    <section className="section">
      <h2 className="section-label">Updates</h2>
      <div className="group">
        <div className="group-row plain">
          <span>Version {update.current}</span>
          {!offered && (
            <button className="btn sm" disabled={!update.enabled || update.step === "checking"} onClick={check}>
              Check for updates
            </button>
          )}
        </div>
        <Status update={update} />
      </div>
    </section>
  );
}
