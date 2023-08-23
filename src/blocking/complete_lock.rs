use crate::CompleteLockState;
use std::sync::Condvar;
use std::sync::{Arc, Mutex};

/// A completion lock based on `Condvar`s, which are the most efficient mechanism for
/// this kind of lock on platforms that support them.
#[derive(Clone)]
pub(crate) struct CompleteLock {
    pair: Arc<(Mutex<CompleteLockState>, Condvar)>,
}
impl CompleteLock {
    /// Creates a new, incomplete, lock.
    pub(crate) fn new() -> Self {
        Self {
            pair: Arc::new((Mutex::new(CompleteLockState::Incomplete), Condvar::new())),
        }
    }
    /// Gets the current state of the lock.
    pub(crate) fn state(&self) -> CompleteLockState {
        let (lock, _) = &*self.pair;
        *lock.lock().unwrap()
    }
    /// Marks the lock as complete. If the lock was previously poisoned, this will override that.
    pub(crate) fn mark_complete(&self) {
        let (lock, cvar) = &*self.pair;
        *lock.lock().unwrap() = CompleteLockState::Complete;
        cvar.notify_all();
    }
    /// Poisons the lock, signalling to any waiters that what it guards is now in an invalid
    /// state.
    pub(crate) fn poison(&self) {
        let (lock, cvar) = &*self.pair;
        let mut state = lock.lock().unwrap();
        // We can only poison incomplete locks
        if *state == CompleteLockState::Incomplete {
            *state = CompleteLockState::Poisoned;
            cvar.notify_all();
        }
    }
    /// Waits until the lock is completed, returning its state. This will also return
    /// if/when the lock is explicitly poisoned.
    #[must_use] // Must check for poisoning!
    pub(crate) fn wait_for_completion(&self) -> CompleteLockState {
        let (lock, cvar) = &*self.pair;
        let mut state = lock.lock().unwrap();
        while *state == CompleteLockState::Incomplete {
            state = cvar.wait(state).unwrap();
        }
        *state
    }
}

#[cfg(test)]
mod tests {
    use super::CompleteLock;
    use crate::CompleteLockState;

    /// The amount of time we should wait for threads to be updated by a `CompleteLock`, in milliseconds.
    static THREAD_WAITING_TIME: u64 = 400;

    #[test]
    fn should_unlock_waiter_no_threads() {
        // Very simple test case, but it's the best we can do in single-threaded Wasm
        let lock = CompleteLock::new();

        lock.mark_complete();
        let state = lock.wait_for_completion();
        assert_eq!(state, CompleteLockState::Complete);
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn should_unlock_multiple_waiters() -> Result<(), Box<dyn std::error::Error>> {
        let lock = CompleteLock::new();

        let lock_1 = lock.clone();
        let waiter_1 = std::thread::spawn(move || {
            let _ = lock_1.wait_for_completion();
            true
        });

        let lock_2 = lock.clone();
        let waiter_2 = std::thread::spawn(move || {
            let _ = lock_2.wait_for_completion();
            true
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!waiter_1.is_finished());
        assert!(!waiter_2.is_finished());

        lock.mark_complete();
        std::thread::sleep(std::time::Duration::from_millis(THREAD_WAITING_TIME));
        assert!(waiter_1.is_finished());
        assert!(waiter_2.is_finished());

        let res_1 = waiter_1.join().expect("couldn't wait for waiter 1");
        let res_2 = waiter_2.join().expect("couldn't wait for waiter 2");

        assert!(res_1 == res_2 && res_1 == true);

        Ok(())
    }
    #[test]
    fn poison_should_unlock() {
        let lock = CompleteLock::new();

        lock.poison();
        let state = lock.wait_for_completion();
        assert_eq!(state, CompleteLockState::Poisoned);
    }
}
