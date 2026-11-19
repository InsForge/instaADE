//! Keeps the side effects of a long flow (saving a credential, writing a config file or the
//! connected list, opening a link or a terminal, starting a process) from happening after the
//! account they began under has signed out or changed.
//!
//! One lock, the account lock (`Core::side_effects`), orders all of it. Every account change
//! (sign-out, a device sign-in, the session saved by the last run, a session the server
//! ended) takes it, moves the account count on, ends the old session and clears or saves the
//! stores before it lets go. A gate takes the same lock, compares the flow's account count
//! with the current one and does the work before it lets go, so a change can never fall
//! between the check and the work.
//!
//! A session the server ends (a refresh answers 401, in any command or background refresh)
//! tells a watcher the app starts for each session it installs. The watcher makes the same
//! change as a sign-out at once, so no command has to notice the end itself.
//!
//! Lock order, outermost first. A thread that holds one of these locks may take a later one,
//! never an earlier one:
//!
//! 1. a connected-list store's own lock (the store writes through a gate);
//! 2. the account lock (`Core::side_effects`);
//! 3. the app state lock (`Core::inner`), held only for short reads and writes;
//! 4. a session's own lock (`auth`), which makes "check that the session is alive, then save
//!    or clear its tokens" one step;
//! 5. the stores themselves (keychain entries, files, in-memory test stores) and a session's
//!    end signal, which only wakes the watcher task and never runs it in place.
//!
//! A refresh holds its session's token lock across the network call and then takes 4; nothing
//! that holds 2, 3 or 4 waits for that token lock. Locks 2 and 3 are never held across an
//! await, and the state is emitted only after both are released. The only callbacks run under
//! 2 are the launches themselves (the URL opener, Terminal, the port forward, the `claude`
//! command); none of them calls back into the app.

/// A flow's permission to act. `run` does the work only while the flow's account is still
/// the current one, and it does the check and the work as one step with respect to every
/// account change.
pub trait Gate: Send + Sync {
    /// Runs `f` if the account is unchanged. Returns false when `f` did not run.
    fn run(&self, f: &mut dyn FnMut()) -> bool;
}

/// A gate that never closes, for flows that have no account.
pub struct Open;

impl Gate for Open {
    fn run(&self, f: &mut dyn FnMut()) -> bool {
        f();
        true
    }
}

/// Runs `f` through `gate`; None when the account changed and `f` did not run.
pub fn guarded<R>(gate: &dyn Gate, f: impl FnOnce() -> R) -> Option<R> {
    let mut f = Some(f);
    let mut out = None;
    gate.run(&mut || {
        if let Some(f) = f.take() {
            out = Some(f());
        }
    });
    out
}
