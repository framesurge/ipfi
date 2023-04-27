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
