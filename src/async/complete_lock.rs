use tokio::sync::{Mutex, Notify};
use std::sync::Arc;

/// A completion lock based on `Condvar`s, which are the most efficient mechanism for
/// this kind of lock on platforms that support them.
// NOTE: This is now supported on Wasm!
#[derive(Clone)]
pub(crate) struct CompleteLock {
    pair: Arc<(Mutex<bool>, Notify)>,
}
impl CompleteLock {
    pub(crate) fn new() -> Self {
        Self {
            pair: Arc::new((Mutex::new(false), Notify::new())),
        }
    }
    pub(crate) async fn completed(&self) -> bool {
        let (lock, _) = &*self.pair;
        *lock.lock().await
    }
    pub(crate) async fn mark_complete(&self) {
        let (lock, notifier) = &*self.pair;
        *lock.lock().await = true;
        notifier.notify_waiters();
    }
    pub(crate) async fn wait_for_completion(&self) {
        let (lock, notifier) = &*self.pair;
        // Perform an outright check first
        let completed = lock.lock().await;
        if *completed { return }
        drop(completed);

        loop {
            // Wait to be notified about completion
            notifier.notified().await;
            // And check the guard, otherwise waiting again
            let completed = lock.lock().await;
            if *completed { break }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CompleteLock;

    #[tokio::test]
    async fn should_unlock_waiter_no_threads() {
        // Very simple test case, but it's the best we can do in single-threaded Wasm
        let lock = CompleteLock::new();

        lock.mark_complete().await;
        lock.wait_for_completion().await;
        assert!(lock.completed().await);
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn should_unlock_multiple_waiters() -> Result<(), Box<dyn std::error::Error>> {
        let lock = CompleteLock::new();

        let lock_1 = lock.clone();
        let waiter_1 = tokio::task::spawn(async move {
            lock_1.wait_for_completion().await;
            true
        });

        let lock_2 = lock.clone();
        let waiter_2 = tokio::task::spawn(async move {
            lock_2.wait_for_completion().await;
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
}
