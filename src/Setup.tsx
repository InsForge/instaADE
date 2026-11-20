import { useEffect, useState } from "react";
import { refreshT3, signIn, T3_DOWNLOAD_URL } from "./api";
import { openExternal } from "./common";
import type { Run } from "./common";
import { Alert, Check, Logo } from "./icons";
import type { AppState } from "./types";

/** Reads the T3 Code version on this Mac again, and shows that it did. */
export function CheckT3Button({ run }: { run: Run }) {
  const [phase, setPhase] = useState<"idle" | "checking" | "done">("idle");
  useEffect(() => {
    if (phase !== "done") return;
    const t = setTimeout(() => setPhase("idle"), 1500);
    return () => clearTimeout(t);
  }, [phase]);
  const check = () => {
    setPhase("checking");
    void run(() => refreshT3()).then((ok) => setPhase(ok ? "done" : "idle"));
  };
  return (
    <button className="btn check-t3" disabled={phase === "checking"} onClick={check}>
      {phase === "checking" ? "Checking…" : phase === "done" ? "Checked" : "Check again"}
    </button>
  );
}

/** T3 Code is not on this Mac: shown on first run and at the top of the list. */
export function T3Missing({ run }: { run: Run }) {
  return (
    <div className="warn-card">
      <div className="warn-head">
        <span className="warn-icon">
          <Alert />
        </span>
        <div className="warn-text">
          <span className="strong">T3 Code is not on this Mac</span>
          <span className="small soft">Instabox opens sandboxes in T3 Code. Install it first.</span>
        </div>
      </div>
      <div className="warn-buttons">
        <CheckT3Button run={run} />
        <button className="btn primary" onClick={() => openExternal(T3_DOWNLOAD_URL)}>
          Download T3 Code
        </button>
      </div>
    </div>
  );
}

export default function Setup({ state, run }: { state: AppState; run: Run }) {
  const t3Ok = state.t3 !== null;
  return (
    <>
      <div className="setup">
        <div className="brand-block">
          <Logo />
          <div className="brand-text">
            <h1 className="brand-name">Instabox</h1>
            <span className="muted">Run T3 Code on an instabox sandbox.</span>
          </div>
        </div>
        {!t3Ok && <T3Missing run={run} />}
        {state.signedIn ? (
          <p className="signed-in">
            <Check />
            Signed in{state.login ? ` as ${state.login}` : ""}
          </p>
        ) : (
          <div className="sign-in">
            <button className={`btn lg${t3Ok ? " primary" : ""}`} onClick={() => run(() => signIn("github"))}>
              Sign in with GitHub
            </button>
            <button className="btn lg" onClick={() => run(() => signIn("google"))}>
              Sign in with Google
            </button>
            {state.signInCode && (
              <p className="sign-in-code" role="status">
                Waiting for sign-in in your browser. Your code is <strong className="code">{state.signInCode}</strong>
              </p>
            )}
          </div>
        )}
      </div>
      {t3Ok && (
        <div className="foot found">
          <Check />
          <span>T3 Code {state.t3} found on this Mac</span>
        </div>
      )}
    </>
  );
}
