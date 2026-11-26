//! Loads setlist song items on threads of their own, so a slow, sleeping or
//! unplugged volume never holds up the control thread: the thread a shell
//! polls from, which also carries Panic and the transport.
//!
//! Each load gets its own thread rather than a queue on one: a read that
//! hangs then holds up only the item it reads, never the loads after it.
//! The control thread picks the item up from a [`Load`] without waiting
//! ([`Load::take`]), or waits for it a bounded time ([`Load::wait`]).

use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Instant;

/// A value built on a thread of its own: empty until the thread is done.
/// Dropping it before then leaves the thread to drop the value itself, on
/// that thread.
pub(crate) struct Load<T> {
    slot: Arc<Slot<T>>,
}

/// Where a load thread leaves what it built.
struct Slot<T> {
    value: Mutex<Option<T>>,
    filled: Condvar,
}

impl<T> Slot<T> {
    fn fill(&self, value: T) {
        *lock(&self.value) = Some(value);
        self.filled.notify_all();
    }
}

impl<T: Send + 'static> Load<T> {
    /// Runs `job` on a thread of its own. A thread that cannot start, or a
    /// job that panics, leaves `failed` with the reason.
    pub(crate) fn spawn(job: impl FnOnce() -> T + Send + 'static, failed: fn(String) -> T) -> Self {
        let slot = Arc::new(Slot {
            value: Mutex::new(None),
            filled: Condvar::new(),
        });
        let theirs = Arc::clone(&slot);
        let started = thread::Builder::new()
            .name("setlist-load".to_owned())
            .spawn(move || {
                let value = panic::catch_unwind(AssertUnwindSafe(job))
                    .unwrap_or_else(|_| failed("loading it panicked".to_owned()));
                theirs.fill(value);
            });
        if let Err(error) = started {
            slot.fill(failed(format!("its load thread could not start: {error}")));
        }
        Self { slot }
    }
}

impl<T> Load<T> {
    /// What the thread built, once it is done, taken out; `None` before then
    /// and once taken.
    pub(crate) fn take(&self) -> Option<T> {
        lock(&self.slot.value).take()
    }

    /// Waits until the thread is done, at most until `deadline`; whether it
    /// is (or what it built was taken).
    pub(crate) fn wait(&self, deadline: Instant) -> bool {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let (value, _) = self
            .slot
            .filled
            .wait_timeout_while(lock(&self.slot.value), timeout, |value| value.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        value.is_some()
    }

    /// `read` applied to what the thread built, without taking it; `None`
    /// before the thread is done.
    pub(crate) fn peek<R>(&self, read: impl FnOnce(&T) -> R) -> Option<R> {
        lock(&self.slot.value).as_ref().map(read)
    }
}

/// The slot's lock: nothing panics while holding it, so a poisoned one
/// still holds a whole value.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_load_is_empty_until_its_thread_is_done_then_taken_once() {
        let (release, wait) = mpsc::channel::<()>();
        let load = Load::spawn(
            move || {
                wait.recv().unwrap();
                7
            },
            |_| 0,
        );
        assert_eq!(load.take(), None);
        assert!(!load.wait(Instant::now() + Duration::from_millis(20)));
        release.send(()).unwrap();
        assert!(load.wait(Instant::now() + Duration::from_secs(10)));
        assert_eq!(load.peek(|value| *value), Some(7));
        assert_eq!(load.take(), Some(7));
        assert_eq!(load.take(), None);
    }

    #[test]
    fn a_job_that_panics_leaves_the_failure() {
        let load = Load::spawn(|| -> Result<u8, String> { panic!("boom") }, Err);
        assert!(load.wait(Instant::now() + Duration::from_secs(10)));
        assert_eq!(load.take(), Some(Err("loading it panicked".to_owned())));
    }
}
