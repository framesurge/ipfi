#[cfg(not(target_arch = "wasm32"))]
use std::sync::Condvar;
use std::sync::{Arc, Mutex};

/// A completion lock based on a direct `bool` mutation and spinlocks. This is an
/// inefficient implementation, but one that will work on embedded hardware with atomic
/// support and, importantly, in Wasm.
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub(crate) struct CompleteLock {
    lock: Arc<Mutex<bool>>,
}
#[cfg(target_arch = "wasm32")]
impl CompleteLock {
    pub(crate) fn new() -> Self {
        Self {
            lock: Arc::new(Mutex::new(false)),
        }
    }
    pub(crate) fn completed(&self) -> bool {
        *self.lock.lock().unwrap()
    }
    pub(crate) fn mark_complete(&self) {
        *self.lock.lock().unwrap() = true;
    }
    pub(crate) fn wait_for_completion(&self) {
        while !*self.lock.lock().unwrap() {
            std::hint::spin_loop()
        }
    }
}

/// A completion lock based on `Condvar`s, which are the most efficient mechanism for
/// this kind of lock on platforms that support them. This will be preferentially used
/// in almost all cases, with a notable exception for Wasm compilations.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub(crate) struct CompleteLock {
    pair: Arc<(Mutex<bool>, Condvar)>,
}
#[cfg(not(target_arch = "wasm32"))]
impl CompleteLock {
    pub(crate) fn new() -> Self {
        Self {
            pair: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }
    pub(crate) fn completed(&self) -> bool {
        let (lock, _) = &*self.pair;
        *lock.lock().unwrap()
    }
    pub(crate) fn mark_complete(&self) {
        let (lock, cvar) = &*self.pair;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }
    pub(crate) fn wait_for_completion(&self) {
        let (lock, cvar) = &*self.pair;
        let mut completed = lock.lock().unwrap();
        while !*completed {
            completed = cvar.wait(completed).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CompleteLock;

    /// The amount of time we should wait for threads to be updated by a `CompleteLock`, in milliseconds.
    static THREAD_WAITING_TIME: u64 = 400;

    #[test]
    fn should_unlock_waiter_no_threads() {
        // Very simple test case, but it's the best we can do in single-threaded Wasm
        let lock = CompleteLock::new();

        lock.mark_complete();
        lock.wait_for_completion();
        assert!(lock.completed());
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn should_unlock_multiple_waiters() -> Result<(), Box<dyn std::error::Error>> {
        let lock = CompleteLock::new();

        let lock_1 = lock.clone();
        let waiter_1 = std::thread::spawn(move || {
            lock_1.wait_for_completion();
            true
        });

        let lock_2 = lock.clone();
        let waiter_2 = std::thread::spawn(move || {
            lock_2.wait_for_completion();
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
}
