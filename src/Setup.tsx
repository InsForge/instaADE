import { signIn } from "./api";
import type { Run } from "./common";
import { Logo } from "./icons";
import type { AppState } from "./types";

/** Where the app lives: the menu bar on macOS, the system tray on Windows. */
const home = () => (navigator.userAgent.includes("Windows") ? "the system tray" : "the menu bar");

/** Sign-in. An ADE is never asked for here: it is offered after sign-in, in the list. */
export default function Setup({ state, run }: { state: AppState; run: Run }) {
  return (
    <div className="setup">
      <div className="brand-block">
        <Logo />
        <div className="brand-text">
          <h1 className="brand-name">Instabox</h1>
          <span className="muted">Start, pause and open your sandboxes from {home()}.</span>
        </div>
      </div>
      <div className="sign-in">
        <button className="btn lg primary" onClick={() => run(() => signIn("github"))}>
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
    </div>
  );
}
