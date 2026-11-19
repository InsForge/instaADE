import { useState } from "react";
import type { FormEvent } from "react";
import { createSandbox } from "./api";
import Progress from "./Progress";
import type { Run } from "./SandboxList";
import type { AppState, SleepChoice } from "./types";

/** The server's default idle time (30 min), else the first choice that sleeps at all. */
export function defaultSleep(choices: SleepChoice[]): number {
  if (choices.some((c) => c.seconds === 1800)) return 1800;
  return choices.find((c) => c.seconds !== 0)?.seconds ?? 1800;
}

interface Props {
  state: AppState;
  run: Run;
  onConnect: (id: string) => void;
  /** After a successful create: back to the list. */
  onCreated: () => void;
  onBack: () => void;
}

export default function NewSandbox({ state, run, onConnect, onCreated, onBack }: Props) {
  const free = state.tier === "free";
  const [name, setName] = useState("");
  const [fromSnapshot, setFromSnapshot] = useState(false);
  // What the user picked. Until then (or once it leaves the list) the default applies:
  // the first ready snapshot, the plan's first size, the default auto sleep.
  const [pickedSnapshot, setPickedSnapshot] = useState("");
  const [pickedSize, setPickedSize] = useState("");
  const [pickedSleep, setPickedSleep] = useState<number | null>(null);

  const snapshotId = state.snapshots.some((s) => s.id === pickedSnapshot) ? pickedSnapshot : (state.snapshots[0]?.id ?? "");
  const size = state.sizes.find((z) => z.label === pickedSize) ?? state.sizes[0];
  const sleep =
    pickedSleep !== null && state.sleepChoices.some((c) => c.seconds === pickedSleep) ? pickedSleep : defaultSleep(state.sleepChoices);
  const working = state.progress !== null && state.progress.step !== "done" && !state.progress.error;
  // A failed create left a sandbox: offer only Retry on it (or × to close), not a second one.
  const failedWithSandbox = !!state.progress?.machineId && !!state.progress.error;
  const canCreate =
    name.trim() !== "" &&
    !working &&
    !failedWithSandbox &&
    (free || size !== undefined) &&
    (!fromSnapshot || snapshotId !== "");

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!canCreate) return;
    // The free plan fixes size and sleep, so the app sends the plan's values.
    const vcpus = free ? 4 : size.vcpus;
    const memMiB = free ? 8192 : size.memMiB;
    void run(() =>
      createSandbox({
        name: name.trim(),
        snapshotId: fromSnapshot ? snapshotId : undefined,
        vcpus,
        memMiB,
        autoSuspendAfterSeconds: free ? 1800 : sleep,
      }),
    ).then((ok) => ok && onCreated());
  };

  return (
    <section className="card">
      <h1>New sandbox</h1>
      <form onSubmit={submit} className="form">
        <label htmlFor="sb-name">Name</label>
        <input id="sb-name" value={name} onChange={(e) => setName(e.target.value)} placeholder="dev-2" />

        <span className="field-label">Start from</span>
        <div className="radios">
          <label>
            <input type="radio" name="from" checked={!fromSnapshot} onChange={() => setFromSnapshot(false)} /> Standard Linux
          </label>
          <label>
            <input
              type="radio"
              name="from"
              checked={fromSnapshot}
              disabled={state.snapshots.length === 0}
              onChange={() => setFromSnapshot(true)}
            />{" "}
            My snapshot
          </label>
          {fromSnapshot && (
            <select aria-label="Snapshot" value={snapshotId} onChange={(e) => setPickedSnapshot(e.target.value)}>
              {state.snapshots.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          )}
        </div>

        <label htmlFor="sb-size">Size</label>
        <div>
          <select id="sb-size" disabled={free} value={free ? "free" : (size?.label ?? "")} onChange={(e) => setPickedSize(e.target.value)}>
            {free ? (
              <option value="free">4 vCPU / 8 GB</option>
            ) : (
              state.sizes.map((s) => (
                <option key={s.label} value={s.label}>
                  {s.label}
                </option>
              ))
            )}
          </select>
          {free && <span className="muted hint"> Free plan: fixed</span>}
        </div>

        <label htmlFor="sb-sleep">Auto sleep</label>
        <div>
          <select id="sb-sleep" disabled={free} value={free ? "free" : sleep} onChange={(e) => setPickedSleep(Number(e.target.value))}>
            {free ? (
              <option value="free">After 30 min</option>
            ) : (
              state.sleepChoices.map((s) => (
                <option key={s.seconds} value={s.seconds}>
                  {s.label}
                </option>
              ))
            )}
          </select>
          {free && <span className="muted hint"> Free plan: fixed</span>}
        </div>

        <div className="form-actions">
          <button type="button" className="btn" onClick={onBack}>
            Cancel
          </button>
          <button type="submit" className="btn primary" disabled={!canCreate}>
            Create &amp; connect
          </button>
        </div>
      </form>
      <Progress state={state} run={run} onConnect={onConnect} />
    </section>
  );
}
