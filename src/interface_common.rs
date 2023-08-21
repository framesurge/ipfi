//! This internal module contains code that is common between the blocking and asynchronous `Interface` types.

use crate::error::Error;
use crate::Terminator;

/// A procedure registered on an interface.
pub(crate) enum Procedure {
    /// A procedure that resolves immediately, returning all its output at once.
    Standard {
        /// The closure that will be called, which uses pure bytes as an interface. If there is a failure to
        /// deserialize the arguments or serialize the return type, an error will be returned.
        ///
        /// The bytes this takes for its arguments will *not* have a MessagePack length marker set at the front,
        /// and that will be added internally at deserialization.
        #[allow(clippy::type_complexity)]
        closure: Box<dyn Fn(&[u8]) -> Result<Vec<u8>, Error> + Send + Sync + 'static>,
    },
    /// A procedure that streams its output gradually.
    ///
    /// Note that this is used to also support asynchronous procedures that still return all their output
    /// at once, just after a delay.
    Streaming {
        /// The closure that will be called. This is expected to return immediately, and then handle the process
        /// of streaming values later as it wishes: it will be provided a closure that can be used to yield individual
        /// values. Note that the closure will almost certainly have to spawn a new thread or task to run itself.
        #[allow(clippy::type_complexity)]
        closure: Box<
            dyn Fn(
                    Box<dyn Fn(Vec<u8>, Terminator) + Send + Sync + 'static>,
                    &[u8],
                ) -> Result<(), Error>
                + Send
                + Sync
                + 'static,
        >,
    },
}

/// Defines the entire interface. The only difference between the blocking and async interfaces is that one uses
/// `async`/`await` because of the different `CompleteLock`, so we just set that up very simply!
#[macro_export]
#[doc(hidden)]
macro_rules! define_interface {
    ($($async: tt)? $(, $await: tt)?) => {
        use super::complete_lock::CompleteLock;
        use $crate::error::Error;
        #[cfg(feature = "serde")]
        use $crate::procedure_args::Tuple;
        use $crate::roi_queue::RoiQueue;
        use $crate::{CallIndex, Terminator, IpfiInteger, ProcedureIndex, WireId};
        use dashmap::DashMap;
        use fxhash::FxBuildHasher;
        use nohash_hasher::BuildNoHashHasher;
        #[cfg(feature = "serde")]
        use serde::{de::DeserializeOwned, Serialize};
        use $crate::interface_common::Procedure;

        /// An inter-process communication (IPC) interface based on the arbitrary reception of bytes.
        ///
        /// This is formed in a message-based interface, with messages identified by variable-size integers. Each time
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
            /// The messages received over the interface. This is implemented as a concurrent hash map indexed by a `IpfiInteger`,
            /// which means that, when a message is read, we can remove its entry from the map entirely and its identifier can
            /// be relinquished into a reuse-or-increment queue. The second entry in the value tuple is a chunk count, used for
            /// allowing length-aware deserialisation of streamed lists of discrete packets.
            messages: DashMap<IpfiInteger, (Vec<u8>, IpfiInteger, CompleteLock), BuildNoHashHasher<IpfiInteger>>,
            /// A queue that produces unique message identifiers while allowing us to recycle old ones. This enables far greater
            /// memory efficiency.
            message_id_queue: RoiQueue,
            /// A record of locks used to wait on the existence of a certain message
            /// index. Since multiple threads could simultaneously wait for different message
            /// indices, this has to be implemented this way!
            creation_locks: DashMap<IpfiInteger, CompleteLock, BuildNoHashHasher<IpfiInteger>>,
            /// A list of procedures registered on this interface that those communicating with this
            /// program may execute.
            procedures: DashMap<ProcedureIndex, Procedure, BuildNoHashHasher<IpfiInteger>>,
            /// A map of procedure call indices and the wire IDs that produced them to local message buffer addresses.
            /// These are necessary because, when a program sends a partial procedure call, and then later completes it,
            /// we need to know that the two messages are part of the same call. On their side, the caller will maintain
            /// a list of call indices, which we map here to local message buffers, allowing us to continue filling them
            /// as necessary.
            ///
            /// For clarity, this is a map of `(procedure_idx, call_idx, wire_id)` to `buf_idx`.
            call_to_buffer_map: DashMap<(ProcedureIndex, CallIndex, WireId), IpfiInteger, FxBuildHasher>,
            /// A queue that produces unique identifiers for the next wire that connects to this interface. Wire identifiers
            /// are needed to ensure that call indices, which may be the same across multiple wires, are kept separate from
            /// each other. This queue will automatically recirculate relinquished identifiers.
            wire_id_queue: RoiQueue,
        }
        impl Default for Interface {
            fn default() -> Self {
                Self {
                    messages: DashMap::default(),
                    message_id_queue: RoiQueue::new(),
                    creation_locks: DashMap::default(),
                    procedures: DashMap::default(),
                    call_to_buffer_map: DashMap::default(),
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
            #[cfg(feature = "serde")]
            pub fn add_procedure<
                A: Serialize + DeserializeOwned + Tuple,
                R: Serialize + DeserializeOwned,
            >(
                &self,
                idx: IpfiInteger,
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
                        rmp::encode::write_array_len(&mut bytes, A::len())
                            .map_err(|_| Error::WriteLenMarkerFailed)?;
                        bytes.extend(data);

                        rmp_serde::decode::from_slice(&bytes)?
                    };

                    let ret = f(arg_tuple);
                    let ret = rmp_serde::encode::to_vec(&ret)?;

                    Ok(ret)
                });
                let procedure = Procedure::Standard { closure };
                self.procedures.insert(ProcedureIndex(idx), procedure);
            }
            /// Adds a streaming procedure with serialisation handled automatically. This is designed for procedures that will yield
            /// something that can eventually be interpreted as a list of values, and, as such, each yield will be interpreted as
            /// a separate *chunk*. On the other side, the caller will be able to deserialise the response as a list of values. Hence,
            /// the yielder passed to such procedures only requires the caller to specify whether or not the yielded message is the final
            /// chunk. If you wish to send partial chunks, this should be done manually through `.add_raw_streaming_procedure()`.
            #[cfg(feature = "serde")]
            pub fn add_sequence_procedure<
                A: Serialize + DeserializeOwned + Tuple,
                R: Serialize + DeserializeOwned,
            >(
                &self,
                idx: IpfiInteger,
                f: impl Fn(Box<dyn Fn(R, bool) + Send + Sync + 'static>, A) + Send + Sync + 'static,
            ) {
                let closure = Box::new(move |raw_yielder: Box<dyn Fn(Vec<u8>, Terminator) + Send + Sync + 'static>, data: &[u8]| {
                    // Add the correct length prefix so this can be deserialized as the tuple it is (this is what allows
                    // piecemeal argument transmission)
                    let arg_tuple = if A::len() == 0 {
                        // If the function takes no arguments, the length marker hack doesn't work
                        rmp_serde::decode::from_slice(&rmp_serde::encode::to_vec(&())?)?
                    } else {
                        let mut bytes = Vec::new();
                        rmp::encode::write_array_len(&mut bytes, A::len())
                            .map_err(|_| Error::WriteLenMarkerFailed)?;
                        bytes.extend(data);

                        rmp_serde::decode::from_slice(&bytes)?
                    };
                    let yielder = Box::new(move |data: R, is_final: bool| {
                        let data = rmp_serde::encode::to_vec(&data).expect("TODO: error message system for failures like this");
                        let terminator = if is_final { Terminator::Complete } else { Terminator::Chunk };
                        raw_yielder(data, terminator);
                    });

                    f(yielder, arg_tuple);

                    Ok(())
                });
                let procedure = Procedure::Streaming { closure };
                self.procedures.insert(ProcedureIndex(idx), procedure);
            }
            /// Same as `.add_procedure()`, but this accepts procedures that work directly with raw bytes, involving no serialization or deserialization
            /// process. This method is only recommended when the `serde` feature cannot be used for whatever reason, as it requires you to carefully manage
            /// byte streams yourself, as there will be absolutely no validity checking of them by IPFI. Unlike `.add_procedure()`, this will accept any
            /// bytes and pass them straight to you, performing no intermediary steps.
            pub fn add_raw_procedure(
                &self,
                idx: IpfiInteger,
                f: impl Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
            ) {
                // We only need to wrap the result in `Ok(..)`
                let procedure = Procedure::Standard {
                    closure: Box::new(move |data: &[u8]| Ok(f(data))),
                };
                self.procedures.insert(ProcedureIndex(idx), procedure);
            }
            /// Same as `.add_procedure_raw()`, but this will add a *streaming* procedure, which, rather than returning its output all in
            /// one go, will yield chunks of it gradually, allowing it to continue operating in the background while other messages
            /// are processed. This can be used both for procedures that gather parts of their output gradually (e.g. those that yield
            /// real-time information) and for asynchronous procedures, which will return all their output at once, but after they
            /// have finished a computation in parallel.
            ///
            /// Note that streaming procedures should provide *all* their output through the given closure (which can be thought
            /// of as analogous to the `yield` keyword in languages with native support for generators). The second argument
            /// to this closure is whether or not your procedure is finished. After you have called `yielder(_, true)`, any
            /// subsequent calls will be propagated, but disregarded by a compliant IPFI implementation on the other side of the
            /// wire.
            ///
            /// You should be especially careful of the value of [`Terminator`] you set for each yield. Of course, the final yield
            /// should be [`Terminator::Complete`], however streaming procedures have the notion of *chunks*, which are discrete
            /// packets for later deserialisation. For instance, you could use a streaming procedure to stream parts of a single
            /// string, or you could stream independent strings. These have different serialised representations, and, as such, it
            /// is typical to want to maintain a count of how many chunks have been sent. By setting [`Terminator::Chunk`], you
            /// can explicitly mark such chunks. Note that this also means you can send partial chunks.
            ///
            /// **Warning:** this function is deliberately low-level, and performs *absolutely no thread management*. Your procedure
            /// should return as soon as possible, and should start a thread/task that then performs subsequent yields. If you'd like
            /// your procedure to automatically be executed in another thread/task, or if you'd like to return an actual `Stream`,
            /// then you may want to use another, higher-level method on [`Interface`]. Additionally, bear in mind that working with
            /// serialisation is somewhat harder when working with streams, as all data will be collated into the same buffer for
            /// deserialisation when received. This means procedures that are not accumulating some larger object, but rather a sequence
            /// of smaller objects, will need to yield a MessagePack array prefix as their first item. This is handled automatically
            /// by the higher-level method `.add_sequence_procedure()`.
            pub fn add_raw_streaming_procedure(
                &self,
                idx: IpfiInteger,
                f: impl Fn(Box<dyn Fn(Vec<u8>, Terminator) + Send + Sync + 'static>, &[u8]) + Send + Sync + 'static,
            ) {
                let procedure = Procedure::Streaming {
                    closure: Box::new(move |yielder, data: &[u8]| {
                        f(yielder, data);

                        Ok(())
                    }),
                };
                self.procedures.insert(ProcedureIndex(idx), procedure);
            }
            /// Deletes the given message buffer and relinquishes its unique identifier back into
            /// the queue for reuse. This must be provided an ID that was previously issued by `.push()`.
            fn pop(&self, id: IpfiInteger) -> Option<(Vec<u8>, IpfiInteger, CompleteLock)> {
                if let Some((_id, val)) = self.messages.remove(&id) {
                    self.message_id_queue.relinquish(id);
                    Some(val)
                } else {
                    None
                }
            }
            /// Calls the procedure with the given index, returning the raw serialized byte vector it produces. This will get its
            /// argument information from the given internal message buffer index, which it expects to be completed. This method
            /// will not block waiting for a completion (or creation) of that buffer.
            ///
            /// If this calls a streaming procedure, this will return `Ok(None)` on a success, as the return value will be sent
            /// through the given yielder function.
            ///
            /// There is no method provided to avoid byte serialization, because procedure calls over IPFI are intended to be made solely by
            /// remote communicators.
            pub $($async)? fn call_procedure(
                &self,
                procedure_idx: ProcedureIndex,
                call_idx: CallIndex,
                wire_id: WireId,
                yielder: impl Fn(Vec<u8>, Terminator) + Send + Sync + 'static,
            ) -> Result<Option<Vec<u8>>, Error> {
                // Get the buffer where the arguments are supposed to be, and then delete that mapping to free up some space
                let args_buf_idx = self.get_call_buffer(procedure_idx, call_idx, wire_id)$(.$await)?;
                self.call_to_buffer_map
                    .remove(&(procedure_idx, call_idx, wire_id));

                if let Some(procedure) = self.procedures.get(&procedure_idx) {
                    if let Some(m) = self.messages.get(&args_buf_idx) {
                        let (args, _chunk_count, complete_lock) = m.value();
                        if complete_lock.completed()$(.$await)? {
                            // The length marker will be inserted by the internal wrapper closure
                            let ret = match &*procedure {
                                Procedure::Standard { closure } => (closure)(args).map(|val| Some(val)),
                                Procedure::Streaming { closure } => {
                                    (closure)(Box::new(yielder), args)?;
                                    Ok(None)
                                }
                            };
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
            pub(crate) $($async)? fn get_call_buffer(
                &self,
                procedure_idx: ProcedureIndex,
                call_idx: CallIndex,
                wire_id: WireId,
            ) -> IpfiInteger {
                let buf_idx = if let Some(buf_idx) =
                    self.call_to_buffer_map
                        .get(&(procedure_idx, call_idx, wire_id))
                {
                    *buf_idx
                } else {
                    let new_idx = self.push()$(.$await)?;
                    self.call_to_buffer_map
                        .insert((procedure_idx, call_idx, wire_id), new_idx);
                    new_idx
                };

                buf_idx
            }
            /// Allocates space for a new message buffer, creating a new completion lock.
            /// This will also mark a relevant creation lock as completed if one exists.
            pub $($async)? fn push(&self) -> IpfiInteger {
                let new_id = self.message_id_queue.get();
                self.messages
                    .insert(new_id, (Vec::new(), 0, CompleteLock::new()));

                // If anyone was waiting for this buffer to exist, it now does!
                if let Some(lock) = self.creation_locks.get_mut(&new_id) {
                    lock.mark_complete()$(.$await)?;
                }

                new_id
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
            pub $($async)? fn send(&self, datum: i8, message_id: IpfiInteger) -> Result<bool, Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    let (message, _chunk_count, complete_lock) = m.value_mut();
                    if complete_lock.completed()$(.$await)? {
                        Err(Error::AlreadyCompleted { index: message_id })
                    } else if datum < 0 {
                        complete_lock.mark_complete()$(.$await)?;
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
            pub $($async)? fn send_many(&self, data: &[u8], message_id: IpfiInteger) -> Result<(), Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    let (message, _chunk_count, complete_lock) = m.value_mut();
                    if complete_lock.completed()$(.$await)? {
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
            /// has already been terminated or if it was out-of-bounds. This will *not* increment the number of chunks,
            /// which should be done separately (before calling this function!) if streaming deserialisation might be
            /// used. If not, the chunk count can be left alone.
            pub $($async)? fn terminate_message(&self, message_id: IpfiInteger) -> Result<(), Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    let (_message, _chunk_count, complete_lock) = m.value_mut();
                    if complete_lock.completed()$(.$await)? {
                        Err(Error::AlreadyCompleted { index: message_id })
                    } else {
                        complete_lock.mark_complete()$(.$await)?;
                        Ok(())
                    }
                } else {
                    Err(Error::OutOfBounds { index: message_id })
                }
            }
            /// Increment the number of chunks associated with the given message buffer. This will fail if the message
            /// has already been terminated (meaning you must update the chunk count before terminating a message,
            /// if you care about chunk counts).
            pub $($async)? fn increment_chunk_count(&self, message_id: IpfiInteger) -> Result<(), Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    let (_message, chunk_count, complete_lock) = m.value_mut();
                    if complete_lock.completed()$(.$await)? {
                        Err(Error::AlreadyCompleted { index: message_id })
                    } else {
                        *chunk_count += 1;
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
            #[cfg(feature = "serde")]
            pub $($async)? fn get<T: DeserializeOwned>(&self, message: IpfiInteger) -> Result<T, Error> {
                let message = self.get_raw(message)$(.$await)?;
                let decoded = rmp_serde::decode::from_slice(&message)?;
                Ok(decoded)
            }
            /// Same as `.get()`, except this is designed to be used for procedures that are known to stream their output
            /// in discrete chunks. There are two broad types of streaming procedures: those that stream bytes gradually that
            /// eventually become part of a greater whole (all deserialised at once), and those that stream discrete objects that
            /// should all be deserialised independently. This function is for the latter type.
            ///
            /// Note that, if used on any other type of message, this will still work, it will just produce a vector of length 1
            /// (as there was only one chunk involved).
            #[cfg(feature = "serde")]
            pub $($async)? fn get_chunks<T: DeserializeOwned>(&self, message: IpfiInteger) -> Result<Vec<T>, Error> {
                let (message, chunk_count) = self.get_raw_with_chunk_count(message)$(.$await)?;
                let mut bytes = Vec::new();
                // XXX: It is theoretically possible for this to fail if we pass `u32::MAX` chunks, somehow...
                rmp::encode::write_array_len(&mut bytes, chunk_count.into())
                            .map_err(|_| Error::WriteLenMarkerFailed)?;
                bytes.extend(message);
                let decoded = rmp_serde::decode::from_slice(&bytes)?;
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
            pub $($async)? fn get_raw(&self, message_id: IpfiInteger) -> Vec<u8> {
                let (message, _chunk_count) = self.get_raw_with_chunk_count(message_id)$(.$await)?;
                message
            }
            /// Same as `.get_raw()`, but this also includes the number of defined chunks this message came in. Note
            /// that the number of chunks is administered through a separate definition process to the end of a message.
            pub $($async)? fn get_raw_with_chunk_count(&self, message_id: IpfiInteger) -> (Vec<u8>, IpfiInteger) {
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

                        lock.wait_for_completion()$(.$await)?;
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
                    message.2.clone()
                };

                message_lock.wait_for_completion()$(.$await)?;

                // This definitely exists if we got here (and it logically has to).
                // Note that this will also prepare the message identifier for reuse.
                // This is safe to call because we only hold a clone of the completion
                // lock.
                let (message, chunk_count, _complete_lock) = self.pop(message_id).unwrap();
                (message, chunk_count)
            }
        }
    };
}

/// This can define *most* of the tests used for the interface, but not all of them! Those that involve
/// spawning separate threads are trickier.
#[macro_export]
#[doc(hidden)]
macro_rules! define_interface_tests {
    ($test_attr: path $(, $async: tt, $await: tt)?) => {
        fn has_duplicates<T: Eq + std::hash::Hash>(vec: &[T]) -> bool {
            let mut set = std::collections::HashSet::new();
            vec.iter().any(|x| !set.insert(x))
        }

        #[$test_attr]
        $($async)? fn wire_ids_should_be_unique() {
            let interface = Interface::new();
            let mut ids = Vec::new();
            // We only loop up to here for `int-u8`
            for _ in 0..255 {
                let id = interface.get_id();
                assert!(!ids.contains(&id));
                ids.push(id);
            }
        }
        #[$test_attr]
        $($async)? fn call_buffers_should_be_distinct() {
            let interface = Box::leak(Box::new(Interface::new()));
            let buf_1 = interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(0))$(.$await)?;
            let buf_2 = interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(1))$(.$await)?;
            let buf_3 = interface.get_call_buffer(ProcedureIndex(0), CallIndex(1), WireId(0))$(.$await)?;
            let buf_4 = interface.get_call_buffer(ProcedureIndex(1), CallIndex(0), WireId(0))$(.$await)?;

            assert!(!has_duplicates(&[buf_1, buf_2, buf_3, buf_4]));
        }
        #[$test_attr]
        $($async)? fn call_buffers_should_be_reused() {
            let interface = Box::leak(Box::new(Interface::new()));
            let buf_1 = interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(0))$(.$await)?;
            let buf_2 = interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(1))$(.$await)?;
            let buf_3 = interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(0))$(.$await)?;
            let buf_4 = interface.get_call_buffer(ProcedureIndex(0), CallIndex(0), WireId(1))$(.$await)?;

            assert!(has_duplicates(&[buf_1, buf_2, buf_3, buf_4]));
            assert!(buf_1 == buf_3 && buf_2 == buf_4);
        }
        #[$test_attr]
        $($async)? fn writing_to_msg_until_end_should_work() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;

            assert!(interface.send(0, id)$(.$await)?.is_ok());
            assert!(interface.send(1, id)$(.$await)?.is_ok());
            assert!(interface.send(2, id)$(.$await)?.is_ok());
            assert!(interface.send(-1, id)$(.$await)?.is_ok());

            assert!(interface.send(0, id)$(.$await)?.is_err());
            assert!(interface.send(-1, id)$(.$await)?.is_err());
        }
        #[$test_attr]
        $($async)? fn send_many_should_never_terminate() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;

            assert!(interface.send_many(&[0, 0, 1, 3, 2, 5, 12], id)$(.$await)?.is_ok());
            assert!(interface.send_many(&[0, 8, 1, 3], id)$(.$await)?.is_ok());
            assert!(interface.terminate_message(id)$(.$await)?.is_ok());

            assert!(interface.send_many(&[1, 4, 3], id)$(.$await)?.is_err());
        }
        #[$test_attr]
        $($async)? fn send_to_unreserved_buf_should_fail() {
            let interface = Interface::new();
            assert_eq!(interface.push()$(.$await)?, 0);
            assert_eq!(interface.push()$(.$await)?, 1);

            assert!(interface.send(0, 0)$(.$await)?.is_ok());
            assert!(interface.send(0, 1)$(.$await)?.is_ok());

            assert!(interface.send(0, 2)$(.$await)?.is_err());
        }
        #[$test_attr]
        $($async)? fn double_terminate_should_fail() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;
            interface.send(42, id)$(.$await)?.unwrap();
            assert!(interface.terminate_message(id)$(.$await)?.is_ok());
            assert!(interface.terminate_message(id)$(.$await)?.is_err());
        }
        #[$test_attr]
        $($async)? fn roi_buf_queue_should_work() {
            let interface = Interface::new();
            // We should start at 0
            assert_eq!(interface.push()$(.$await)?, 0);
            interface.send(42, 0)$(.$await)?.unwrap();
            interface.terminate_message(0)$(.$await)?.unwrap();
            // We haven't fetched, so this should increment
            assert_eq!(interface.push()$(.$await)?, 1);
            let msg = interface.get_raw(0)$(.$await)?;
            assert_eq!(msg, [42]);
            // But now we have fetched from the ID, so it should be reused
            assert_eq!(interface.push()$(.$await)?, 0);
        }
        #[$test_attr]
        #[cfg(feature = "serde")]
        $($async)? fn get_should_work() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;
            interface.send(42, id)$(.$await)?.unwrap();
            interface.terminate_message(id)$(.$await)?.unwrap();

            // This implicitly tests `.get_raw()` as well
            let msg = interface.get::<u8>(id)$(.$await)?;
            assert!(msg.is_ok());
            assert_eq!(msg.unwrap(), 42);
        }
        #[$test_attr]
        #[cfg(feature = "serde")]
        $($async)? fn get_with_wrong_type_should_fail() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;
            interface.send(42, id)$(.$await)?.unwrap();
            interface.terminate_message(id)$(.$await)?.unwrap();

            let msg = interface.get::<String>(id)$(.$await)?;
            assert!(msg.is_err());
        }
        #[$test_attr]
        $($async)? fn terminate_zero_sized_should_work() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;
            assert!(interface.terminate_message(id)$(.$await)?.is_ok());
            let msg = interface.get_raw(id)$(.$await)?;
            assert_eq!(msg, []);
        }
    };
}
