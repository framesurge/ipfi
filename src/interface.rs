use crate::complete_lock::CompleteLock;
use serde::de::DeserializeOwned;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

/// An inter-process communication (IPC) interface based on the arbitrary reception of bytes.
///
/// This is formed in a message-based interface, with messages identified by 32-bit integers. Each time
/// a new value is sent into the interface, it will be added to the provided message, allowing data from
/// multiple sources to be simultaneously accumulated. Once `-1` is sent, the message will be marked as
/// completed, and future attempts to write to it will fail. Thread-safety and mutability locks are
/// maintained independently for every message we store.
///
/// Note that this system is perfectly thread-safe, and it is perfectly valid for multiple data sources
/// to send multiple messages simultaneously, while multiple receivers simultaneously wait for multiple
/// messages. Once a message is accumulated, it will be stored in the buffer until the interface is
/// dropped.
pub struct Interface {
    /// The messages received over the interface.
    messages: Arc<RwLock<Vec<(Vec<u8>, CompleteLock)>>>,
    /// A record of locks used to wait on the existence of a certain message
    /// index. Since multiple threads could simultaneously wait for different message
    /// indices, this has to be implemented this way!
    creation_locks: Arc<RwLock<HashMap<usize, CompleteLock>>>,
}
impl Interface {
    /// Initializes a new interface with some other host module.
    pub fn new() -> Self {
        Self {
            messages: Arc::new(RwLock::new(Vec::new())),
            creation_locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    /// Allocates space for a new message buffer, creating a new completion lock.
    /// This will also mark a relevant completion lock as completed if one exists.
    ///
    /// The caller must ensure that both the messages and creation locks are accessible
    /// when this function is called, or a deadlock will ensue.
    fn push(&self) {
        let mut messages = self.messages.write().unwrap();
        let new_idx = messages.len();
        messages.push((Vec::new(), CompleteLock::new()));

        let mut creation_locks = self.creation_locks.write().unwrap();
        if let Some(lock) = creation_locks.get_mut(&new_idx) {
            lock.mark_complete();
        }
    }
    /// Sends the given element through to the interface, adding it to the byte array of the message with
    /// the given 32-bit identifier. If `-1` is provided, the message will be marked as completed. If the
    /// message was already marked as completed, no matter what the input, this will return `false`, indicating
    /// a failure. Likewise, if the given message is out-of-bounds for this interface, `false` will be
    /// returned.
    ///
    /// Ths reason this fails gracefully for an out-of-bounds message is so other programs, which may not have
    /// been involved in deciding how many message buffers should be in this interface, can make mistakes without
    /// blowing up the program they're sending data. Callers shoudl be careful, however, to inform the sender that
    /// they are sending data to the void.
    pub fn send(&self, datum: i8, message: usize) -> bool {
        let mut messages = self.messages.write().unwrap();
        let message = if messages.len() > message {
            // Already exists (likely partial)
            messages.get_mut(message).unwrap()
        } else if messages.len() == message {
            // Next in line, we need to allocate a new entry
            drop(messages);
            self.push();
            messages = self.messages.write().unwrap();
            messages.get_mut(message).unwrap()
        } else {
            // Need more messages before this one, dump it
            return false;
        };

        if message.1.completed() {
            false
        } else if datum < 0 {
            message.1.mark_complete();
            true
        } else {
            message.0.push(datum as u8);
            true
        }
    }
    /// Sends many bytes through to the interface. When you have many bytes instead of just one at a time, this
    /// method should be preferred. Note that this method will not allow the termination of a message, and that should
    /// be handled separately.
    ///
    /// This will return `false` if the given message index was out-of-bounds, or if it has already been terminated.
    pub fn send_many(&self, data: &[u8], message: usize) -> bool {
        let mut messages = self.messages.write().unwrap();
        let message = if messages.len() > message {
            // Already exists (likely partial)
            messages.get_mut(message).unwrap()
        } else if messages.len() == message {
            // Next in line, we need to allocate a new entry
            drop(messages);
            self.push();
            messages = self.messages.write().unwrap();
            messages.get_mut(message).unwrap()
        } else {
            // Need more messages before this one, dump it
            return false;
        };

        if message.1.completed() {
            false
        } else {
            message.0.extend(data);
            true
        }
    }
    /// Explicitly terminates the message with the given index. This will return `true` if it was possible, or `false`
    /// if the message had already been terminated (or if it is out-of-bounds).
    pub fn terminate_message(&self, message: usize) -> bool {
        let mut messages = self.messages.write().unwrap();
        let message = if messages.len() > message {
            // Already exists (likely partial)
            messages.get_mut(message).unwrap()
        } else if messages.len() == message {
            // Next in line, we need to allocate a new entry
            // This is possible for a termination operation if the message is zero-sized
            drop(messages);
            self.push();
            messages = self.messages.write().unwrap();
            messages.get_mut(message).unwrap()
        } else {
            // Need more messages before this one, dump it
            return false;
        };

        if message.1.completed() {
            false
        } else {
            message.1.mark_complete();
            true
        }
    }
    /// Gets an object of the given type from the module interface. This will get from the earliest completed message,
    /// blocking if there are only incomplete messages available until one is completed. Depending on the caller's behavior,
    /// this may block forever if they do not terminate a message.
    pub fn get<T: DeserializeOwned>(&self, message: usize) -> Result<T, rmp_serde::decode::Error> {
        let message = self.get_raw(message);
        rmp_serde::decode::from_slice(&message)
    }
    /// Same as `.get()`, but gets the raw byte array instead. Note that this will copy the underlying bytes to return
    /// them outside the thread-lock.
    ///
    /// # Panics
    ///
    /// This will panic if the given message index is out-of-bounds for this interface. Since the caller instantiates
    /// this interface with its message size, they should check this before calling this method.
    pub fn get_raw(&self, message_idx: usize) -> Vec<u8> {
        // We need two completion locks to be ready before we're ready: the first is
        // a creation lock on the message index, and the second is a lock on its
        // actual completion. We'll start by creating a new completion lock if one
        // doesn't already exist. Any future creations will thereby trigger it. Of
        // course, we won't need to do this if the entry already exists.
        let message_lock = {
            // Mutable for reassignment
            let mut messages = self.messages.read().unwrap();
            let message = if let Some(message) = messages.get(message_idx) {
                message
            } else {
                drop(messages);
                // Mutable for reassignment
                let creation_locks = self.creation_locks.read().unwrap();
                let lock = if let Some(lock) = creation_locks.get(&message_idx) {
                    let lock = lock.clone();
                    drop(creation_locks);
                    lock
                } else {
                    let lock = CompleteLock::new();
                    // Upgrade to a writeable lock
                    drop(creation_locks);
                    let mut creation_locks_w = self.creation_locks.write().unwrap();
                    creation_locks_w.insert(message_idx, lock.clone());
                    lock
                };

                lock.wait_for_completion();
                messages = self.messages.read().unwrap();
                messages.get(message_idx).unwrap()
            };
            message.1.clone()
        };

        message_lock.wait_for_completion();

        let messages = self.messages.read().unwrap();
        messages[message_idx].0.to_vec()
    }
}
