//! Keeps the side effects of a long flow (saving a credential, writing a config file,
//! opening a link or a terminal) from happening after the account they began under has
//! signed out or changed.

/// A flow's permission to act. `run` does the work only while the flow's account is still
/// the current one, and it does the check and the work as one step with respect to sign-out.
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
