import { refreshT3, signIn, T3_DOWNLOAD_URL } from "./api";
import { ExternalLink } from "./ErrorNotice";
import type { Run } from "./SandboxList";
import type { AppState } from "./types";

interface Props {
  state: AppState;
  run: Run;
  onCreate: () => void;
}

export default function Setup({ state, run, onCreate }: Props) {
  const t3Ok = state.t3 !== null;
  const signedIn = state.signedIn;
  return (
    <section className="card setup">
      <h1>Set up Instabox Bridge</h1>
      <ol className="checks">
        <li>
          <span className="num">1</span>
          <span className="label">T3 Code</span>
          {t3Ok ? (
            <span className="success">✓ Found v{state.t3}</span>
          ) : (
            <span className="row-actions">
              <ExternalLink url={T3_DOWNLOAD_URL}>Download T3 Code</ExternalLink>
              <button className="btn small" onClick={() => run(() => refreshT3())}>
                Check again
              </button>
            </span>
          )}
        </li>
        <li>
          <span className="num">2</span>
          <span className="label">Instabox</span>
          {signedIn ? (
            <span className="success">✓ Signed in{state.login ? ` as ${state.login}` : ""}</span>
          ) : (
            <span className="row-actions">
              <button className="btn" onClick={() => run(() => signIn("github"))}>
                Sign in with GitHub
              </button>
              <button className="btn" onClick={() => run(() => signIn("google"))}>
                Sign in with Google
              </button>
            </span>
          )}
        </li>
        {!signedIn && state.signInCode && (
          <li className="sub">
            Waiting for sign-in in your browser. Your code is <strong className="mono code">{state.signInCode}</strong>
          </li>
        )}
        <li>
          <span className="num">3</span>
          <span className="label">Done</span>
          {t3Ok && signedIn && (
            <button className="btn primary" onClick={onCreate}>
              Create your first sandbox
            </button>
          )}
        </li>
      </ol>
    </section>
  );
}
