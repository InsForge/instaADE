//! This computer's clipboard, for a secret the user pastes once, such as the SSH address
//! Paradev's host setup takes. The copy is cleared a minute later, but only while the clipboard
//! still holds exactly that secret: whatever the user copied since stays. Text that is no
//! secret, such as Paseo's host address, is copied and left there.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::process::{RunError, command, run_bounded, run_with_input};

/// How long a copied secret stays on the clipboard.
pub const CLEAR_AFTER: Duration = Duration::from_secs(60);

/// A clipboard command that takes longer counts as failed.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Reads and writes this computer's clipboard. Both block for a moment.
pub trait Clipboard: Send + Sync {
    /// The text on the clipboard; None when it cannot be read.
    fn read(&self) -> Option<String>;
    fn write(&self, text: &str) -> Result<(), String>;
}

/// `pbpaste` and `pbcopy` on macOS. No other platform has a clipboard here yet: Paradev, a
/// macOS app, needs it, and Paseo on Windows shows its address without copying it.
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn read(&self) -> Option<String> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        match run_bounded(command("/usr/bin/pbpaste"), TIMEOUT) {
            Ok((Some(0), out, _)) => String::from_utf8(out).ok(),
            _ => None,
        }
    }

    fn write(&self, text: &str) -> Result<(), String> {
        if !cfg!(target_os = "macos") {
            return Err("Copying to the clipboard is only on macOS for now".to_string());
        }
        match run_with_input(command("/usr/bin/pbcopy"), text.as_bytes(), TIMEOUT) {
            Ok((Some(0), _, _)) => Ok(()),
            Ok((code, _, err)) => Err(format!(
                "Could not copy to the clipboard (pbcopy exited with {code:?}): {}",
                String::from_utf8_lossy(&err).trim()
            )),
            Err(RunError::Spawn(e)) => Err(format!("Could not copy to the clipboard: {e}")),
            Err(RunError::TimedOut) => Err("Copying to the clipboard took too long".to_string()),
        }
    }
}

/// Waits for a duration: tokio's sleep in the app, a test's own clock in a test.
pub type Sleep = Arc<dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Copies secrets and clears each one `CLEAR_AFTER` later. A newer copy restarts the wait: only
/// the last copy is cleared, a full `CLEAR_AFTER` after it was made. When the app quits first,
/// `clear_pending_within` clears it then.
pub struct SecretClipboard {
    clipboard: Arc<dyn Clipboard>,
    sleep: Sleep,
    /// Its lock is held through each copy and each clearing, so a newer copy never lands
    /// between a clearing's read and its write.
    copies: Arc<Mutex<Copies>>,
}

#[derive(Default)]
struct Copies {
    /// Counts copies, so a wait that a newer copy replaced clears nothing.
    count: u64,
    /// The last secret copied, until it is cleared or left alone.
    pending: Option<String>,
}

impl SecretClipboard {
    pub fn new(clipboard: Arc<dyn Clipboard>, sleep: Sleep) -> SecretClipboard {
        SecretClipboard {
            clipboard,
            sleep,
            copies: Arc::new(Mutex::new(Copies::default())),
        }
    }

    /// This computer's clipboard, on tokio's clock.
    pub fn system() -> SecretClipboard {
        SecretClipboard::new(
            Arc::new(SystemClipboard),
            Arc::new(|d| Box::pin(tokio::time::sleep(d))),
        )
    }

    /// Puts `secret` on the clipboard and clears it `CLEAR_AFTER` later (see `clear_if_still`).
    /// Blocks for a moment. The clearing needs a tokio runtime; without one the secret stays.
    pub fn copy(&self, secret: &str) -> Result<(), String> {
        self.copy_then_clear(secret).map(drop)
    }

    /// Puts `text`, which is no secret (Paseo's host address), on the clipboard and leaves it
    /// there. It waits for a clearing that runs, so the clearing cannot wipe it. A secret still
    /// pending is no longer on the clipboard, so its clearing leaves the text alone.
    pub fn copy_plain(&self, text: &str) -> Result<(), String> {
        let _copies = self.copies.lock().unwrap_or_else(PoisonError::into_inner);
        self.clipboard.write(text)
    }

    /// `copy`, with the task that clears the secret, so a test can wait for it.
    fn copy_then_clear(&self, secret: &str) -> Result<Option<tokio::task::JoinHandle<()>>, String> {
        let this = {
            let mut copies = self.copies.lock().unwrap_or_else(PoisonError::into_inner);
            self.clipboard.write(secret)?;
            copies.count += 1;
            copies.pending = Some(secret.to_string());
            copies.count
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return Ok(None);
        };
        let (clipboard, copies) = (self.clipboard.clone(), self.copies.clone());
        let wait = (self.sleep)(CLEAR_AFTER);
        Ok(Some(rt.spawn(async move {
            wait.await;
            let _ = tokio::task::spawn_blocking(move || {
                let mut copies = copies.lock().unwrap_or_else(PoisonError::into_inner);
                if copies.count == this {
                    clear_pending(&mut copies, &*clipboard);
                }
            })
            .await;
        })))
    }

    /// The app is quitting: clears the secret whose clearing is still pending, by the same rule
    /// as `CLEAR_AFTER` (only while the clipboard holds exactly it). Waits at most `wait`, on a
    /// thread of its own, so a stuck clipboard command never holds up the quit. True when it
    /// finished in time.
    pub fn clear_pending_within(&self, wait: Duration) -> bool {
        let (clipboard, copies) = (self.clipboard.clone(), self.copies.clone());
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut copies = copies.lock().unwrap_or_else(PoisonError::into_inner);
            clear_pending(&mut copies, &*clipboard);
            let _ = done.send(());
        });
        finished.recv_timeout(wait).is_ok()
    }
}

/// Clears the pending secret if the clipboard still holds exactly it. Either way nothing is
/// pending after.
fn clear_pending(copies: &mut Copies, clipboard: &dyn Clipboard) {
    if let Some(secret) = copies.pending.take() {
        clear_if_still(clipboard, &secret);
    }
}

/// Empties the clipboard when it holds exactly `secret`. Anything else, or a clipboard that
/// cannot be read, is left alone.
fn clear_if_still(clipboard: &dyn Clipboard, secret: &str) {
    if clipboard.read().as_deref() == Some(secret) {
        let _ = clipboard.write("");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::sync::watch;

    /// A clipboard in memory that records every write.
    #[derive(Default)]
    struct FakeClipboard {
        text: Mutex<Option<String>>,
        writes: Mutex<Vec<String>>,
    }

    impl Clipboard for FakeClipboard {
        fn read(&self) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
        fn write(&self, text: &str) -> Result<(), String> {
            *self.text.lock().unwrap() = Some(text.to_string());
            self.writes.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    impl FakeClipboard {
        fn text(&self) -> Option<String> {
            self.read()
        }
        /// The user copies something else.
        fn user_copies(&self, text: &str) {
            *self.text.lock().unwrap() = Some(text.to_string());
        }
    }

    /// A clock that moves only when the test moves it.
    struct FakeClock(watch::Sender<Duration>);

    impl FakeClock {
        fn new() -> Arc<FakeClock> {
            Arc::new(FakeClock(watch::channel(Duration::ZERO).0))
        }
        fn sleep(self: &Arc<Self>) -> Sleep {
            let clock = self.clone();
            Arc::new(move |d| {
                let mut now = clock.0.subscribe();
                let until = *now.borrow() + d;
                Box::pin(async move {
                    let _ = now.wait_for(|t| *t >= until).await;
                })
            })
        }
        fn advance(&self, secs: u64) {
            self.0.send_modify(|t| *t += Duration::from_secs(secs));
        }
    }

    fn setup() -> (Arc<FakeClipboard>, Arc<FakeClock>, SecretClipboard) {
        let board = Arc::new(FakeClipboard::default());
        let clock = FakeClock::new();
        let secret = SecretClipboard::new(board.clone(), clock.sleep());
        (board, clock, secret)
    }

    const ADDRESS: &str = "ssh://ibssh_new@ssh.example.test:2222";

    /// Waits until the clearing task is done, or a short real time has passed.
    async fn settled(task: &mut tokio::task::JoinHandle<()>) -> bool {
        tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn the_address_is_cleared_a_minute_after_the_copy() {
        let (board, clock, secret) = setup();
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        assert_eq!(board.text().as_deref(), Some(ADDRESS));
        clock.advance(59);
        assert!(!settled(&mut task).await, "still waiting at 59 s");
        assert_eq!(board.text().as_deref(), Some(ADDRESS));
        clock.advance(1);
        assert!(settled(&mut task).await);
        assert_eq!(board.text().as_deref(), Some(""));
        assert_eq!(*board.writes.lock().unwrap(), [ADDRESS, ""]);
        assert_eq!(CLEAR_AFTER, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn what_the_user_copied_since_is_never_wiped() {
        let (board, clock, secret) = setup();
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        clock.advance(10);
        board.user_copies("my own note");
        clock.advance(50);
        assert!(settled(&mut task).await);
        assert_eq!(board.text().as_deref(), Some("my own note"));
        assert_eq!(*board.writes.lock().unwrap(), [ADDRESS], "no write");
        // A copy that differs only a little is not the address either.
        let (board, clock, secret) = setup();
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        board.user_copies(&format!("{ADDRESS}\n"));
        clock.advance(60);
        assert!(settled(&mut task).await);
        assert_eq!(board.text(), Some(format!("{ADDRESS}\n")));
    }

    #[tokio::test]
    async fn a_newer_copy_restarts_the_wait() {
        for second in [ADDRESS, "ssh://ibssh_other@ssh.example.test:2222"] {
            let (board, clock, secret) = setup();
            let mut first = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
            clock.advance(30);
            let mut last = secret.copy_then_clear(second).unwrap().unwrap();
            clock.advance(30);
            assert!(settled(&mut first).await);
            assert_eq!(
                board.text().as_deref(),
                Some(second),
                "60 s after the first copy"
            );
            clock.advance(29);
            assert!(!settled(&mut last).await);
            assert_eq!(
                board.text().as_deref(),
                Some(second),
                "59 s after the last copy"
            );
            clock.advance(1);
            assert!(settled(&mut last).await);
            assert_eq!(
                board.text().as_deref(),
                Some(""),
                "60 s after the last copy"
            );
        }
    }

    #[tokio::test]
    async fn a_newer_copy_during_a_clearing_is_not_wiped() {
        use std::sync::mpsc;
        /// Holds the clearing right after its read, until the test lets it go on.
        struct SlowRead {
            inner: FakeClipboard,
            read: Mutex<Option<mpsc::Sender<()>>>,
            go: Mutex<Option<mpsc::Receiver<()>>>,
        }
        impl Clipboard for SlowRead {
            fn read(&self) -> Option<String> {
                let text = self.inner.read();
                if let Some(read) = self.read.lock().unwrap().take() {
                    read.send(()).unwrap();
                    let go = self.go.lock().unwrap().take().unwrap();
                    go.recv().unwrap();
                }
                text
            }
            fn write(&self, text: &str) -> Result<(), String> {
                self.inner.write(text)
            }
        }
        let (read_tx, read_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let board = Arc::new(SlowRead {
            inner: FakeClipboard::default(),
            read: Mutex::new(Some(read_tx)),
            go: Mutex::new(Some(go_rx)),
        });
        let clock = FakeClock::new();
        let secret = Arc::new(SecretClipboard::new(board.clone(), clock.sleep()));
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        clock.advance(60);
        // The clearing has read the address and not written yet.
        tokio::task::spawn_blocking(move || read_rx.recv().unwrap())
            .await
            .unwrap();
        let newer = "ssh://ibssh_other@ssh.example.test:2222";
        let copier = secret.clone();
        let copy = std::thread::spawn(move || copier.copy(newer).unwrap());
        // Time for the newer copy to land, if it could.
        tokio::time::sleep(Duration::from_millis(50)).await;
        go_tx.send(()).unwrap();
        assert!(settled(&mut task).await);
        copy.join().unwrap();
        assert_eq!(board.inner.text().as_deref(), Some(newer));
    }

    #[tokio::test]
    async fn plain_text_stays_and_no_clearing_wipes_it() {
        let (board, clock, secret) = setup();
        secret.copy_plain("ssh://instabox-dev-1").unwrap();
        clock.advance(120);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(secret.clear_pending_within(QUIT_WAIT));
        assert_eq!(board.text().as_deref(), Some("ssh://instabox-dev-1"));
        // Over a secret whose clearing is pending: the clearing and the quit leave it alone.
        let (board, clock, secret) = setup();
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        clock.advance(30);
        secret.copy_plain("ssh://instabox-dev-1").unwrap();
        clock.advance(30);
        assert!(settled(&mut task).await);
        assert!(secret.clear_pending_within(QUIT_WAIT));
        assert_eq!(board.text().as_deref(), Some("ssh://instabox-dev-1"));
        assert_eq!(
            *board.writes.lock().unwrap(),
            [ADDRESS, "ssh://instabox-dev-1"],
            "nothing cleared"
        );
    }

    #[tokio::test]
    async fn a_clipboard_that_cannot_be_read_is_left_alone() {
        struct Unreadable(Mutex<Vec<String>>);
        impl Clipboard for Unreadable {
            fn read(&self) -> Option<String> {
                None
            }
            fn write(&self, text: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(text.to_string());
                Ok(())
            }
        }
        let board = Arc::new(Unreadable(Mutex::new(Vec::new())));
        let clock = FakeClock::new();
        let secret = SecretClipboard::new(board.clone(), clock.sleep());
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        clock.advance(60);
        assert!(settled(&mut task).await);
        assert_eq!(*board.0.lock().unwrap(), [ADDRESS]);
    }

    const QUIT_WAIT: Duration = Duration::from_secs(1);

    #[tokio::test]
    async fn quitting_clears_an_address_whose_clearing_is_pending() {
        let (board, clock, secret) = setup();
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        clock.advance(10);
        assert!(secret.clear_pending_within(QUIT_WAIT));
        assert_eq!(board.text().as_deref(), Some(""));
        // The minute's clearing later finds nothing left to do.
        board.user_copies(ADDRESS);
        clock.advance(50);
        assert!(settled(&mut task).await);
        assert_eq!(board.text().as_deref(), Some(ADDRESS));
        assert_eq!(*board.writes.lock().unwrap(), [ADDRESS, ""]);
    }

    #[test]
    fn quitting_never_wipes_what_the_user_copied_since() {
        let (board, _clock, secret) = setup();
        secret.copy(ADDRESS).unwrap();
        board.user_copies("my own note");
        assert!(secret.clear_pending_within(QUIT_WAIT));
        assert_eq!(board.text().as_deref(), Some("my own note"));
        assert_eq!(*board.writes.lock().unwrap(), [ADDRESS], "no write");
    }

    #[tokio::test]
    async fn quitting_after_the_clearing_ran_clears_nothing() {
        let (board, clock, secret) = setup();
        let mut task = secret.copy_then_clear(ADDRESS).unwrap().unwrap();
        clock.advance(60);
        assert!(settled(&mut task).await);
        // The user copied the same text again by hand: no clearing is pending for it.
        board.user_copies(ADDRESS);
        assert!(secret.clear_pending_within(QUIT_WAIT));
        assert_eq!(board.text().as_deref(), Some(ADDRESS));
        // Nor when nothing was ever copied.
        let (board, _clock, secret) = setup();
        board.user_copies("my own note");
        assert!(secret.clear_pending_within(QUIT_WAIT));
        assert_eq!(board.text().as_deref(), Some("my own note"));
    }

    #[test]
    fn a_stuck_clipboard_does_not_hold_up_the_quit() {
        use std::sync::mpsc;
        struct Stuck(Mutex<Option<mpsc::Receiver<()>>>);
        impl Clipboard for Stuck {
            fn read(&self) -> Option<String> {
                if let Some(rx) = self.0.lock().unwrap().take() {
                    let _ = rx.recv();
                }
                None
            }
            fn write(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let (release, rx) = mpsc::channel::<()>();
        let clock = FakeClock::new();
        let secret = SecretClipboard::new(Arc::new(Stuck(Mutex::new(Some(rx)))), clock.sleep());
        secret.copy(ADDRESS).unwrap();
        let started = std::time::Instant::now();
        assert!(!secret.clear_pending_within(Duration::from_millis(100)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        drop(release);
    }

    #[test]
    fn without_a_runtime_it_only_copies() {
        let (board, _clock, secret) = setup();
        assert!(secret.copy_then_clear(ADDRESS).unwrap().is_none());
        assert_eq!(board.text().as_deref(), Some(ADDRESS));
    }

    #[test]
    fn the_system_clipboard_is_macos_only() {
        if !cfg!(target_os = "macos") {
            assert_eq!(SystemClipboard.read(), None);
            assert!(SystemClipboard.write("x").is_err());
        }
    }
}
