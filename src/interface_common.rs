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
                    Box<dyn Fn(Vec<u8>, Terminator) -> Result<(), Error> + Send + Sync + 'static>,
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
        use $crate::{CallIndex, Terminator, IpfiInteger, ProcedureIndex, WireId, CompleteLockState};
        use dashmap::{DashMap, mapref::one::Ref};
        use fxhash::FxBuildHasher;
        use nohash_hasher::BuildNoHashHasher;
        #[cfg(feature = "serde")]
        use serde::{de::DeserializeOwned, Serialize};
        use $crate::interface_common::Procedure;
        // Regardless of async or not, we'll still use the std mutex
        use std::sync::Mutex;

        /// A single message held by an [`Interface`].
        pub(crate) struct MessageBuffer {
            /// The chunks in this message. As each chunk is terminated, a new one will be started. At
            /// all stages, new bytes will be appended to the last chunk. Importantly, chunks are defined
            /// by the sender of the message (e.g. they may be token streams), and hence are not fixed-size.
            ///
            /// Note that the majority of messages will be single-chunk, and even some streaming procedures
            /// will only send one chunk. For instance, a streaming procedure may be defined that sends
            /// bytes gradually, but, altogether, those bytes make up a single deserialisable object.
            chunks: Vec<Vec<u8>>,
            /// A lock representing whether or not the buffer is completed. This is not held for individual
            /// chunks.
            complete_lock: CompleteLock,
            /// A sender through which we can send chunks. This works with raw bytes, and the end receiver
            /// will be wrapped in a function capable of deserialising each individual chunk. This is locked
            /// to ensure it can be shared between threads, but this should be completely deadlock-safe,
            /// as only one person can access the `MessageBuffer` itself at a time (mutably, at least, so
            /// we do need to enforce the semantics of mutable borrowing for sending).
            ///
            /// The `Option` inside the `Mutex` is used to allow dropping the sender once a message has been
            /// terminated, hence ending the real-time stream, while retaining the information that there was
            /// once a sender (and hence that the message may be corrupted).
            // Whether this sender is from `tokio` or `std` depends on the import context this macro is
            // used in
            sender: Option<Mutex<Option<Sender<Vec<u8>>>>>,
        }

        /// A wrapper that allows receiving message chunks in real-time. Be mindful that, once one of these is created,
        /// all chunks that arrive for the message it tracks will be immediately sent to it, thereby preventing any
        /// calls to `.get()` on the [`Interface`] from accessing non-empty data.
        pub struct ChunkReceiver {
            // The type of this will depend on the import context in which this macro is used
            rx: Receiver<Vec<u8>>,
            /// The completion lock on the message we're receiving chunks for. This allows checking for poisons
            /// regularly.
            complete_lock: CompleteLock,
            /// The message identifier this chunk receiver tracks.
            // This is only used for preparing errors
            message_id: IpfiInteger,
            /// How long we should wait between receiver calls.
            timeout_millis: usize,
        }
        impl ChunkReceiver {
            fn new(rx: Receiver<Vec<u8>>, message_id: IpfiInteger, complete_lock: CompleteLock) -> Self {
                Self { rx, complete_lock, message_id, timeout_millis: 10 }
            }
            /// Sets the maximum number of milliseconds that this receiver could potentially wait for on a
            /// call to `.recv()`/`.recv_raw()` if the underlying wire terminates.
            ///
            /// The default value is 10 milliseconds: higher values will mean more time between a wire termination and
            /// an error being returned, and lower values will mean more resource-intensive waiting for values when
            /// termination does not occur.
            pub fn termination_timeout_millis(mut self, millis: usize) -> Self {
                self.timeout_millis = millis;
                self
            }
            /// Waits to receive the raw bytes of the next chunk.
            ///
            /// If this finds that the underlying message's completion lock has been poisoned (which would typically
            /// happen if the wire had been terminated), then it will return an error. Note that this does not necessarily
            /// mean that it cannot be polled again, as there may still be a chunk or two left to come through due
            /// to atomic operations and ordering. If the message is later completed, any poisons would be removed and
            /// this would calmly return `Ok(None)`.
            ///
            /// In order to continually check the completion lock to ensure it hasn't been poisoned, this implements
            /// receiving through a timeout. If the timeout is reached, the completion lock is checked again, and then
            /// the timeout restarts. This means this performs more resource-intensive waiting than a traditional receiver
            /// call would, although this should not be material for most applications. (The default timeout is 10
            /// milliseconds, which is thus the maximum time this would wait for after a wire had been terminated.)
            pub $($async)? fn recv_raw(&mut self) -> Result<Option<Vec<u8>>, Error> {
                loop {
                    if self.complete_lock.state()$(.$await)? == CompleteLockState::Poisoned {
                        // The lock is poisoned, but still check if there's anything the receiver can give us
                        return match self.rx.try_recv() {
                            Ok(bytes) => Ok(Some(bytes)),
                            // `TryRecvError` will be different depending on the import context in which this macro is used
                            //
                            // Either the channel is empty, and we probably won't see any messages every again, or it *appears*
                            // closed. It's important that we don't accept that, however, because that would occur from simply
                            // dropping the message buffer from the interface, which could happen in a sweep for old wire messages.
                            // As such, we have no reliable indicator of completion beyond the completion lock, so we'll
                            // defer to that. If we had really terminated gracefully, the completion lock would have been
                            // updated.
                            Err(_) => Err(Error::LockPoisoned { index: self.message_id }),
                        }
                    } else {
                        // let res = self.rx.recv()$(.$await.ok_or(()))?.ok();
                        // This method is implemented in async/blocking-specific ways: it blocks waiting for a value, then
                        // times out after some user-specified number of milliseconds, returning `None` if it couldn't get
                        // anything in that time (that way, we can re-check the completion lock for poisoning regularly)
                        match self.recv_timeout(self.timeout_millis)$(.$await)? {
                            // Timed out, check for poisons and start again
                            None => continue,
                            // Got something and we weren't poisoned when we last checked!
                            Some(val) => return Ok(val),
                        }
                    }
                }
            }
            /// Waits to receive the next chunk, deserialising it into the given type. Generally, all chunks will be
            /// deserialised into the same type, however it is perfeclty possible, if there is a known type layout,
            /// to deserialise one chunk as one type and a different one as another, although this is not recommended
            /// except in highly deterministic systems.
            ///
            /// The convoluted return type of this function indicates the following:
            ///     1. There could be a poisoned lock involved when we poll, so there might be an error with polling,
            ///     2. But if there isn't, the sender might have been dropped (i.e. message is complete),
            ///     3. Even if there is a chunk, we might fail to deserialise it,
            ///     4. If all that works, you get `T`.
            #[cfg(feature = "serde")]
            pub $($async)? fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<Result<T, Error>>, Error> {
                let rx_option = self.recv_raw()$(.$await)??;
                if let Some(bytes) = rx_option {
                    let decoded = match rmp_serde::decode::from_slice(&bytes) {
                        Ok(decoded) => decoded,
                        Err(err) => return Ok(Some(Err(err.into())))
                    };
                    Ok(Some(Ok(decoded)))
                } else {
                    Ok(None)
                }
            }
        }

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
        ///
        /// # Chunks
        ///
        /// Messages sent to an IPFI interface are sent as raw bytes, however, with the `serde` feature enabled, they can
        /// be deserialised into arbitrary types. Usually, a message will consist of only one type, however, sometimes,
        /// especially in the case of a procedure that yields multiple independent values (e.g. strings), a single message
        /// will consist of multiple discrete packets, all of which should be deserialised independently. For this use-case,
        /// IPFI implements a chunking system, whereby senders to an interface can explicitly terminate an individual chunk,
        /// before starting a new one. As a result, accessing the raw bytes of a message will yield a `Vec<Vec<u8>>`, a list
        /// of chunks (each of which is a list of bytes).
        ///
        /// Sometimes, one may wish to access chunks in real-time, as they are terminated individually, and this can be done
        /// through `.get_chunk_stream()`, which will return a [`ChunkReceiver`] that handles deserialisation. However, if this
        /// method is called, then, every time a chunk is terminated, it will unquestioningly be sent to the receiver, rather than
        /// being saved in the message buffer. Hence, after a real-time receiver is created, calls to methods like `.get()` will
        /// still succeed, but they will return empty messages. Only `.get_chunks()` is wise to this: it will explicitly make
        /// sure that no real-time interface is present before accessing the underlying message data. Note that, regardless of
        /// the methods called, it is impossible for messages to enter a broken state, provided they were entered into the interface
        /// correctly.
        pub struct Interface {
            /// The messages received over the interface. This is implemented as a concurrent hash map indexed by a `IpfiInteger`,
            /// which means that, when a message is read, we can remove its entry from the map entirely and its identifier can
            messages: DashMap<IpfiInteger, MessageBuffer, BuildNoHashHasher<IpfiInteger>>,
            /// A queue that produces unique message identifiers while allowing us to recycle old ones. This enables far greater
            /// memory efficiency.
            message_id_queue: RoiQueue,
            /// A record of locks used to wait on the existence of a certain message
            /// index. Since multiple threads could simultaneously wait for different message
            /// indices, this has to be implemented this way!
            creation_locks: DashMap<IpfiInteger, CompleteLock, BuildNoHashHasher<IpfiInteger>>,
            /// A list of procedures registered on this interface that those communicating with this
            /// program may execute. This amalgamates both streaming and standard procedures together.
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
            /// call. Any messages associated with this wire identifier will be popped automatically if this is called with
            /// `pop_messages`, which it generally should be in server-like contexts (otherwise, a client could cause an out-of-memory
            /// error by submitting just shy of enough arguments for the same procedure many times over different wires).
            ///
            /// Generally, this should be called within drop implementations, and it is not necessary to call this for the inbuilt [`Wire`].
            pub fn relinquish_id(&self, id: WireId) {
                self.wire_id_queue.relinquish(id.0);
            }
            /// Drops the message associated with accumulating arguments for the given procedure call. This could be called for both procedures
            /// that are still accumulating arguments and for those that are done accumulating, but which are still streaming. As these buffers
            /// have already been created, creation locks will never be an issue, while waiting for a completion lock would lead to waiting
            /// forever, as the internal lock would persist, but the version of it held by the interface would be dropped (however, waiting
            /// on completion locks for argument accumulation buffers would be very strange).
            pub fn drop_associated_call_message(&self, procedure_idx: ProcedureIndex, call_idx: CallIndex, wire_id: WireId) {
                if let Some((_, message_id)) = self.call_to_buffer_map.remove(&(procedure_idx, call_idx, wire_id)) {
                    // This will deal with recycling the ID appropriately
                    self.pop(message_id);
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
                f: impl Fn(Box<dyn Fn(R, bool) -> Result<(), Error> + Send + Sync + 'static>, A) + Send + Sync + 'static,
            ) {
                let closure = Box::new(move |raw_yielder: Box<dyn Fn(Vec<u8>, Terminator) -> Result<(), Error> + Send + Sync + 'static>, data: &[u8]| {
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
                        let data = rmp_serde::encode::to_vec(&data)?;
                        let terminator = if is_final { Terminator::Complete } else { Terminator::Chunk };
                        raw_yielder(data, terminator)
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
                f: impl Fn(Box<dyn Fn(Vec<u8>, Terminator) -> Result<(), Error> + Send + Sync + 'static>, &[u8]) + Send + Sync + 'static,
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
            fn pop(&self, id: IpfiInteger) -> Option<MessageBuffer> {
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
                yielder: impl Fn(Vec<u8>, Terminator) -> Result<(), Error> + Send + Sync + 'static,
            ) -> Result<Option<Vec<u8>>, Error> {
                // Get the buffer where the arguments are supposed to be, and then delete that mapping to free up some space
                let args_buf_idx = self.get_call_buffer(procedure_idx, call_idx, wire_id)$(.$await)?;
                self.call_to_buffer_map
                    .remove(&(procedure_idx, call_idx, wire_id));

                if let Some(procedure) = self.procedures.get(&procedure_idx) {
                    if let Some(m) = self.messages.get(&args_buf_idx) {
                        let message = m.value();
                        match message.complete_lock.state()$(.$await)? {
                            CompleteLockState::Complete => {
                                // The length marker will be inserted by the internal wrapper closure
                                let ret = match &*procedure {
                                    // Note that we always know there will be at least one chunk, so this can
                                    // never panic (and procedure calls are never chunked)
                                    Procedure::Standard { closure } => (closure)(&message.chunks[0]).map(|val| Some(val)),
                                    Procedure::Streaming { closure } => {
                                        (closure)(Box::new(yielder), &message.chunks[0])?;
                                        Ok(None)
                                    }
                                };
                                // Success, relinquish this message buffer
                                // To do that though, we have to drop anything referencing the map
                                drop(m);
                                self.pop(args_buf_idx);
                                ret
                            },
                            CompleteLockState::Incomplete => Err(Error::CallBufferIncomplete {
                                index: args_buf_idx,
                            }),
                            CompleteLockState::Poisoned => Err(Error::CallBufferPoisoned {
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
                    .insert(new_id, MessageBuffer {
                        // Always allocate one chunk first
                        chunks: vec![ Vec::new() ],
                        complete_lock: CompleteLock::new(),
                        // No sender is created until the user asks for it explicitly
                        sender: None,
                    });

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
            ///
            /// Notably, this will still append to a poisoned message buffer (because a poisoned completion lock merely
            /// indicates that it is unlikely that the message will ever be completed, it doesn't hurt to try). If this
            /// call would complete a poisoned buffer, its state will be changed to completed.
            pub $($async)? fn send(&self, datum: i8, message_id: IpfiInteger) -> Result<bool, Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    let message = m.value_mut();
                    // Only fail on completions, we'll allow poisons
                    if message.complete_lock.state()$(.$await)? == CompleteLockState::Complete {
                        Err(Error::AlreadyCompleted { index: message_id })
                    } else if datum < 0 {
                        message.complete_lock.mark_complete()$(.$await)?;
                        Ok(false)
                    } else {
                        // There will always be at least one chunk
                        message.chunks.last_mut().unwrap().push(datum as u8);
                        Ok(true)
                    }
                } else {
                    Err(Error::OutOfBounds { index: message_id })
                }
            }
            /// Sends many bytes through to the interface. When you have many bytes instead of just one at a time, this
            /// method should be preferred. Note that this method will not allow the termination of a message or chunk,
            /// and that should be handled separately.
            ///
            /// Like `.send()`, this will ignore poisoned message buffers, and will try to complete them if possible
            /// (without changing their state).
            pub $($async)? fn send_many(&self, data: &[u8], message_id: IpfiInteger) -> Result<(), Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    let message = m.value_mut();
                    if message.complete_lock.state()$(.$await)? == CompleteLockState::Complete {
                        Err(Error::AlreadyCompleted { index: message_id })
                    } else {
                        // There will always be at least one chunk
                        message.chunks.last_mut().unwrap().extend(data);
                        Ok(())
                    }
                } else {
                    Err(Error::OutOfBounds { index: message_id })
                }
            }
            /// Explicitly terminates the message with the given index. This will return an error if the message
            /// has already been terminated or if it was out-of-bounds. This will not change anything about the
            /// chunk layout of the message, and, for the final chunk, this should be called *instead of* terminating
            /// the chunk, otherwise an additional, empty chunk will be created. If there is a chunk receiver registered,
            /// this will send the final chunk through it (only if that chunk hasn't been sent before).
            ///
            /// For messages with real-time chunk receivers registered, this will drop the only sender, thus ending
            /// the channel, and it will pop the message, allowing the index to be reused.
            ///
            /// If this terminates a message with a poisoned completion lock, it will change the lock's state from
            /// poisoned to completed.
            pub $($async)? fn terminate_message(&self, message_id: IpfiInteger) -> Result<(), Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    let message = m.value_mut();
                    if message.complete_lock.state()$(.$await)? == CompleteLockState::Complete {
                        Err(Error::AlreadyCompleted { index: message_id })
                    } else {
                        message.complete_lock.mark_complete()$(.$await)?;
                        if let Some(sender) = &message.sender {
                            // We can guarantee that the referenced message will exist, because there will always be
                            // at least one chunk
                            let completed_chunk = message.chunks.remove(message.chunks.len() - 1);
                            // If sending the chunk fails (receiver dropped), add it back
                            // We know that this will be `Some(sender) internally, because it will only
                            // be set to `None` after termination, which we've checked for
                            let mut sender = sender.lock().unwrap();
                            match sender.as_ref().unwrap().send(completed_chunk) {
                                Ok(_) => {
                                    // We need to make sure there's at least one chunk to uphold internal invariants
                                    message.chunks.push(Vec::new());
                                    // And now drop the entire message for identifier recycling (no-one is going to
                                    // `.get()` it afterward)
                                    drop(sender);
                                    drop(m);
                                    self.pop(message_id);
                                },
                                Err(err) => {
                                    message.chunks.push(err.0);
                                    // Now drop the sender (no-one will access it after this, because we've marked the
                                    // message as completed), but maintain the information that there once was one (as
                                    // `Some(Mutex(None))`)
                                    *sender = None;
                                }
                            };
                        }
                        Ok(())
                    }
                } else {
                    Err(Error::OutOfBounds { index: message_id })
                }
            }
            /// Terminates the current chunk in the given message buffer, creating a new chunk to accept further
            /// data. This should not be called to terminate the final chunk, you should use `.terminate_message()`
            /// for that to properly seal the buffer. If there is a real-time chunk receiver registered for this
            /// message buffer, this method will take the terminated chunk and send it directly to that receiver,
            /// thereby removing it from the message buffer. This kind of potential "corruption" is signalled by
            /// the `Some(_)` value of `message.sender`.
            ///
            /// This will fail if the message itself has been terminated, but will succeed if the message has a poisoned
            /// completion lock.
            pub $($async)? fn terminate_chunk(&self, message_id: IpfiInteger) -> Result<(), Error> {
                if let Some(mut m) = self.messages.get_mut(&message_id) {
                    // Borrowing mutably here ensures we can't deadlock on the sender `Mutex`
                    let message = m.value_mut();
                    if message.complete_lock.state()$(.$await)? == CompleteLockState::Complete {
                        Err(Error::AlreadyCompleted { index: message_id })
                    } else {
                        // We'll always append further data to the last chunk
                        if let Some(sender) = &message.sender {
                            // We can guarantee that the referenced message will exist, because there will always be
                            // at least one chunk
                            let completed_chunk = message.chunks.remove(message.chunks.len() - 1);
                            // If sending the chunk fails (receiver dropped), add it back
                            // We know that this will be `Some(sender) internally, because it will only
                            // be set to `None` after termination, which we've checked for
                            match sender.lock().unwrap().as_ref().unwrap().send(completed_chunk) {
                                Ok(_) => (),
                                Err(err) => {
                                    message.chunks.push(err.0)
                                }
                            };
                        }
                        // We might have removed the only chunk, but regardless, we're adding a new one (always-one-
                        // chunk invariant preserved)
                        message.chunks.push(Vec::new());
                        Ok(())
                    }
                } else {
                    Err(Error::OutOfBounds { index: message_id })
                }
            }
            /// Creates a sender/receiver pair that can be used to accessing the chunks in a message in real-time. Once
            /// this method is called, any future `.get_chunks()` calls are guaranteed to return `None`, as they cannot
            /// provide useful information. Other `.get()`-type calls will yield no information, because chunks will be
            /// streamed out in real-time, and thereby removed from the message buffer. Essentially, once this is called,
            /// all bytes are piped out to the created receiver, rather than being saved.
            ///
            /// If the provided message buffer does not yet exist, this will wait for it to before creating the sender/receiver
            /// pair. This is done to avoid disrupting any existing creation locks that might be held on the message.
            pub $($async)? fn get_chunk_stream(&self, message_id: IpfiInteger) -> Result<ChunkReceiver, Error> {
                self.wait_for_message_creation(message_id)$(.$await)??;
                // This is guaranteed to exist, but it almost certainly won't be complete
                let mut message = self.messages.get_mut(&message_id).unwrap();
                // First, create a raw bytes sender/receiver pair, which we can wrap for deserialisation
                // Types are dependent on import context of this macro
                let (tx, rx) = channel::<Vec<u8>>();
                message.sender = Some(Mutex::new(Some(tx)));

                Ok(ChunkReceiver::new(rx, message_id, message.complete_lock.clone()))
            }
            /// Returns whether or not the given message is chunked. This will wait until the message has been completed
            /// before returning anything. Note that whether or not a message is chunked is purely determined by whether
            /// or not the sender terminates a chunk explicitly (thereby creating a second chunk, etc.).
            pub $($async)? fn is_chunked(&self, message_id: IpfiInteger) -> Result<bool, Error> {
                let message_ref = self.wait_for_message(message_id)$(.$await)??;
                Ok(message_ref.chunks.len() > 1)
            }
            /// Returns whether or not the given message has a real-time chunk receiver registered on it. If so, this likely
            /// indicates that the message buffer will be incomplete and should not be fetched directly. See [`Interface`] for
            /// further details on chunking. This method will wait until the given message has been completed.
            pub $($async)? fn has_stream(&self, message_id: IpfiInteger) -> Result<bool, Error> {
                let message_ref = self.wait_for_message(message_id)$(.$await)??;
                Ok(message_ref.sender.is_some())
            }
            /// Gets an object of the given type from the given message buffer index of the interface. This will block
            /// waiting for the given message buffer to be (1) created and (2) marked as complete. Depending on the
            /// caller's behaviour, this may block forever if they never complete the message.
            ///
            /// For messages with multiple chunks, this will only pay attention to the first chunk. For chunked messages,
            /// you should prefer `.get_chunks()`, which will deserialise as many chunks as are available. To determine whether
            /// or not a message has been chunked, you can use `.is_chunked()`.
            ///
            /// Note that this method will extract the underlying message from the given buffer index, leaving it
            /// available for future messages or procedure call metadata. This means that requesting the same message
            /// index may yield completely different data.
            #[cfg(feature = "serde")]
            pub $($async)? fn get<T: DeserializeOwned>(&self, message_id: IpfiInteger) -> Result<T, Error> {
                let chunks = self.get_raw(message_id)$(.$await)??;
                // There will always be at least one chunk
                let decoded = rmp_serde::decode::from_slice(&chunks[0])?;
                Ok(decoded)
            }
            /// Same as `.get()`, except this is designed to be used for procedures that are known to stream their output
            /// in discrete chunks. There are two broad types of streaming procedures: those that stream bytes gradually that
            /// eventually become part of a greater whole (all deserialised at once), and those that stream discrete objects that
            /// should all be deserialised independently. This function is for the latter type.
            ///
            /// Note that, if used on any other type of message, this will still work, it will just produce a vector of length 1
            /// (as there was only one chunk involved).
            ///
            /// Bear in mind that, if `.get_chunk_stream()` has been previously called on this message, then this will produce
            /// no data (as the chunks will have been streamed to the receiver in real-time). In that case, `None` will be returned
            /// for clarity.
            #[cfg(feature = "serde")]
            pub $($async)? fn get_chunks<T: DeserializeOwned>(&self, message_id: IpfiInteger) -> Result<Option<Vec<T>>, Error> {
                // Wait for the message and check if it has a sender attached: if so, this method would yield
                // no useful information because everything is streamed in real-time
                let message_ref = self.wait_for_message(message_id)$(.$await)??;
                if message_ref.sender.is_some() {
                    return Ok(None);
                }
                drop(message_ref);
                // We've already manually waited on the message, so we can safely directly pop it out
                let MessageBuffer { chunks, .. } = self.pop(message_id).unwrap();

                let mut decoded_chunks = Vec::new();
                for chunk in chunks {
                    let decoded = rmp_serde::decode::from_slice(&chunk)?;
                    decoded_chunks.push(decoded);
                }

                Ok(Some(decoded_chunks))
            }
            /// Same as `.get()`, but gets the raw byte array instead. This will return the chunks in which
            /// the message was sent, which, for the vast majority of messages, will be only one. See
            /// [`Interface`] to learn more about chunks.
            ///
            /// This will block until the message with the given identifier has been created and completed.
            ///
            /// Note that this method will extract the underlying message from the given buffer index, leaving it
            /// available for future messages or procedure call metadata. This means that requesting the same message
            /// index may yield completely different data. It is typically useful to use this method through some higher-level
            /// structure, such as a [`crate::CallHandle`].
            pub $($async)? fn get_raw(&self, message_id: IpfiInteger) -> Result<Vec<Vec<u8>>, Error> {
                // Wait for the creation and completion locks, but don't retain any of the information
                // we get
                let _ = self.wait_for_message(message_id)$(.$await)??;

                // This definitely exists if we got here (and it logically has to).
                // Note that this will also prepare the message identifier for reuse.
                // This is safe to call because we only hold a clone of the completion
                // lock.
                let message = self.pop(message_id).unwrap();
                Ok(message.chunks)
            }
            /// Waits until the given message has been created and completed, then returns the information about it as
            /// a reference. This returns an abstraction over a lock, which should be dropped as soon as possible.
            ///
            /// If this finds any of the underlying completion locks to be poisoned, it will fail.
            $($async)? fn wait_for_message(&self, message_id: IpfiInteger) -> Result<Ref<'_, IpfiInteger, MessageBuffer, BuildNoHashHasher<IpfiInteger>>, Error> {
                // We need two completion locks to be ready before we're ready: the first is
                // a creation lock on the message index, and the second is a lock on its
                // actual completion. We'll start by creating a new completion lock if one
                // doesn't already exist. Any future creations will thereby trigger it. Of
                // course, we won't need to do this if the entry already exists.
                let message_lock = {
                    let message = if let Some(message) = self.messages.get(&message_id) {
                        message
                    } else {
                        self.wait_for_message_creation(message_id)$(.$await)??;

                        // We can guarantee that this will exist now
                        self.messages
                            .get(&message_id)
                            .expect("complete lock should signal that message exists")
                    };
                    // Cheap clone
                    message.complete_lock.clone()
                };

                let state = message_lock.wait_for_completion()$(.$await)?;
                if state == CompleteLockState::Poisoned {
                    return Err(Error::LockPoisoned { index: message_id })
                }
                Ok(self.messages.get(&message_id).unwrap())
            }
            /// Waits until the given message has been created. This will return as soon as it has been, without regard
            /// for whether or not it has been completed.
            ///
            /// If this finds a poisoned creation lock, it will fail. However, if the message buffer exists already,
            /// this will succeed *even if* there is a poisoned creation lock (this almost certainly indicates
            /// that the message in this buffer is not the one you were expecting, but that can be handled by pre-allocating
            /// message buffers for later filling and reading, which should be done at a higher level than this method,
            /// e.g. [`Wire`] does this whenever a procedure call is started).
            $($async)? fn wait_for_message_creation(&self, message_id: IpfiInteger) -> Result<(), Error> {
                // If the message buffer is already present, return immediately, otherwise register a creation
                // lock and wait for it (a poisoned creation lock and a message buffer that exists would be
                // indicative of a different message having been created, which, frankly, we don't care about
                // at the interface level, that's a wire problem)
                //
                // Note that the wire allocates buffers for responses the second it starts a procedure call,
                // so it will never poison them (these semantics are therefore only used at the interface level,
                // which is low-level enough that we can do this comfortably)
                if self.messages.get(&message_id).is_none() {
                    let lock = if let Some(lock) = self.creation_locks.get(&message_id) {
                        lock.clone()
                    } else {
                        // This is the only time at which we create a new creation lock
                        let lock = CompleteLock::new();
                        self.creation_locks.insert(message_id, lock.clone());
                        lock
                    };

                    let state = lock.wait_for_completion()$(.$await)?;
                    if state == CompleteLockState::Poisoned {
                        return Err(Error::LockPoisoned { index: message_id })
                    }
                    // Now delete that lock to free up some space
                    // Note that our read-only creation locks instance will definitely have been dropped
                    // by this point
                    self.creation_locks.remove(&message_id);
                };

                Ok(())
            }
            /// Gets the [`CompleteLock`] for the given message, if it exists.
            pub(crate) fn get_complete_lock(&self, message_id: IpfiInteger) -> Option<CompleteLock> {
                if let Some(message) = self.messages.get(&message_id) {
                    Some(message.complete_lock.clone())
                } else {
                    None
                }
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
            let msg = interface.get_raw(0)$(.$await)?.unwrap();
            assert_eq!(msg[0], [42]);
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
            let msg = interface.get_raw(id)$(.$await)?.unwrap();
            assert_eq!(msg[0], []);
        }
        #[$test_attr]
        $($async)? fn chunk_streaming_should_work() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;
            // This will resolve immediately because the message has been created
            let mut rx = interface.get_chunk_stream(id)$(.$await)?.unwrap();

            interface.send(42, id)$(.$await)?.unwrap();
            interface.terminate_chunk(id)$(.$await)?.unwrap();
            assert_eq!(rx.recv_raw()$(.$await)?.unwrap().unwrap(), [42]);
            interface.send(43, id)$(.$await)?.unwrap();
            // We don't terminate that final chunk!
            interface.terminate_message(id)$(.$await)?.unwrap();
            assert_eq!(rx.recv_raw()$(.$await)?.unwrap().unwrap(), [43]);

            assert!(rx.recv_raw()$(.$await)?.unwrap().is_none());

            // Make sure the message buffer has been reallocated
            let new_id = interface.push()$(.$await)?;
            assert_eq!(id, new_id);
        }
        #[$test_attr]
        $($async)? fn stream_then_drop_rx_should_buffer() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;
            // This will resolve immediately because the message has been created
            let mut rx = interface.get_chunk_stream(id)$(.$await)?.unwrap();

            interface.send(42, id)$(.$await)?.unwrap();
            interface.terminate_chunk(id)$(.$await)?.unwrap();
            assert_eq!(rx.recv_raw()$(.$await)?.unwrap().unwrap(), [42]);
            // Now drop the receiver: future chunks should be buffered
            drop(rx);
            interface.send(43, id)$(.$await)?.unwrap();
            // We don't terminate that final chunk!
            // Because the receiver has been dropped, the message buffer should not be recycled
            interface.terminate_message(id)$(.$await)?.unwrap();

            // Getting the buffer itself should fail, but, getting the raw bytes should show
            // the last chunk buffered (as it couldn't be sent)
            #[cfg(feature = "serde")]
            assert!(interface.get_chunks::<()>(id)$(.$await)?.unwrap().is_none());
            assert_eq!(interface.get_raw(id)$(.$await)?.unwrap(), [[43]]);
        }
        #[cfg(feature = "serde")]
        #[$test_attr]
        $($async)? fn get_chunks_should_work() {
            let interface = Interface::new();
            let id = interface.push()$(.$await)?;

            interface.send(42, id)$(.$await)?.unwrap();
            interface.terminate_chunk(id)$(.$await)?.unwrap();
            interface.send(43, id)$(.$await)?.unwrap();
            // We don't terminate that final chunk!
            interface.terminate_message(id)$(.$await)?.unwrap();

            assert_eq!(interface.get_chunks::<u32>(id)$(.$await)?.unwrap().unwrap(), [42, 43]);
        }
    };
}
