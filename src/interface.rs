use crate::{
    complete_lock::CompleteLock, error::Error, procedure_args::Tuple, roi_queue::RoiQueue,
};
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};

// The following are newtype wrappers that can deliberately only be constructed internally to avoid confusion.
// This also allows us to keep the internals as implementation details, and to change them without a breaking
// change if necessary.
/// A call index. This is its own type to avoid confusion.
///
/// For information about call indices, see [`Wire`].
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct CallIndex(pub(crate) usize);
/// A procedure index. This is its own type to avoid confusion.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct ProcedureIndex(pub(crate) usize);
// Users need to be able to construct these manually
impl ProcedureIndex {
    /// Creates a new procedure index as given.
    pub fn new(idx: usize) -> Self {
        Self(idx)
    }
}
/// An identifier for a wire.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct WireId(pub(crate) usize);

/// A procedure registered on an interface.
pub struct Procedure {
    /// The closure that will be called, which uses pure bytes as an interface. If there is a failure to
    /// deserialize the arguments or serialize the return type, an error will be returned.
    ///
    /// The bytes this takes for its arguments will *not* have a MessagePack length marker set at the front,
    /// and that will be added internally at deserialization.
    #[allow(clippy::type_complexity)]
    closure: Box<dyn Fn(&[u8]) -> Result<Vec<u8>, Error> + Send + Sync + 'static>,
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
    /// The messages received over the interface. This is implemented as a concurrent hash map indexed by a `usize`,
    /// which means that, when a message is read, we can remove its entry from the map entirely and its identifier can
    /// be relinquished into a reuse-or-increment queue.
    messages: DashMap<usize, (Vec<u8>, CompleteLock)>,
    /// A queue that produces unique message identifiers while allowing us to recycle old ones. This enables far greater
    /// memory efficiency.
    message_id_queue: RoiQueue,
    /// A record of locks used to wait on the existence of a certain message
    /// index. Since multiple threads could simultaneously wait for different message
    /// indices, this has to be implemented this way!
    creation_locks: DashMap<usize, CompleteLock>,
    /// A list of procedures registered on this interface that those communicating with this
    /// program may execute.
    procedures: DashMap<ProcedureIndex, Procedure>,
    /// A map of procedure call indices and the wire IDs that produced them to local message buffer addresses.
    /// These are necessary because, when a program sends a partial procedure call, and then later completes it,
    /// we need to know that the two messages are part of the same call. On their side, the caller will maintain
    /// a list of call indices, which we map here to local message buffers, allowing us to continue filling them
    /// as necessary.
    ///
    /// For clarity, this is a map of `(procedure_idx, call_idx, wire_id)` to `buf_idx`.
    call_to_buffer_map: DashMap<(ProcedureIndex, CallIndex, WireId), usize>,
    /// A queue that produces unique identifiers for the next wire that connects to this interface. Wire identifiers
    /// are needed to ensure that call indices, which h may be the same across multiple wires, are kept separate from
    /// each other. This queue will automatically recirculate relinquished identifiers.
    wire_id_queue: RoiQueue,
}
impl Default for Interface {
    fn default() -> Self {
        Self {
            messages: DashMap::new(),
            message_id_queue: RoiQueue::new(),
            creation_locks: DashMap::new(),
            procedures: DashMap::new(),
            call_to_buffer_map: DashMap::new(),
            wire_id_queue: RoiQueue::new(),
        }
    }
}
impl Interface {
    /// Initializes a new interface to be used for connecting to as many other programs as necessary through wires,
    /// or through manual communication management.
    pub fn new() -> Self {
        Self::default()
    }
    /// Gets an ID for a wire or other communication primitive that will depend on this interface. Any IPC primitive
    /// that will call procedures should acquire one of these for itself to make sure its procedure calls do not overlap
    /// with those of other wires. Typically, this will be called internally by the [`crate::Wire`] type.
    pub fn get_id(&self) -> WireId {
        WireId(self.wire_id_queue.get())
    }
    /// Marks the given wire identifier as relinquished. This will return it into the queue and recirculate it to the next
    /// new wire that requests an identifier. As such, the provided identifier *must not* be reused after it is provided to this
    /// call.
    ///
    /// Generally, this should be called within drop implementations, and it is not necessary to call this for the inbuilt [`Wire`].
    pub fn relinquish_id(&self, id: WireId) {
        self.wire_id_queue.relinquish(id.0);
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
    pub fn add_procedure<
        A: Serialize + DeserializeOwned + Tuple,
        R: Serialize + DeserializeOwned,
    >(
        &self,
        idx: usize,
        f: impl Fn(A) -> R + Send + Sync + 'static,
    ) {
        let closure = Box::new(move |data: &[u8]| {
            // Add the correct length prefix so this can be deserialized as the tuple it is (this is what allows
            // piecemeal argument transmission)
            let arg_tuple = if A::len() == 0 {
                // If the function takes no arguments, the length marker hack doesn't work
                rmp_serde::decode::from_slice(&rmp_serde::encode::to_vec(&())?)?
            } else {
                let mut bytes = Vec::new();
                rmp::encode::write_array_len(&mut bytes, A::len() as u32)
                    .map_err(|_| Error::WriteLenMarkerFailed)?;
                bytes.extend(data);

                rmp_serde::decode::from_slice(&bytes)?
            };

            let ret = f(arg_tuple);
            let ret = rmp_serde::encode::to_vec(&ret)?;

            Ok(ret)
        });
        let procedure = Procedure { closure };
        self.procedures.insert(ProcedureIndex(idx), procedure);
    }
    /// Calls the procedure with the given index, returning the raw serialized byte vector it produces. This will get its
    /// argument information from the given internal message buffer index, which it expects to be completed. This method
    /// will not block waiting for a completion (or creation) of that buffer.
    ///
    /// There is no method provided to avoid byte serialization, because procedure calls over IPFI are intended to be made solely by
    /// remote communicators.
    pub fn call_procedure(
        &self,
        procedure_idx: ProcedureIndex,
        call_idx: CallIndex,
        wire_id: WireId,
    ) -> Result<Vec<u8>, Error> {
        // Get the buffer where the arguments are supposed to be, and then delete that mapping to free up some space
        let args_buf_idx = self.get_call_buffer(procedure_idx, call_idx, wire_id);
        self.call_to_buffer_map
            .remove(&(procedure_idx, call_idx, wire_id));

        if let Some(procedure) = self.procedures.get(&procedure_idx) {
            if let Some(m) = self.messages.get(&args_buf_idx) {
                let (args, complete_lock) = m.value();
                if complete_lock.completed() {
                    // The length marker will be inserted by the internal wrapper closure
                    let ret = (procedure.closure)(args);
                    // Success, relinquish this message buffer
                    // To do that though, we have to drop anything referencing the map
                    drop(m);
                    self.pop(args_buf_idx);
                    ret
                } else {
                    Err(Error::CallBufferIncomplete {
                        index: args_buf_idx,
                    })
                }
            } else {
                Err(Error::NoCallBuffer {
                    index: args_buf_idx,
                })
            }
        } else {
            Err(Error::NoSuchProcedure {
                index: procedure_idx.0,
            })
        }
    }
    /// Gets the index of a local message buffer to be used to store the given call of the given procedure from the given wire.
    /// This allows arguments to be accumulated piecemeal before the actual procedure call.
    ///
    /// This will create a buffer for this call if one doesn't already exist, and otherwise it will create one.
    pub(crate) fn get_call_buffer(
        &self,
        procedure_idx: ProcedureIndex,
        call_idx: CallIndex,
        wire_id: WireId,
    ) -> usize {
        let buf_idx = if let Some(buf_idx) =
            self.call_to_buffer_map
                .get(&(procedure_idx, call_idx, wire_id))
        {
            *buf_idx
        } else {
            let new_idx = self.push();
            self.call_to_buffer_map
                .insert((procedure_idx, call_idx, wire_id), new_idx);
            new_idx
        };

        buf_idx
    }
    /// Allocates space for a new message buffer, creating a new completion lock.
    /// This will also mark a relevant creation lock as completed if one exists.
    pub fn push(&self) -> usize {
        let new_id = self.message_id_queue.get();
        self.messages
            .insert(new_id, (Vec::new(), CompleteLock::new()));

        // If anyone was waiting for this buffer to exist, it now does!
        if let Some(lock) = self.creation_locks.get_mut(&new_id) {
            lock.mark_complete();
        }

        new_id
    }
    /// Deletes the given message buffer and relinquishes its unique identifier back into
    /// the queue for reuse. This must be provided an ID that was previously issued by `.push()`.
    fn pop(&self, id: usize) -> Option<(Vec<u8>, CompleteLock)> {
        if let Some((_id, val)) = self.messages.remove(&id) {
            self.message_id_queue.relinquish(id);
            Some(val)
        } else {
            None
        }
    }
    /// Sends the given element through to the interface, adding it to the byte array of the message with
    /// the given 32-bit identifier. If `-1` is provided, the message will be marked as completed. This will
    /// return an error if the message was already completed, or out-of-bounds, or otherwise `Ok(true)`
    /// if the message buffer is still open, or `Ok(false)` if this call caused it to complete.
    ///
    /// If the message identifier provided does not exist (which may be because the message had already been
    /// completed and read), then
    ///
    /// # Errors
    ///
    /// This will fail if the message with the given identifier had already been completed, or it did not exist.
    /// Either of these cases can be trivially caused by a malicious client, and the caller should therefore be
    /// careful in how it handles these errors.
    pub fn send(&self, datum: i8, message_id: usize) -> Result<bool, Error> {
        if let Some(mut m) = self.messages.get_mut(&message_id) {
            let (message, complete_lock) = m.value_mut();
            if complete_lock.completed() {
                Err(Error::AlreadyCompleted { index: message_id })
            } else if datum < 0 {
                complete_lock.mark_complete();
                Ok(false)
            } else {
                message.push(datum as u8);
                Ok(true)
            }
        } else {
            Err(Error::OutOfBounds { index: message_id })
        }
    }
    /// Sends many bytes through to the interface. When you have many bytes instead of just one at a time, this
    /// method should be preferred. Note that this method will not allow the termination of a message, and that should
    /// be handled separately.
    pub fn send_many(&self, data: &[u8], message_id: usize) -> Result<(), Error> {
        if let Some(mut m) = self.messages.get_mut(&message_id) {
            let (message, complete_lock) = m.value_mut();
            if complete_lock.completed() {
                Err(Error::AlreadyCompleted { index: message_id })
            } else {
                message.extend(data);
                Ok(())
            }
        } else {
            Err(Error::OutOfBounds { index: message_id })
        }
    }
    /// Explicitly terminates the message with the given index. This will return an error if the message
    /// has already been terminated or if it was out-of-bounds.
    pub fn terminate_message(&self, message_id: usize) -> Result<(), Error> {
        if let Some(mut m) = self.messages.get_mut(&message_id) {
            let (_message, complete_lock) = m.value_mut();
            if complete_lock.completed() {
                Err(Error::AlreadyCompleted { index: message_id })
            } else {
                complete_lock.mark_complete();
                Ok(())
            }
        } else {
            Err(Error::OutOfBounds { index: message_id })
        }
    }
    /// Gets an object of the given type from the given message buffer index of the interface. This will block
    /// waiting for the given message buffer to be (1) created and (2) marked as complete. Depending on the
    /// caller's behaviour, this may block forever if they never complete the message.
    ///
    /// Note that this method will extract the underlying message from the given buffer index, leaving it
    /// available for future messages or procedure call metadata. This means that requesting the same message
    /// index may yield completely different data.
    pub fn get<T: DeserializeOwned>(&self, message: usize) -> Result<T, Error> {
        let message = self.get_raw(message);
        let decoded = rmp_serde::decode::from_slice(&message)?;
        Ok(decoded)
    }
    /// Same as `.get()`, but gets the raw byte array instead. Note that this will copy the underlying bytes to return
    /// them outside the thread-lock.
    ///
    /// This will block until the message with the given identifier has been created and completed.
    ///
    /// Note that this method will extract the underlying message from the given buffer index, leaving it
    /// available for future messages or procedure call metadata. This means that requesting the same message
    /// index may yield completely different data.
    pub fn get_raw(&self, message_id: usize) -> Vec<u8> {
        // We need two completion locks to be ready before we're ready: the first is
        // a creation lock on the message index, and the second is a lock on its
        // actual completion. We'll start by creating a new completion lock if one
        // doesn't already exist. Any future creations will thereby trigger it. Of
        // course, we won't need to do this if the entry already exists.
        let message_lock = {
            let message = if let Some(message) = self.messages.get(&message_id) {
                message
            } else {
                let lock = if let Some(lock) = self.creation_locks.get(&message_id) {
                    lock.clone()
                } else {
                    // This is the only time at which we create a new creation lock
                    let lock = CompleteLock::new();
                    self.creation_locks.insert(message_id, lock.clone());
                    lock
                };

                lock.wait_for_completion();
                // Now delete that lock to free up some space
                // Note that our read-only creation locks instance will definitely have been dropped
                // by this point
                self.creation_locks.remove(&message_id);

                // We can guarantee that this will exist now
                self.messages
                    .get(&message_id)
                    .expect("complete lock should signal that message exists")
            };
            // Cheap clone
            message.1.clone()
        };

        message_lock.wait_for_completion();

        // This definitely exists if we got here (and it logically has to).
        // Note that this will also prepare the message identifier for reuse.
        // This is safe to call because we only hold a clone of the completion
        // lock.
        let (message, _complete_lock) = self.pop(message_id).unwrap();
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_duplicates<T: Eq + std::hash::Hash>(vec: &[T]) -> bool {
        let mut set = std::collections::HashSet::new();
        vec.iter().any(|x| !set.insert(x))
    }

    #[test]
    fn wire_ids_should_be_unique() {
        let interface = Interface::new();
        let mut ids = Vec::new();
        for _ in 0..1000 {
            let id = interface.get_id();
            assert!(!ids.contains(&id));
            ids.push(id);
        }
    }
    #[test]
    fn call_buffers_should_be_distinct() {
        let interface = Box::leak(Box::new(Interface::new()));
        let buf_1 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(0))
        });
        let buf_2 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(1))
        });
        let buf_3 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(0), CallIndex(1), WireId(0))
        });
        let buf_4 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(1), CallIndex(0), WireId(0))
        });

        assert!(!has_duplicates(&[
            buf_1.join().unwrap(),
            buf_2.join().unwrap(),
            buf_3.join().unwrap(),
            buf_4.join().unwrap(),
        ]));
    }
    #[test]
    fn call_buffers_should_be_reused() {
        let interface = Box::leak(Box::new(Interface::new()));
        let buf_1 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(0))
        });
        let buf_2 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(1))
        });
        let buf_3 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(0))
        });
        let buf_4 = std::thread::spawn(|| {
            interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(1))
        });

        let buf_1 = buf_1.join().unwrap();
        let buf_2 = buf_2.join().unwrap();
        let buf_3 = buf_3.join().unwrap();
        let buf_4 = buf_4.join().unwrap();

        assert!(has_duplicates(&[buf_1, buf_2, buf_3, buf_4]));
        assert!(buf_1 == buf_3 && buf_2 == buf_4);
    }
    #[test]
    fn message_buf_allocs_should_not_overlap() {
        let interface = Box::leak(Box::new(Interface::new()));
        let buf_1 = std::thread::spawn(|| interface.push());
        let buf_2 = std::thread::spawn(|| interface.push());
        let buf_3 = std::thread::spawn(|| interface.push());
        let buf_4 = std::thread::spawn(|| interface.push());

        let buf_1 = buf_1.join().unwrap();
        let buf_2 = buf_2.join().unwrap();
        let buf_3 = buf_3.join().unwrap();
        let buf_4 = buf_4.join().unwrap();

        assert!(!has_duplicates(&[buf_1, buf_2, buf_3, buf_4]));
    }
    #[test]
    fn writing_to_msg_until_end_should_work() {
        let interface = Interface::new();
        assert!(interface.send(0, 0).is_ok());
        assert!(interface.send(1, 0).is_ok());
        assert!(interface.send(2, 0).is_ok());
        assert!(interface.send(-1, 0).is_ok());

        assert!(interface.send(0, 0).is_err());
        assert!(interface.send(-1, 0).is_err());
    }
    #[test]
    fn send_many_should_never_terminate() {
        let interface = Interface::new();
        assert!(interface.send_many(&[0, 0, 1, 3, 2, 5, 12], 0).is_ok());
        assert!(interface.send_many(&[0, 8, 1, 3], 0).is_ok());
        assert!(interface.terminate_message(0).is_ok());

        assert!(interface.send_many(&[1, 4, 3], 0).is_err());
    }
    #[test]
    fn send_to_late_buf_should_fail() {
        let interface = Interface::new();
        // No buffers allocated, but 0 should work (next-in-line)
        assert!(interface.send(0, 0).is_ok());
        // Now 1 is next
        assert!(interface.send(0, 1).is_ok());
        // Now 2 is next, so 3 shouldn't work
        assert!(interface.send(0, 3).is_err());
    }
    #[test]
    fn get_should_work() {
        let interface = Interface::new();
        interface.send(42, 0).unwrap();
        interface.terminate_message(0).unwrap();

        // This implicitly tests `.get_raw()` as well
        let msg = interface.get::<u8>(0);
        assert!(msg.is_ok());
        assert_eq!(msg.unwrap(), 42);
    }
    #[test]
    fn get_with_wrong_type_should_fail() {
        let interface = Interface::new();
        interface.send(42, 0).unwrap();
        interface.terminate_message(0).unwrap();

        let msg = interface.get::<String>(0);
        assert!(msg.is_err());
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_get_should_resolve() {
        let interface = Box::leak(Box::new(Interface::new()));

        let msg = std::thread::spawn(|| {
            // Note that this message does not even exist yet
            interface.get::<String>(1)
        });
        assert!(!msg.is_finished());
        // We can terminate before creation (zero-sized)
        interface.terminate_message(0).unwrap();
        interface
            .send_many(&rmp_serde::encode::to_vec("Hello, world!").unwrap(), 1)
            .unwrap();
        // We haven't terminated it yet
        assert!(!msg.is_finished());
        interface.terminate_message(1).unwrap();

        let msg = msg.join().unwrap().unwrap();
        assert_eq!(msg, "Hello, world!");
    }
}
