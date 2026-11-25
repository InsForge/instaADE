import { installUpdate, updateLater } from "./api";
import type { UpdateView } from "./types";

// A failure shows in state.update, so the rejections need no banner of their own.
const install = () => {
  void installUpdate().catch(() => {});
};
const later = () => {
  void updateLater().catch(() => {});
};

/** The update of InstaBox on offer: Update and Later, the download, or why it failed with Retry. */
export default function UpdateRow({ update }: { update: UpdateView }) {
  const { version, step, percent } = update;
  if (step === "failed") {
    return (
      <div className="banner danger" role="alert">
        <span className="banner-text">{update.error?.message ?? `Could not update to ${version}`}</span>
        <button className="btn sm" onClick={install}>
          Retry
        </button>
        <button className="btn sm" onClick={later}>
          Later
        </button>
      </div>
    );
  }
  if (step === "available") {
    return (
      <div className="banner" role="status">
        <span className="banner-text">InstaBox {version} is available</span>
        <button className="btn sm primary" onClick={install}>
          Update
        </button>
        <button className="btn sm" onClick={later}>
          Later
        </button>
      </div>
    );
  }
  const installing = step === "installing";
  const done = installing ? 100 : (percent ?? 0);
  return (
    <div className="banner update-progress" role="status">
      <span className="banner-text">
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
