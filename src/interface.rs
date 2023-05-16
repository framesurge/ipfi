use crate::{complete_lock::CompleteLock, error::Error};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

/// A procedure registered on an interface.
pub struct Procedure {
    /// The closure that will be called, which uses pure bytes as an interface. If there is a failure to
    /// deserialize the arguments or serialize the return type, an error will be returned.
    closure: Box<dyn Fn(Vec<u8>) -> Result<Vec<u8>, Error> + Send + Sync + 'static>,
}

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
    /// A list of procedures registered on this interface that those communicating with this
    /// program may execute.
    procedures: Arc<RwLock<HashMap<usize, Procedure>>>,
}
impl Interface {
    /// Initializes a new interface with some other host module.
    pub fn new() -> Self {
        Self {
            messages: Arc::new(RwLock::new(Vec::new())),
            creation_locks: Arc::new(RwLock::new(HashMap::new())),
            procedures: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    /// Adds the given function as a procedure on this interface, which will allow other programs interacting with
    /// this one to execute it. Critically, no validation of intent or origin is requires to remotely execute procedures
    /// over IPFI, so you must be certain of two things when using this method:
    ///
    /// 1. You **never** expose an IPFI interface to an untrusted program, or to a program that executes untrusted user code.
    /// 2. You are happy for the functions you register using this method to be accessible by any programs that you allow to
    /// interface with your code.
    ///
    /// You must also ensure that any invariants you expect from the arguments provided to your procedure are checked *by your procedure*,
    /// since arguments will be deserialized from raw bytes, and, although this process will catch structurally invalid input, it
    /// will not catch logically invalid input. As an example, of you have a function that adds two positive numbers, and you expect
    /// both given arguments to be positive, something you previously upheld at the caller level, you must no uphold that invariant
    /// within the procedure itself, otherwise any program could use IPFI to pass invalid integers. Alternately, you can use a custom
    /// serialization/deserialization process to uphold invariants, provided you are satisfied that is secure against totally untrusted
    /// input.
    pub fn add_procedure<A: Serialize + DeserializeOwned, R: Serialize + DeserializeOwned>(&self, idx: usize, f: impl Fn(A) -> R + Send + Sync + 'static) {
        let closure = Box::new(move |bytes: Vec<u8>| {
            let arg_tuple = rmp_serde::decode::from_slice(&bytes)?;
            let ret = f(arg_tuple);
            let ret = rmp_serde::encode::to_vec(&ret)?;

            Ok(ret)
        });
        let procedure = Procedure {
            closure,
        };
        self.procedures.write().unwrap().insert(idx, procedure);
    }
    /// Calls the procedure with the given index, returning the raw serialized byte vector it produces, or `None` if a procedure with
    /// the given index has not been registered. This takes ownership of the byte arguments provided to the function, for internal
    /// lifetime reasons.
    ///
    /// There is no method provided to avoid byte serialization, because procedure calls over IPFI are intended to be made solely by
    /// remote communicators.
    pub fn call_procedure(&self, idx: usize, args: Vec<u8>) -> Option<Result<Vec<u8>, Error>> {
        if let Some(procedure) = self.procedures.read().unwrap().get(&idx) {
            Some((procedure.closure)(args))
        } else {
            None
        }
    }
    /// Allocates space for a new message buffer, creating a new completion lock.
    /// This will also mark a relevant creation lock as completed if one exists.
    ///
    /// The caller must ensure that both the messages and creation locks are accessible
    /// when this function is called, or a deadlock will ensue.
    pub fn push(&self) -> usize {
        let mut messages = self.messages.write().unwrap();
        let new_idx = messages.len();
        messages.push((Vec::new(), CompleteLock::new()));

        // If anyone was waiting for this buffer to exist, it now does!
        let mut creation_locks = self.creation_locks.write().unwrap();
        if let Some(lock) = creation_locks.get_mut(&new_idx) {
            lock.mark_complete();
        }

        new_idx
    }
    /// Sends the given element through to the interface, adding it to the byte array of the message with
    /// the given 32-bit identifier. If `-1` is provided, the message will be marked as completed. This will
    /// return an error if the message was already completed, or out-of-bounds, or otherwise `Ok(true)`
    /// if the message buffer is still open, or `Ok(false)` if this call caused it to complete.
    ///
    /// Ths reason this fails gracefully for an out-of-bounds message is so other programs, which may not have
    /// been involved in deciding how many message buffers should be in this interface, can make mistakes without
    /// blowing up the program they're sending data. Callers shoudl be careful, however, to inform the sender that
    /// they are sending data to the void.
    pub fn send(&self, datum: i8, message_idx: usize) -> Result<bool, Error> {
        let mut messages = self.messages.write().unwrap();
        let message = if messages.len() > message_idx {
            // Already exists (likely partial)
            messages.get_mut(message_idx).unwrap()
        } else if messages.len() == message_idx {
            // Next in line, we need to allocate a new entry
            drop(messages); // BUG Race condition here??
            self.push();
            messages = self.messages.write().unwrap();
            messages.get_mut(message_idx).unwrap()
        } else {
            // Need more messages before this one, dump it
            return Err(Error::OutOfBounds { index: message_idx, max_idx: messages.len() - 1 });
        };

        if message.1.completed() {
            Err(Error::AlreadyCompleted { index: message_idx })
        } else if datum < 0 {
            message.1.mark_complete();
            Ok(false)
        } else {
            message.0.push(datum as u8);
            Ok(true)
        }
    }
    /// Sends many bytes through to the interface. When you have many bytes instead of just one at a time, this
    /// method should be preferred. Note that this method will not allow the termination of a message, and that should
    /// be handled separately.
    pub fn send_many(&self, data: &[u8], message_idx: usize) -> Result<(), Error> {
        let mut messages = self.messages.write().unwrap();
        let message = if messages.len() > message_idx {
            // Already exists (likely partial)
            messages.get_mut(message_idx).unwrap()
        } else if messages.len() == message_idx {
            // Next in line, we need to allocate a new entry
            drop(messages);
            self.push();
            messages = self.messages.write().unwrap();
            messages.get_mut(message_idx).unwrap()
        } else {
            // Need more messages before this one, dump it
            return Err(Error::OutOfBounds { index: message_idx, max_idx: messages.len() - 1 });
        };

        if message.1.completed() {
            Err(Error::AlreadyCompleted { index: message_idx })
        } else {
            message.0.extend(data);
            Ok(())
        }
    }
    /// Explicitly terminates the message with the given index. This will return an error if the message
    /// has already been terminated or if it was out-of-bounds.
    pub fn terminate_message(&self, message_idx: usize) -> Result<(), Error> {
        let mut messages = self.messages.write().unwrap();
        let message = if messages.len() > message_idx {
            // Already exists (likely partial)
            messages.get_mut(message_idx).unwrap()
        } else if messages.len() == message_idx {
            // Next in line, we need to allocate a new entry
            // This is possible for a termination operation if the message is zero-sized
            drop(messages);
            self.push();
            messages = self.messages.write().unwrap();
            messages.get_mut(message_idx).unwrap()
        } else {
            // Need more messages before this one, dump it
            return Err(Error::OutOfBounds { index: message_idx, max_idx: messages.len() - 1 });
        };

        if message.1.completed() {
            Err(Error::AlreadyCompleted { index: message_idx })
        } else {
            message.1.mark_complete();
            Ok(())
        }
    }
    // /// Gets an object of the given type from the module interface, specifically from the *next*
    // /// unread message. To determine what the latest unread message is, a counter should
    // /// be provided to this method, which will be incremented as necessary. This allows
    // /// the caller to manage whether the read queue should be thread-local or shared, since
    // /// both approaches have system-specific benefits.
    // ///
    // /// **IMPORTANT:** If this returns an error, the given counter will **not** have been incremented!
    // /// It will only be incremented if the message was successfully read and deserialized.
    // pub fn get_next<T: DeserializeOwned>(&self, counter: &mut usize) -> Result<T, rmp_serde::decode::Error> {
    //     let message = self.get(*counter);
    //     // Only increment if message deserialization didn't fail
    //     if message.is_ok() {
    //         *counter += 1;
    //     }
    //     message
    // }
    /// Gets an object of the given type from the given message buffer index of the interface. This will block
    /// waiting for the given message buffer to be (1) created and (2) marked as complete. Depending on the
    /// caller's behaviour, this may block forever if they never complete the message.
    pub fn get<T: DeserializeOwned>(&self, message: usize) -> Result<T, Error> {
        let message = self.get_raw(message);
        let decoded = rmp_serde::decode::from_slice(&message)?;
        Ok(decoded)
    }
    // /// Gets the next message as a raw byte array. See `.get_next()` for the semantics
    // /// of getting the next message in any form.
    // pub fn get_next_raw(&self, counter: &mut usize) -> Vec<u8> {
    //     let message = self.get_raw(*counter);
    //     // We do this after to avoid any panics leading to a broken counter state
    //     *counter += 1;
    //     message
    // }
    /// Same as `.get()`, but gets the raw byte array instead. Note that this will copy the underlying bytes to return
    /// them outside the thread-lock.
    ///
    /// This will block until the message with the given index has been created and completed.
    fn get_raw(&self, message_idx: usize) -> Vec<u8> {
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
