use crate::CompleteLockState;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// A completion lock based on `Condvar`s, which are the most efficient mechanism for
/// this kind of lock on platforms that support them.
// NOTE: This is now supported on Wasm!
#[derive(Clone)]
pub(crate) struct CompleteLock {
    pair: Arc<(Mutex<CompleteLockState>, Notify)>,
}
impl CompleteLock {
    /// Creates a new, incomplete, lock.
    pub(crate) fn new() -> Self {
        Self {
            pair: Arc::new((Mutex::new(CompleteLockState::Incomplete), Notify::new())),
        }
    }
    /// Gets the current state of the lock.
    pub(crate) async fn state(&self) -> CompleteLockState {
        let (lock, _) = &*self.pair;
        *lock.lock().await
    }
    /// Marks the lock as complete. If the lock was previously poisoned, this will override that.
    pub(crate) async fn mark_complete(&self) {
        let (lock, cvar) = &*self.pair;
        *lock.lock().await = CompleteLockState::Complete;
        cvar.notify_waiters();
    }
    /// Poisons the lock, signalling to any waiters that what it guards is now in an invalid
    /// state.
    pub(crate) async fn poison(&self) {
        let (lock, cvar) = &*self.pair;
        let mut state = lock.lock().await;
        // We can only poison incomplete locks
        if *state == CompleteLockState::Incomplete {
            *state = CompleteLockState::Poisoned;
            cvar.notify_waiters();
        }
    }
    /// Waits until the lock is completed, returning its state. This will also return
    /// if/when the lock is explicitly poisoned.
    #[must_use] // Must check for poisoning!
    pub(crate) async fn wait_for_completion(&self) -> CompleteLockState {
        let (lock, notifier) = &*self.pair;
        // Perform an outright check first
        let state = lock.lock().await;
        if *state != CompleteLockState::Incomplete {
            return *state;
        }
        drop(state);

        loop {
            // Wait to be notified about completion
            notifier.notified().await;
            // And check the guard, otherwise waiting again
            let state = lock.lock().await;
            if *state != CompleteLockState::Incomplete {
                return *state;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CompleteLock;
    use crate::CompleteLockState;

    #[tokio::test]
    async fn should_unlock_waiter_no_threads() {
        // Very simple test case, but it's the best we can do in single-threaded Wasm
        let lock = CompleteLock::new();

        lock.mark_complete().await;
        let state = lock.wait_for_completion().await;
        assert_eq!(state, CompleteLockState::Complete);
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn should_unlock_multiple_waiters() -> Result<(), Box<dyn std::error::Error>> {
        let lock = CompleteLock::new();

        let lock_1 = lock.clone();
        let waiter_1 = tokio::task::spawn(async move {
            let _ = lock_1.wait_for_completion().await;
            true
        });

        let lock_2 = lock.clone();
        let waiter_2 = tokio::task::spawn(async move {
            let _ = lock_2.wait_for_completion().await;
            true
        });

        assert!(!waiter_1.is_finished());
        assert!(!waiter_2.is_finished());

        lock.mark_complete().await;

        let res_1 = waiter_1.await.expect("couldn't wait for waiter 1");
        let res_2 = waiter_2.await.expect("couldn't wait for waiter 2");

        assert!(res_1 == res_2 && res_1 == true);

        Ok(())
    }
    #[tokio::test]
    async fn poison_should_unlock() {
        let lock = CompleteLock::new();

        lock.poison().await;
        let state = lock.wait_for_completion().await;
        assert_eq!(state, CompleteLockState::Poisoned);
    }
}
