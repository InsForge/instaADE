import { dismissOldApp, trashOldApp } from "./api";
import type { Run } from "./common";

/** The old app (Instabox.app) is still installed: Move to Trash, or Not now until the next launch. */
export default function OldAppNotice({ run }: { run: Run }) {
  return (
    <div className="banner" role="status">
      <span className="banner-text">Instabox is still in Applications. InstaADE replaces it.</span>
      <button className="btn sm primary" onClick={() => run(() => trashOldApp())}>
        Move to Trash
      </button>
      <button className="btn sm" onClick={() => run(() => dismissOldApp())}>
        Not now
      </button>
    </div>
  );
}
