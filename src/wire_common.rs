//! This module contains code that is common to the blocking and asynchronous `Wire`.

/// This defines nearly all the methods on `Wire`, together with auxiliary types like `CallHandle`, but it does not
/// define methods that do thread spawning, or the `AutonomousWireHandle` type.
#[macro_export]
#[doc(hidden)]
macro_rules! define_wire {
    ($read_ty: ty, $write_ty: ty, $reader_fn: ident $(, $async: tt, $await: tt)?) => {
        use super::interface::Interface;
        use $crate::integer::*;
        #[cfg(feature = "serde")]
        use $crate::procedure_args::ProcedureArgs;
        use $crate::wire_utils::*;
        use $crate::{error::Error, Terminator, CallIndex, IpfiInteger, ProcedureIndex, WireId};
        use crossbeam_queue::SegQueue;
        use dashmap::{DashMap, DashSet};
        use fxhash::FxBuildHasher;
        use nohash_hasher::BuildNoHashHasher;
        #[cfg(feature = "serde")]
        use serde::de::DeserializeOwned;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use super::{ChunkReceiver, complete_lock::CompleteLock};

        const DEFAULT_MAX_ONGOING_PROCEDURES: IpfiInteger = 10; // TODO

        /// A mechanism to interact ergonomically with an interface using synchronous Rust I/O buffers.
        ///
        /// This wire sets up an internal message queue of data needing to be sent, allowing bidirectional messaging even over locked buffers (such as stdio)
        /// without leading to race conditions.
        ///
        /// Note that the process of reading messages from the other side of the wire will implicitly perform procedure calls to satisfy their requests. As such,
        /// any failures in procedures will be propagated upward, failing this wire. If this behaviour is undesired, you should manually restart calls like `.fill()`
        /// on any errors. Even in server scenarios, however, this behaviour is usually desired, as sending a response back to the client is extremely difficult.
        /// Genuinely falible procedures are fully supported, and errors will be sent over the wire, by "failures in procedures", we are typically referring to
        /// errors in serialising return types for transmission, or the like.
        ///
        /// # Termination
        ///
        /// An important concept in IPFI is that there can be many [`Wire`]s to one [`Interface`], and that, although it's generally not a good idea, there
        /// can be many input/output buffers for a single [`Wire`]. However, fundamentally a wire represents a connection between the local program and some remote
        /// program, which means that, when the remote program terminates, continuing to use the wire is invalid. As such, wires maintain an internal flag
        /// that notes whether or not the other side has terminated them yet, and, if this flag is found to be set, all operation on the wire will fail.
        ///
        /// The main method used for reading data into an IPFI [`Interface`] is `wire.fill()`, which will attempt to read as many full messages as it can before
        /// one ends (messages can be sent in as many partials as the sender pleases). If an `UnexpectedEof` error occurs here, the termination flag will
        /// automatically be set under the assumption that the remote program has terminated. In some rare cases, you may wish to recover from this, which can
        /// be done by creating a new [`Wire`] (i.e. once a wire has been terminated, it can no longer be used, and all method calls will immediately fail).
        ///
        /// # Procedure calls
        ///
        /// ## Call indices
        ///
        /// IPFI manages procedure calls through a system of procedure indices (e.g. the `print_hello()` procedure might be assigned index 0, you as the programmer
        /// control this) and *call indices*. Since a procedure call can be done piecemeal, with even fractions of arguments being sent as raw bytes, the remote
        /// program must be able to know which partials are associated with each other so it can accumulate the partials into one place for later assembly. This
        /// is done through call indices, which are essentially counters maintained by each wire that mark how many times it has executed a certain procedure.
        /// Then, on the remote side, messages received on the same wire from the same call index of a certain procedure index will be grouped together and
        /// interpreted as one set of arguments when a message is sent that marks the call as complete, allowing it to be properly executed.
        ///
        /// ## Transmitting arguments as bytes
        ///
        /// There are several low-level methods within this `struct` that allow you to transmit partial arguments to a remote procedure directly, although
        /// doing so is somewhat precarious. When you register a procedure locally, IPFI expects that procedure to take a tuple with an arbitrary number of
        /// elements, but it will accept arguments in any list type (e.g. a procedure that takes three `bool`s could have its arguments provided on the
        /// remote caller as `vec![true, false, true]`, `[true, false, true]`, or `(true, false, true)`), which will then be serialized to a MessagePack
        /// list type, which is fundamentally the serialization of all the component elements, preceded by a length prefix. It is the caller's responsibility
        /// to perform this serialization manually, **ignoring the length prefix**, which will be added on on the remote when the call is marked as complete.
        /// This system allows the extreme flexibility of sending not just one argument at a time, but even fractions of single arguments, enabling advanced
        /// streaming use-cases and interoperability with the underlying single-byte IPFI interface functions.
        ///
        /// You should also be aware that sending zero bytes for your arguments will be interpreted by the remote as a termination order, and the message buffer
        /// it uses to receive the arguments for that particular procedure call will be closed. This would prevent sending any further arguments, and likely
        /// lead to a corrupt procedure call.
        ///
        /// Note that, unless you're working on extremely low-level applications, 99% of the time the `.call()` method will be absolutely fine for you, and
        /// if you want to send a few complete arguments at a time, you can use the partial methods that let you provide something implementing [`ProcedureArgs`],
        /// a trait that will perform the underlying serialization for you.
        #[derive(Clone)]
        pub struct Wire<'a> {
            /// The unique identifier the attached interface has assigned to this wire.
            id: WireId,
            /// The interface to interact with.
            interface: &'a Interface,
            /// Whether or not this wire was instantiated with support for handling general messages. Wires used in server-like situations, where a client will be
            /// calling functions from the server, but not the other way around, should generally set this to `true` to eliminate  anunnecessary potential attack
            /// surface.
            ///
            /// When this is set to `true`, function calls will also be disabled on this wire.
            module_style: bool,
            /// An internal message queue, used for preventing interleaved writing, where part of one message is sent, and then part of another, due
            /// to race conditions. This would be fine if the headers were kept intact, although race conditions are rarely so courteous.
            ///
            /// This uses a lock-free queue to maximise concurrent performance.
            queue: Arc<SegQueue<Vec<u8>>>,
            /// A map that keeps track of how many times each remote procedure has been called, allowing call indices to be intelligently
            /// and largely internally handled.
            remote_call_counter: Arc<DashMap<ProcedureIndex, IpfiInteger, BuildNoHashHasher<IpfiInteger>>>,
            /// A map of procedure and call indices (respectively) to local response buffer indices. Once an entry is added here, it should never be changed until
            /// it is removed.
            ///
            /// This serves as a security mechanism to ensure that response messages are mapped *locally* to response buffer indices, which means we can be sure
            /// that a remote cannot access and control arbitrary message buffers on our local interface, thereby compromising other wires.
            response_idx_map: Arc<DashMap<(ProcedureIndex, CallIndex), IpfiInteger, FxBuildHasher>>,
            /// A set of procedure and call indices (respectively) that are actively accumulating arguments. The entries in here will be removed
            /// as the procedure call's response has been completely streamed, hence this partly mirrors the contents
            /// of the interface's `call_to_buf_map`. This is needed for when the wire is dropped, allowing us to instruct the interface to
            /// drop in-progress procedure calls that wouldn't otherwise complete, preventing a denial-of-service attack based on exhausting
            /// memory by creating many partial procedure call fragments across different wires. A
            ///
            /// The length of this set is also monitored to impose a limit on the number of procedure calls a single wire can simultaneously
            /// call (another type of DoS attack).
            ongoing_procedures: Arc<DashSet<(ProcedureIndex, CallIndex), FxBuildHasher>>,
            /// The maximum number of procedures that the other side of this wire is allowed to accumulate arguments for or execute
            /// simultaneously. This is used to prevent DoS attacks based on sending partial arguments for many calls, or based on
            /// executing resource-heavy procedures en-masse.
            max_ongoing_procedures: IpfiInteger,
            /// A map of response indices in the attached interface to their completion locks, for call handles generated from procedure
            /// calls made on this wire that are waiting/unwaited upon. This is used when the wire is dropped to automatically poison these
            /// locks, preventing call handles from hanging forever on wire termination.
            idle_call_handles: Arc<DashMap<IpfiInteger, CompleteLock, BuildNoHashHasher<IpfiInteger>>>,
            /// A flag for whether or not this wire has been terminated. Once it has been, *all* further operations will fail.
            terminated: Arc<AtomicBool>,
        }
        impl<'a> Wire<'a> {
            /// Creates a new buffer-based wire to work with the given interface. This takes in the interface to work with and a writeable
            /// buffer to use for termination when this wire is dropped.
            ///
            /// # Security
            ///
            /// This method implicitly allows this wire to call functions on the remote, which is fine, except that calling a function means
            /// you need to receive some kind of response (that reception will be done automatically by `.fill()`. it does *not* require waiting
            /// on the call handle, that is only used to get the response to you and deserialize it). Response messages do not have access to
            /// local message buffers, and their security is tightly controlled, however they present an additional attack surface that may be
            /// totally unnecessary in module-style applications.
            ///
            /// In short, although there are no known security holes in the procedure response system, programs that exist to have their procedures
            /// called by others (called 'module-style programs'), and that will not call procedures themselves, should prefer [`Wire::new_module`],
            /// which disables several wire features to reduce the attack surface.
            pub fn new(interface: &'a Interface) -> Self {
                Self {
                    id: interface.get_id(),
                    interface,
                    module_style: false,
                    queue: Arc::new(SegQueue::new()),
                    remote_call_counter: Arc::new(DashMap::default()),
                    response_idx_map: Arc::new(DashMap::default()),
                    ongoing_procedures: Arc::new(DashSet::default()),
                    max_ongoing_procedures: DEFAULT_MAX_ONGOING_PROCEDURES,
                    idle_call_handles: Arc::new(DashMap::default()),

                    // If we detect an EOF, this will be set
                    terminated: Arc::new(AtomicBool::new(false)),
                }
            }
            /// Creates a new buffer-based wire to work with the given interface. This is the same as `.new()`, except it disables support for
            /// response messages, which are unnecessary in some contexts, where they would only present an additional attack surface. If you
            /// intend for this wire to be used by the remote to call local functions, but not the other way around, you should use this method.
            pub fn new_module(interface: &'a Interface) -> Self {
                Self {
                    id: interface.get_id(),
                    interface,
                    module_style: true,
                    queue: Arc::new(SegQueue::new()),
                    remote_call_counter: Arc::new(DashMap::default()),
                    response_idx_map: Arc::new(DashMap::default()),
                    ongoing_procedures: Arc::new(DashSet::default()),
                    max_ongoing_procedures: DEFAULT_MAX_ONGOING_PROCEDURES,
                    idle_call_handles: Arc::new(DashMap::default()),

                    // If we detect an EOF, this will be set
                    terminated: Arc::new(AtomicBool::new(false)),
                }
            }
            /// Sets the maximum number of ongoing procedures that the other end of this wire may invoke. "Ongoing procedures"
            /// refer to two things: procedures that are still accumulating arguments, and procedures that haven't finished
            /// executing yet. By setting a maximum number for these and rejecting any past that, we can prevent denial-of-service
            /// attacks that try to overload our end of the wire by starting too many procedure calls at once.
            ///
            /// The default for this is 10, but this value may change without warning until v1.0, so you're advised to set
            /// this manually when DoS protection matters!
            ///
            /// For protecting a server, this should be combined with [`Self::new_module`], which will prevent this wire from
            /// processing response messages, which can be used to exhaust memory if used maliciously.
            pub fn max_ongoing_procedures(mut self, value: IpfiInteger) -> Self {
                self.max_ongoing_procedures = value;
                self
            }
            /// Asks the wire whether or not it has been terminated. This polls an internal flag that can be read by many threads
            /// simultaneously, and as such this operation is cheap.
            ///
            /// See the `struct` documentation for further information about wire termination.
            #[inline(always)]
            pub fn is_terminated(&self) -> bool {
                // All subsequent operations will see our `Ordering::Release` storing
                self.terminated.load(Ordering::Acquire)
            }
            /// Calls the procedure with the given remote procedure index. This will return a handle you can use to block waiting
            /// for the return value of the procedure.
            ///
            /// Generally, this should be preferred as a high-level method, although several lower-level methods are available for
            /// sending one argument at a time, or similar piecemeal use-cases.
            #[cfg(feature = "serde")]
            pub $($async)? fn call(
                &self,
                procedure_idx: ProcedureIndex,
                args: impl ProcedureArgs,
            ) -> Result<CallHandle, Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                } else if self.module_style {
                    return Err(Error::CallFromModule);
                }

                let args = args.into_bytes()?;
                self.call_with_bytes(procedure_idx, &args)$(.$await)?
            }

            // --- Low-level procedure calling methods ---

            /// Calls the procedure with the given remote procedure index. This will prepare the local interface for a response also.
            /// This function will transmit the given argument buffer assuming that it does not know all the arguments, and it will
            /// leave the remote buffer that stores these arguments open.
            ///
            /// This will return the call index for this execution, for later reference in continuing or finishing the call. The index
            /// of the local message buffer where the response is held will be returned when the call is completed.
            ///
            /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
            #[cfg(feature = "serde")]
            pub $($async)? fn start_call_with_partial_args(
                &self,
                procedure_idx: ProcedureIndex,
                args: impl ProcedureArgs,
            ) -> Result<CallIndex, Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                } else if self.module_style {
                    return Err(Error::CallFromModule);
                }

                let args = args.into_bytes()?;
                self.start_call_with_partial_bytes(procedure_idx, &args)$(.$await)?
            }
            /// Same as `.start_call_with_partial_args()`, but this works directly with bytes, allowing you to send strange things
            /// like a two-thirds of an argument.
            ///
            /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
            pub $($async)? fn start_call_with_partial_bytes(
                &self,
                procedure_idx: ProcedureIndex,
                args: &[u8],
            ) -> Result<CallIndex, Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                } else if self.module_style {
                    return Err(Error::CallFromModule);
                }

                if args.is_empty() {
                    return Err(Error::ZeroLengthInNonTerminating);
                }

                // Create a new entry in the remote call counter for this call index
                let call_idx =
                    if let Some(mut last_call_idx) = self.remote_call_counter.get_mut(&procedure_idx) {
                        // We need to update what's in there
                        let new_call_idx = *last_call_idx + 1;
                        *last_call_idx = new_call_idx;
                        CallIndex(new_call_idx)
                    } else {
                        // We need to insert a new entry
                        self.remote_call_counter.insert(procedure_idx, 0);
                        CallIndex(0)
                    };
                // Allocate a new message buffer on the interface that we'll receive the response into
                let response_idx = self.interface.push()$(.$await)?;
                // Add that to the remote index map so we can retrieve it for later continutation and termination
                // of this call
                {
                    self.response_idx_map
                        .insert((procedure_idx, call_idx), response_idx);
                }

                // This doesn't need to access the remote call counter, so we can leave it be safely
                self.continue_given_call_with_bytes(procedure_idx, call_idx, args)?;

                Ok(call_idx)
            }
            /// Same as `.call()`, but this works directly with bytes. You must be careful to provide the full byte serialization here,
            /// and be sure to follow the above guidance on this! (I.e. you must not include the length marker.)
            ///
            /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
            pub $($async)? fn call_with_bytes(
                &self,
                procedure_idx: ProcedureIndex,
                args: &[u8],
            ) -> Result<CallHandle, Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                } else if self.module_style {
                    return Err(Error::CallFromModule);
                }

                // We allow zero-length payloads here (for functions with no arguments in particular)

                // Create a new entry in the remote call counter for this call index
                let call_idx =
                    if let Some(mut last_call_idx) = self.remote_call_counter.get_mut(&procedure_idx) {
                        // We need to update what's in there
                        let new_call_idx = *last_call_idx + 1;
                        *last_call_idx = new_call_idx;
                        CallIndex(new_call_idx)
                    } else {
                        // We need to insert a new entry
                        self.remote_call_counter.insert(procedure_idx, 0);
                        CallIndex(0)
                    };
                // Allocate a new message buffer on the interface that we'll receive the response into
                //
                // NOTE: The fact that we allocate this immediately means we never have to poison creation
                // locks if the wire terminates, it's safe to poison the completion locks only
                let response_idx = self.interface.push()$(.$await)?;
                // Add that to the remote index map so we can retrieve it for later continutation and termination
                // of this call
                self.response_idx_map
                    .insert((procedure_idx, call_idx), response_idx);

                // Construct the message we want to send
                let msg = Message::Call {
                    procedure_idx,
                    call_idx,
                    args,
                    // We can terminate in one go here
                    terminator: Terminator::Complete,
                };
                // Convert that message into bytes and place it in the queue
                let bytes = msg.to_bytes();
                self.queue.push(bytes);

                // Get the response index we're using
                let response_idx = self.get_response_idx(procedure_idx, call_idx)?;
                Ok(CallHandle::new(
                    response_idx,
                    self.interface,
                    self.idle_call_handles.clone(),
                ))
            }
            /// Continues the procedure call with the given remote procedure index and call index by sending the given arguments.
            /// This will not terminate the message, and will leave it open for calling.
            ///
            /// For an explanation of how call indices work, see [`Wire`].
            ///
            /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
            #[cfg(feature = "serde")]
            pub fn continue_given_call_with_args(
                &self,
                procedure_idx: ProcedureIndex,
                call_idx: CallIndex,
                args: impl ProcedureArgs,
            ) -> Result<(), Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                } else if self.module_style {
                    return Err(Error::CallFromModule);
                }

                let args = args.into_bytes()?;
                self.continue_given_call_with_bytes(procedure_idx, call_idx, &args)
            }
            /// Same as `.continue_given_call_with_args()`, but this works directly with bytes.
            ///
            /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
            pub fn continue_given_call_with_bytes(
                &self,
                procedure_idx: ProcedureIndex,
                call_idx: CallIndex,
                args: &[u8],
            ) -> Result<(), Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                } else if self.module_style {
                    return Err(Error::CallFromModule);
                }

                if args.is_empty() {
                    return Err(Error::ZeroLengthInNonTerminating);
                }

                // Construct the message we want to send
                let msg = Message::Call {
                    procedure_idx,
                    call_idx,
                    args,
                    terminator: Terminator::None,
                };
                // Convert that message into bytes and place it in the queue
                let bytes = msg.to_bytes();
                self.queue.push(bytes);

                Ok(())
            }
            /// Terminates the given call by sending a zero-length argument payload.
            ///
            /// This will return a handle the caller can use to wait on the return value of the remote procedure.
            ///
            /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
            pub fn end_given_call(
                &self,
                procedure_idx: ProcedureIndex,
                call_idx: CallIndex,
            ) -> Result<CallHandle, Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                } else if self.module_style {
                    return Err(Error::CallFromModule);
                }

                // Construct the zero-length payload message we want to send
                let msg = Message::Call {
                    procedure_idx,
                    call_idx,
                    args: &[],
                    terminator: Terminator::Complete,
                };
                // Convert that message into bytes and place it in the queue
                let bytes = msg.to_bytes();
                self.queue.push(bytes);

                // Get the response index we're using
                let response_idx = self.get_response_idx(procedure_idx, call_idx)?;
                Ok(CallHandle::new(
                    response_idx,
                    self.interface,
                    self.idle_call_handles.clone(),
                ))
            }
            /// Gets the local response buffer index for the given procedure and call indices. If no such buffer has been allocated,
            /// this will return an error.
            #[inline]
            fn get_response_idx(
                &self,
                procedure_idx: ProcedureIndex,
                call_idx: CallIndex,
            ) -> Result<IpfiInteger, Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                }

                self.response_idx_map
                    .get(&(procedure_idx, call_idx))
                    .map(|x| *x)
                    .ok_or(Error::NoResponseBuffer {
                        index: procedure_idx.0,
                        call_idx: call_idx.0,
                    })
            }

            /// Writes a manual end-of-input signal to the output, which, when flushed (potentially automatically if you've called `wire.start()`),
            /// will cause any `wire.fill()` calls in the remote program to return `None`, which can be checked for termination. This is
            /// necessary when communicating with single-threaded programs, which must read all their input at once, to tell them to stop reading
            /// and start doing other work. This does not signal the termination of the wire, or even that there will not be any input in future,
            /// it simply allows you to signal to the remote that it should start doing something else. Internally, the reception of this case is
            /// generally not handled, and it is up to you as a user to handle it.
            pub fn signal_end_of_input(&self) -> Result<(), Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                }
                let msg = Message::EndOfInput;
                let bytes = msg.to_bytes();
                self.queue.push(bytes);

                Ok(())
            }
            /// Writes a termination signal to the output, which will permanently neuter this connection, and all further operations on this wire
            /// will fail. This will **not** mark this wire as terminated internally, but it will lead to the termination of the other side's wire.
            /// To mark this wire as terminated (thereby halting ongoing procedures, etc.), you should drop it, or allow it to receive a termination
            /// signal from the other side.
            ///
            /// This does a similar thing to [`signal_termination`], except that this also requires flushing, and it can be more useful if `wire.open`
            /// has consumed the writer handle. However, if you need to send a termination signal when your program shuts down after any errors,
            /// [`signal_termination`] should be preferred.
            pub fn signal_termination(&self) {
                let msg = Message::Termination;
                let bytes = msg.to_bytes();
                self.queue.push(bytes);
            }
            /// Sets the internal termination flag to `true`, causing all subsequent operations to fail. This will also cause all
            /// ongoing streaming procedures to have their next `yielder()` call return an error, leading them to fail (in most cases).
            /// In other words, this will single-handedly terminate all active work on this wire. This will also drop all message buffers
            /// that were being used to accumulate arguments for procedure calls.
            ///
            /// Additionally, this will poison the completion locks of all responses to procedure calls made over this wire that have
            /// not yet been gotten.
            ///
            /// This is not accessible externally because termination should only occur when a termination signal is received, or when
            /// the wire is dropped (there is literally nothing you can do with it once it's terminated, so we may as well keep the
            /// interface simpler, especially with names like `.signal_termination()` floating about).
            // NOTE: If any part of this method changes, the `Drop` impl must be updated too
            $($async)? fn mark_terminated(&self) {
                self.mark_terminated_sync();
                Self::poison_idle_call_handles(&self.idle_call_handles)$(.$await)?
            }
            /// Internal function for the synchronous parts of the termination implementation. This simplifies the drop implementation.
            fn mark_terminated_sync(&self) {
                // We use `Ordering::Release` to make sure that all subsequent reads see this.
                // This will cause all ongoing procedures to fail in their next yield.
                self.terminated.store(true, Ordering::Release);
                // Drop all message buffers that were being used to accumulate arguments for procedure calls (effectively cancelling
                // calls that were getting ready to start, but which hadn't yet)
                for entry in self.ongoing_procedures.iter() {
                    self.interface.drop_associated_call_message(entry.0, entry.1, self.id);
                }
            }
            /// Internal associated function for the (possibly) asynchronous parts of the termination implementation (that doesn't depend on `self`).
            $($async)? fn poison_idle_call_handles(idle_call_handles: impl AsRef<DashMap<IpfiInteger, CompleteLock, BuildNoHashHasher<IpfiInteger>>>) {
                // Poison the completion locks of any call handles that either haven't been waited on yet, or that are being
                // waited on actively (note that it doesn't matter if one doesn't get removed in time, because the complete locks are
                // literal clones, we don't look them up by message identifier; it's still important that they're cleaned out though,
                // to avoid a memory exhaustion DoS attack)
                for entry in idle_call_handles.as_ref().iter() {
                    // Note that any of these that have actually completed will not really be poisoned
                    entry.value().poison()$(.$await)?;
                }
            }

            /// Receives a single message from the given reader, sending it into the interface as appropriate. This contains
            /// the core logic that handles messages, partial procedure calls, etc.
            ///
            /// If a procedure call is completed in this read, this method will automatically block waiting for the response,
            /// and it will followingly add said response to the internal writer queue.
            ///
            /// This returns whether or not it read a message/call termination message). Remember, however, that receiving
            /// a call termination message does not mean the response to that call has been sent in the case of a streaming
            /// procedure! Alternately, `None` will be returned if there was a manual end of input message, or on a wire
            /// termination.
            pub $($async)? fn receive_one(
                &self,
                #[allow(unused_parens)]
                reader: &mut ($read_ty),
            ) -> Result<Option<bool>, Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                }

                // First is the type of message
                let mut ty_buf = [0u8];
                reader.read_exact(&mut ty_buf)$(.$await)??;
                let ty = u8::from_le_bytes(ty_buf);
                match ty {
                    // Expressly ignore response messages if we're a module
                    2 if self.module_style => Ok(Some(false)),
                    // Procedure call or response thereto (same base format)
                    1 | 2 => {
                        // First is the multiflag
                        let mut multiflag_buf = [0u8; 1];
                        reader.read_exact(&mut multiflag_buf)$(.$await)??;
                        let bits = u8_to_bool_array(multiflag_buf[0]);
                        let terminator = Terminator::from_flag((bits[6], bits[7]))?;

                        // Then the procedure index
                        let procedure_idx = Integer::from_flag((bits[0], bits[1]))
                            .$reader_fn(reader)
                            $(.$await)??
                            .into_int()
                            .ok_or(Error::IdxTooBig)?;
                        let procedure_idx = ProcedureIndex(procedure_idx);
                        // Then the call index
                        let call_idx = Integer::from_flag((bits[2], bits[3]))
                            .$reader_fn(reader)
                            $(.$await)??
                            .into_int()
                            .ok_or(Error::IdxTooBig)?;
                        let call_idx = CallIndex(call_idx);
                        // Then the number of bytes to expect (this will only be too big if there's a pointer width disparity)
                        let num_bytes = Integer::from_flag((bits[4], bits[5]))
                            .$reader_fn(reader)
                            $(.$await)??
                            .into_usize()
                            .ok_or(Error::IdxTooBig)?;

                        // Read the number of bytes we expect
                        let mut bytes = vec![0u8; num_bytes];
                        reader.read_exact(&mut bytes)$(.$await)??;

                        // Now diverge
                        match ty {
                            // Call
                            1 => {
                                // If this is a new call and it would break the limit of ongoing procedures, don't let it through
                                if !self.ongoing_procedures.contains(&(procedure_idx, call_idx)) && self.ongoing_procedures.len() >= self.max_ongoing_procedures as usize {
                                    // TODO Need to send an explicit error message back here
                                    return Ok(Some(false))
                                }
                                // We need to know where to put argument information
                                let call_buf_idx = self
                                    .interface
                                    .get_call_buffer(procedure_idx, call_idx, self.id)$(.$await)?;
                                // Make sure this is registered as an ongoing procedure call (even if it immediately finishes, there might be an
                                // error that could have been deliberately caused, and we don't want to take any chances with DoS attacks)
                                self.ongoing_procedures.insert((procedure_idx, call_idx));
                                // And now put it there, accumulating across many messages as necessary
                                self.interface.send_many(&bytes, call_buf_idx)$(.$await)??;

                                // If this message was the last in its series, we should call the procedure, assuming we have all the arguments
                                // we need
                                if terminator == Terminator::Complete {
                                    self.interface.terminate_message(call_buf_idx)$(.$await)??;

                                    // This will actually execute!
                                    // Note that this will remove the `call_buf_idx` mapping and drain the arguments out of that buffer
                                    let self_queue = self.queue.clone();
                                    let ongoing_procedures = self.ongoing_procedures.clone();
                                    let terminated = self.terminated.clone();
                                    let ret = self
                                        .interface
                                        .call_procedure(procedure_idx, call_idx, self.id, move |bytes, terminator| {
                                            // If we've terminated, all ongoing procedures should be informed the next time they yield a value
                                            // to prevent unnecessary computations and potential DoS attempts
                                            if terminated.load(Ordering::Acquire) {
                                                return Err(Error::WireTerminated)
                                            }
                                            // TODO Need a way for this to send errors too...(third argument?)
                                            let ret_msg = Message::Response {
                                                procedure_idx,
                                                call_idx,
                                                message: &bytes,
                                                terminator,
                                            };
                                            let ret_msg_bytes = ret_msg.to_bytes();
                                            self_queue.push(ret_msg_bytes);
                                            // If this call is complete, remove it from the list of ongoing procedures
                                            if terminator == Terminator::Complete {
                                                ongoing_procedures.remove(&(procedure_idx, call_idx));
                                            }

                                            Ok(())
                                        })
                                        $(.$await)??;
                                    // We can only send a response immediately if this isn't a streaming procedure
                                    if let Some(ret_bytes) = ret {
                                        let ret_msg = Message::Response {
                                            procedure_idx,
                                            call_idx,
                                            message: &ret_bytes,
                                            terminator: Terminator::Complete,
                                        };
                                        let ret_msg_bytes = ret_msg.to_bytes();
                                        self.queue.push(ret_msg_bytes);
                                        // This call is complete, remove it from the ongoing procedures registry
                                        self.ongoing_procedures.remove(&(procedure_idx, call_idx));
                                    }

                                    // Regardless, we did read a call termination message
                                    Ok(Some(true))
                                } else {
                                    // Chunks are not a thing for arguments (they're not needed, we know from the procedure
                                    // definition how to serialise/deserialise, specifically we know the number of arguments!)
                                    Ok(Some(false))
                                }
                            }
                            // Response
                            2 => {
                                // Get the local message buffer index that we said we'd put the response into
                                let response_idx = self.get_response_idx(procedure_idx, call_idx)?;
                                self.interface.send_many(&bytes, response_idx)$(.$await)??;

                                // If this is marked as the last in its series, we should round off the response message and
                                // implicitly signal to any waiting call handles that it's ready to be read
                                if terminator == Terminator::Complete {
                                    // We don't terminate the final chunk (that would create an extra empty chunk)
                                    self.interface.terminate_message(response_idx)$(.$await)??;
                                    // If that succeeded, the user should be ready to fetch that, and the index will be reused
                                    // after they have, so we should remove it from this map (this is to save space only, because we
                                    // know we won't be querying this procedure call again)
                                    self.response_idx_map.remove(&(procedure_idx, call_idx));

                                    Ok(Some(true))
                                } else if terminator == Terminator::Chunk {
                                    self.interface.terminate_chunk(response_idx)$(.$await)??;
                                    // Chunk terminations don't count as message terminations
                                    Ok(Some(false))
                                } else {
                                    Ok(Some(false))
                                }
                            }
                            _ => unreachable!(),
                        }
                    }
                    // Manual end of input (we should stop whatever called this with a clean error)
                    3 => Ok(None),
                    // Termination: the other program is shutting down and is no longer capable of receiving messages
                    // This is the IPFI equivalent of 'expected EOF'
                    0 => {
                        // This will lead all other operation to fail, potentially in the middle of their work
                        //
                        // This is also very neat, because it means terminations on the other side, which might fail
                        // streaming procedures, can be used to kill receivers for their output on our side too!
                        self.mark_terminated()$(.$await)?;

                        Ok(None)
                    }
                    // Unknown message types will be ignored
                    _ => Ok(Some(false)),
                }
            }
            /// Writes all messages currently in the internal write queue to the given output stream.
            ///
            /// This is named `.flush_partial()` to make clear that this is usually not sufficient, and that an end-of-input
            /// message needs to be sent first! To combine these two calls, use `.flush_end()`.
            pub $($async)? fn flush_partial(
                &self,
                #[allow(unused_parens)]
                writer: &mut ($write_ty)
            ) -> Result<(), Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                }

                // This will remove the written messages from the queue
                while let Some(msg_bytes) = self.queue.pop() {
                    writer.write_all(&msg_bytes)$(.$await)??;
                }
                writer.flush()$(.$await)??;

                Ok(())
            }
            /// Flushes everything that has been written to this wire, along with an end-of-input message. This does *not* terminate
            /// the wire, it merely states that we've written everything we can for now. In programs with a ping-pong structure (e.g.
            /// Alice calls a method and waits for a response from Bob), this is what you would use when you're done with the ping and
            /// waiting for the pong.
            ///
            /// Internally, this simply combines `.signal_end_of_input()` and `.flush_partial()`.
            pub $($async)? fn flush_end(
                &self,
                #[allow(unused_parens)]
                writer: &mut ($write_ty)
            ) -> Result<(), Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                }

                self.signal_end_of_input()?;
                self.flush_partial(writer)$(.$await)?
            }
            /// Flushes everything that has been written to this wire, along with a termination signal, which tells the remote that your
            /// program is now terminating, and that this wire has been rendered invalid. If your program may send further data later, you
            /// should use `.flush_end()` instead, which sends a temporary end-of-input message. You can imagine the difference between
            /// the two as `.flush_end()` being 'over' and this method being 'over and out' (which, in real radio communication, would
            /// actually just be 'out').
            ///
            /// **IMPORTANT:** Never call this while waiting for a response, such as to a function call! Once the other side receives a
            /// termination signal, it will immediately self-poison, causing *all* future method calls on their wire to fail. In many
            /// cases, you won't actually need a termination signal, such as if you know the structure of both programs involved, and
            /// you know what messages they'll send to each other. A termination signal is used to literally force the other program to
            /// stop sending data. To see how this poison impacts the wire, look at the source code and the little preamble before every
            /// single method: if the wire has terminated, they will all immediately fail! Call this only when communicating with an
            /// unknown or untrusted program, but keeping in mind that they could easily ignore the termination signal (i.e. this cannot
            /// be used as a superficial measure).
            pub $($async)? fn flush_terminate(
                &self,
                #[allow(unused_parens)]
                writer: &mut ($write_ty),
            ) -> Result<(), Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                }

                self.signal_termination();
                self.flush_partial(writer)$(.$await)?
            }
            /// An ergonomic equivalent of `.receive_one()` that receives messages until either an error occurs, or until the remote program
            /// sends a manual end-of-input signal (with `.signal_end_of_input()`). This should only be used in single-threaded scenarios
            /// to read a block of messages, as otherwise this is rendered superfluous by `.open()`, which runs this automatically in a
            /// loop.
            ///
            /// Note that the remote program must *manually* send that end-of-input signal, and when it does this will be dependent on the
            /// needs of your unique program.
            ///
            /// **WARNING:** This method will internally handle `UnexpectedEof` errors and terminate the wire itself, assuming that the given
            /// buffer is the only means of communication with the other side of the wire. If this is not the case, you should manually call
            /// `.receive_one()` until it returns `None`, to mimic the behaviour of this method. Note that such errors will still be returned,
            /// after the termination flag has been set.
            pub $($async)? fn fill(
                &self,
                #[allow(unused_parens)]
                reader: &mut ($read_ty)
            ) -> Result<(), Error> {
                if self.is_terminated() {
                    return Err(Error::WireTerminated);
                }

                // Read until end-of-input is sent
                loop {
                    match self.receive_one(reader)$(.$await)? {
                        Ok(None) => break Ok(()),
                        // Some other message, keep reading
                        Ok(_) => continue,
                        // If we run into an unexpected EOF at any time during the `.receive_one()` call, the other side has almost
                        // certainly terminated from that buffer
                        Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                            // This will prevent all further operations on this wire
                            self.mark_terminated()$(.$await)?;
                            break Err(Error::IoError(err));
                        }
                        // Any unexpected errors should be propagated
                        Err(err) => break Err(err),
                    }
                }
            }
        }

        // Drop implementation is outside this macro

        /// A handle for waiting on the return values of remote procedure calls. This is necessary because calling a remote procedure
        /// requires `.flush()` to be called, so waiting inside a function like `.call()` would make very little sense.
        // There is no point in making this `Clone` to avoid issues around dropping the wire, as the `'a` lifetime is what gets in
        // the way
        //
        // We keep the `.wait()` methods even in the async API because this determines type conversion
        #[must_use]
        pub struct CallHandle<'a> {
            /// The message index to wait for. If this is improperly initialised, we will probably get completely different and almost
            /// certainly invalid data.
            response_idx: IpfiInteger,
            /// The interface where the response will appear.
            interface: &'a Interface,
            /// The same map of idle call handles that the [`Wire`] which produces this call handle holds. This is necessary because
            /// the wire completely stops caring about a procedure call once it's abstracted out to a call handle, so we have
            /// to manually remove the response index this call handle tracks from the list of idle handles as soon as we've definitely
            /// got the result. That way, we can prevent it from being poisoned.
            // XXX: Is it theoretically possible for there to be a race condition where we finish up with a message buffer, it gets
            // recycled, and then, in between removing it, the wire hgets dropped, leading to some other message that has taken up
            // residence being poisoned? Not sure about this, but we could stop it by holding a mutable reference to the idle call
            // handles while we wait (but that blocks the entire rest of the wire)...
            idle_call_handles: Arc<DashMap<IpfiInteger, CompleteLock, BuildNoHashHasher<IpfiInteger>>>,
        }
        impl<'a> CallHandle<'a> {
            /// Create a new call handle and register it with the wire for auto-poisoning on termination. This requires that the given
            /// message index has already been created.
            fn new(
                response_idx: IpfiInteger,
                interface: &'a Interface,
                idle_call_handles: Arc<DashMap<IpfiInteger, CompleteLock, BuildNoHashHasher<IpfiInteger>>>
            ) -> Self {
                // Add this call handle to the list
                let complete_lock = interface.get_complete_lock(response_idx).unwrap();
                idle_call_handles.insert(response_idx, complete_lock);

                Self {
                    response_idx,
                    interface,
                    idle_call_handles,
                }
            }
            /// Waits for the procedure call to complete and returns the result.
            ///
            /// **Termination behaviour:** if the wire terminates during this call, the completion lock on the message this waits
            /// for will be explicitly poisoned, causing this call to fail with [`Error::LockPoisoned`].
            #[cfg(feature = "serde")]
            #[inline]
            pub $($async)? fn wait<T: DeserializeOwned>(self) -> Result<T, Error> {
                let res = self.interface.get(self.response_idx)$(.$await)??;
                self.idle_call_handles.remove(&self.response_idx);
                Ok(res)
            }
            /// Same as `.wait()`, but this will use chunk-based deserialisation, and is intended for use with procedures that send
            /// sequences of discrete values gradually over the wire. For more information, see [`Interface::get_chunks`].
            ///
            /// **Termination behaviour:** if the wire terminates during this call, the completion lock on the message this waits
            /// for will be explicitly poisoned, causing this call to fail with [`Error::LockPoisoned`].
            #[cfg(feature = "serde")]
            #[inline]
            pub $($async)? fn wait_chunks<T: DeserializeOwned>(self) -> Result<Option<Vec<T>>, Error> {
                let res = self.interface.get_chunks(self.response_idx)$(.$await)??;
                self.idle_call_handles.remove(&self.response_idx);
                Ok(res)
            }
            /// Creates a [`ChunkReceiverHandle`] for incrementally receiving each chunk as it arrives. This can be used for accessing
            /// data in a semi-realtime way while still abstracting over the issue of partial chunks. This will wait until the
            /// first response data is received before creating a channel.
            ///
            /// **Termination behaviour:** if the wire terminates while a receiver is still held, that receiver will be intelligently
            /// poisoned, and any remaining values will be yielded, before an error is returned that the wire was terminated.
            #[inline]
            pub $($async)? fn wait_chunk_stream(self) -> Result<ChunkReceiverHandle, Error> {
                let rx = self.interface.get_chunk_stream(self.response_idx)$(.$await)??;
                Ok(ChunkReceiverHandle {
                    rx,
                    idle_call_handles: self.idle_call_handles,
                    response_idx: self.response_idx,
                })
            }
            /// Same as `.wait()`, but this works directly with bytes. This will return the chunks in the message (to learn more about
            /// chunks, see [`Interface`]).
            ///
            /// **Termination behaviour:** if the wire terminates during this call, the completion lock on the message this waits
            /// for will be explicitly poisoned, causing this call to fail with [`Error::LockPoisoned`].
            #[inline]
            pub $($async)? fn wait_bytes(self) -> Result<Vec<Vec<u8>>, Error> {
                let res = self.interface.get_raw(self.response_idx)$(.$await)??;
                self.idle_call_handles.remove(&self.response_idx);
                Ok(res)
            }
            /// Turns this call handle into the response index that can be used directly with the [`Interface`].
            /// This allows using lower-level methods on the interface that aren't accessible on the call
            /// handle, but you generally won't need these.
            ///
            /// This is typically required when you want to check whether or not a call handle is ready
            /// to yield.
            ///
            /// **Warning:** calling this will remove the underlying response index from a list the [`Wire`] maintains
            /// of response buffers that haven't been accessed yet. It uses this list to auto-poison those messages'
            /// completion locks so that waiting handles don't hang forever if the wire terminates, but, once this is
            /// called, that tracking can no longer occur! If you call this method, you should be extra careful about
            /// how you handle a terminating wire, as there will be no automatic poisoning of this response buffer anymore!
            pub fn into_response_idx(self) -> IpfiInteger {
                self.idle_call_handles.remove(&self.response_idx);
                self.response_idx
            }
        }

        /// A thin wrapper over [`ChunkReceiver`] produced by a [`CallHandle`]. This works with the [`Wire`] internally to coordinate when the receiver
        /// finishes so the wire knows that it doesn't need to poison the message's complete lock.
        ///
        /// This provides no method for turning itself into a [`ChunkReceiver`] because there should be literally no case in which you want to override
        /// the wire's semantics on this. If you do, then you should abort auto-poisoning behaviour altogether with [`CallHandle::into_response_idx`].
        pub struct ChunkReceiverHandle {
            /// The underlying receiver.
            rx: ChunkReceiver,
            /// A copy of the idle call handles from the [`Wire`].
            idle_call_handles: Arc<DashMap<IpfiInteger, CompleteLock, BuildNoHashHasher<IpfiInteger>>>,
            /// The response index this receiver tracks (used to remove that index from `idle_call_handles` once the receiver is finished).
            response_idx: IpfiInteger,
        }
        impl ChunkReceiverHandle {
            /// See [`ChunkReceiver::termination_timeout_millis`].
            pub fn termination_timeout_millis(mut self, millis: usize) -> Self {
                self.rx = self.rx.termination_timeout_millis(millis);
                self
            }
            /// See [`ChunkReceiver::recv_raw`].
            pub $($async)? fn recv_raw(&mut self) -> Result<Option<Vec<u8>>, Error> {
                let res = self.rx.recv_raw()$(.$await)?;
                match res {
                    // If the receiver finished smoothly, strike this from the list of handles to poison
                    Ok(None) => {
                        self.idle_call_handles.remove(&self.response_idx);
                        Ok(None)
                    },
                    // Otherwise, we're just a thin wrapper
                    _ => res
                }
            }
            /// See [`ChunkReceiver::recv`].
            #[cfg(feature = "serde")]
            pub $($async)? fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<Result<T, Error>>, Error> {
                let res = self.rx.recv::<T>()$(.$await)?;
                match res {
                    // If the receiver finished smoothly, strike this from the list of handles to poison
                    Ok(None) => {
                        self.idle_call_handles.remove(&self.response_idx);
                        Ok(None)
                    },
                    // Otherwise, we're just a thin wrapper
                    _ => res
                }
            }
        }

        /// Signals the termination of this program to any others that may be holding wires to it.
        ///
        /// Generally, it is a good idea to call this function in the cleanup of your program, including if any errors occur,
        /// however, if you're using the common pattern of communciating with another program through your own program's
        /// stdout stream, you won't need to, as shutting down will close that stream and send an EOF signal, which the other
        /// program will detect, implicitly causing a termination and preventing further writes.
        ///
        /// For more information on wire terminations, see [`Wire`].
        pub $($async)? fn signal_termination(
            #[allow(unused_parens)]
            writer: &mut ($write_ty)
        ) -> Result<(), std::io::Error> {
            let msg = Message::Termination;
            let bytes = msg.to_bytes();
            writer.write_all(&bytes)$(.$await)??;

            Ok(())
        }
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! define_wire_tests {
    ($test_attr: path $(, $async: tt, $await: tt)?) => {
        use super::Wire;
        use super::{Interface, ProcedureIndex};
        use std::io::Cursor;

        struct Actor {
            interface: &'static Interface,
            wire: Wire<'static>,
            input: Cursor<Vec<u8>>,
        }
        impl Actor {
            fn new() -> Self {
                let interface = Box::leak(Box::new(Interface::new()));
                let wire = Wire::new(interface);
                Self {
                    interface,
                    wire,
                    input: Cursor::new(Vec::new()),
                }
            }
        }

        // We can only really tests synchronous calls without a ton of extra *stuff*, which is unrealistic anyway because
        // half the time we'll be using stdio and the other half tcp (that's why the examples exist!)
        #[$test_attr]
        $($async)? fn sync_oneshot_byte_call_should_work() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            // A function that adds `42` to the end of a byte stream
            alice.interface.add_raw_procedure(0, |bytes| {
                let mut bytes = bytes.to_vec();
                bytes.extend([42]);

                bytes
            });

            let handle = bob
                .wire
                .call_with_bytes(ProcedureIndex(0), &[0, 1, 2, 3])
                $(.$await)?;
            assert!(handle.is_ok());

            // Bob signals that he's done and sends everything he has to Alice
            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0); // Test only

            // Alice reads until end-of-input and responds to everything she finds
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            // Alice signals she's done and sends everything she has to Bob
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0); // Test only

            // Bob likewise reads until he has everything he needs
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.unwrap().wait_bytes()$(.$await)?.unwrap();
            assert_eq!(result[0], [0, 1, 2, 3, 42]);
        }
        #[$test_attr]
        $($async)? fn two_bytes_calls_should_use_correct_call_indices() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            alice.interface.add_raw_procedure(0, |bytes| {
                let mut bytes = bytes.to_vec();
                bytes.extend([42]);

                bytes
            });

            // First call
            let call_idx = bob
                .wire
                .start_call_with_partial_bytes(ProcedureIndex(0), &[0, 1, 2, 3])
                $(.$await)?
                .unwrap();
            assert_eq!(call_idx.0, 0);
            let handle = bob
                .wire
                .end_given_call(ProcedureIndex(0), call_idx)
                .unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.wait_bytes()$(.$await)?.unwrap();
            assert_eq!(result[0], [0, 1, 2, 3, 42]);

            // Test cleanup
            alice.input = Cursor::new(Vec::new());
            bob.input = Cursor::new(Vec::new());

            // Second call
            let call_idx = bob
                .wire
                .start_call_with_partial_bytes(ProcedureIndex(0), &[0, 1, 2, 3])
                $(.$await)?
                .unwrap();
            assert_eq!(call_idx.0, 1);
            let handle = bob
                .wire
                .end_given_call(ProcedureIndex(0), call_idx)
                .unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.wait_bytes()$(.$await)?.unwrap();
            assert_eq!(result[0], [0, 1, 2, 3, 42]);
        }
        #[$test_attr]
        $($async)? fn partial_bytes_call_should_work() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            // A function that adds `42` to the end of a byte stream
            alice.interface.add_raw_procedure(0, |bytes| {
                let mut bytes = bytes.to_vec();
                bytes.extend([42]);

                bytes
            });

            let call_idx = bob
                .wire
                .start_call_with_partial_bytes(ProcedureIndex(0), &[0])
                $(.$await)?;
            assert!(call_idx.is_ok());
            let call_idx = call_idx.unwrap();
            // This should be the first time we've called the method
            assert_eq!(call_idx.0, 0);
            // Try continuing the call
            assert!(bob
                    .wire
                    .continue_given_call_with_bytes(ProcedureIndex(0), call_idx, &[1, 2, 3])
                    .is_ok());
            // And now try to end it
            let handle = bob.wire.end_given_call(ProcedureIndex(0), call_idx);
            assert!(handle.is_ok());

            // Bob signals that he's done and sends everything he has to Alice
            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0); // Test only

            // Alice reads until end-of-input and responds to everything she finds
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            // Alice signals she's done and sends everything she has to Bob
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0); // Test only

            // Bob likewise reads until he has everything he needs
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.unwrap().wait_bytes()$(.$await)?.unwrap();
            assert_eq!(result[0], [0, 1, 2, 3, 42]);
        }
        #[$test_attr]
        $($async)? fn other_side_should_pick_up_send_after_end() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            // A function that adds `42` to the end of a byte stream
            alice.interface.add_raw_procedure(0, |bytes| {
                let mut bytes = bytes.to_vec();
                bytes.extend([42]);

                bytes
            });

            let call_idx = bob
                .wire
                .start_call_with_partial_bytes(ProcedureIndex(0), &[0, 1, 2, 3])
                $(.$await)?;
            assert!(call_idx.is_ok());
            let call_idx = call_idx.unwrap();
            let handle = bob.wire.end_given_call(ProcedureIndex(0), call_idx);
            assert!(handle.is_ok());
            // Try continuing the call (should work locally)
            assert!(bob
                    .wire
                    .continue_given_call_with_bytes(ProcedureIndex(0), call_idx, &[1, 2, 3])
                    .is_ok());

            // Bob signals that he's done and sends everything he has to Alice
            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0); // Test only

            // Alice should still be fine (should have recycled indices on her end)
            assert!(alice.wire.fill(&mut alice.input)$(.$await)?.is_ok());
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.unwrap().wait_bytes()$(.$await)?.unwrap();
            assert_eq!(result[0], [0, 1, 2, 3, 42]);
        }
        #[cfg(feature = "serde")]
        #[$test_attr]
        $($async)? fn oneshot_args_call_should_work() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            // A greeting procedure
            alice
                .interface
                .add_procedure(0, |(first, last): (String, String)| {
                    format!("Hello, {first} {last}!")
                });

            let handle = bob.wire.call(ProcedureIndex(0), ("John", "Doe"))$(.$await)?;
            assert!(handle.is_ok());

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.unwrap().wait::<String>()$(.$await)?;
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "Hello, John Doe!");
        }
        #[cfg(feature = "serde")]
        #[$test_attr]
        $($async)? fn call_with_wrong_type_should_fail() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            // A greeting procedure
            alice
                .interface
                .add_procedure(0, |(first, last): (String, String)| {
                    format!("Hello, {first} {last}!")
                });

            let handle = bob.wire.call(ProcedureIndex(0), ("John", "Doe"))$(.$await)?;
            assert!(handle.is_ok());

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.unwrap().wait::<u32>()$(.$await)?;
            assert!(result.is_err());
        }
        #[cfg(feature = "serde")]
        #[$test_attr]
        $($async)? fn call_with_wrong_arg_types_should_fail() {
            let mut alice = Actor::new();
            let bob = Actor::new();
            // A greeting procedure
            alice
                .interface
                .add_procedure(0, |(first, last): (String, String)| {
                    format!("Hello, {first} {last}!")
                });

            let handle = bob.wire.call(ProcedureIndex(0), (0, 1))$(.$await)?;
            assert!(handle.is_ok());

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            // While attempting to respond, Alice should encounter an error
            assert!(alice.wire.fill(&mut alice.input)$(.$await)?.is_err());
        }
        #[cfg(feature = "serde")]
        #[$test_attr]
        $($async)? fn partial_args_call_should_work() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            alice
                .interface
                .add_procedure(0, |(first, last): (String, String)| {
                    format!("Hello, {first} {last}!")
                });

            let call_idx = bob
                .wire
                .start_call_with_partial_args(ProcedureIndex(0), ("John",))
                $(.$await)?;
            assert!(call_idx.is_ok());
            let call_idx = call_idx.unwrap();
            // This should be the first time we've called the method
            assert_eq!(call_idx.0, 0);
            // Try continuing the call
            assert!(bob
                    .wire
                    .continue_given_call_with_args(ProcedureIndex(0), call_idx, ("Doe",))
                    .is_ok());
            // And now try to end it
            let handle = bob.wire.end_given_call(ProcedureIndex(0), call_idx);
            assert!(handle.is_ok());

            // Bob signals that he's done and sends everything he has to Alice
            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0); // Test only

            // Alice reads until end-of-input and responds to everything she finds
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            // Alice signals she's done and sends everything she has to Bob
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0); // Test only

            // Bob likewise reads until he has everything he needs
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.unwrap().wait::<String>()$(.$await)?;
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "Hello, John Doe!");
        }
        #[cfg(feature = "serde")]
        #[$test_attr]
        $($async)? fn streaming_procedure_should_work() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            alice
                .interface
                .add_sequence_procedure(0, |yielder, (): ()| {
                    yielder(1, false).unwrap();
                    yielder(2, false).unwrap();
                    // TODO Way of testing when we don't terminate properly?
                    yielder(3, true).unwrap();
                });

            let handle = bob.wire.call(ProcedureIndex(0), ())$(.$await)?;
            assert!(handle.is_ok());

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            let result = handle.unwrap().wait_chunks::<u32>()$(.$await)?;
            assert!(result.is_ok());
            assert_eq!(result.unwrap().unwrap(), [1, 2, 3]);
        }
        #[$test_attr]
        $($async)? fn streaming_both_ends_should_work() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            alice
                .interface
                .add_sequence_procedure(0, |yielder, (): ()| {
                    // We don't bother with proper delays or the like here, thread management
                    // etc. is solely the responsibility of this function (so we would just
                    // be testing Rust's threading systems, which is redundant)
                    yielder(1, false).unwrap();
                    yielder(2, false).unwrap();
                    yielder(3, true).unwrap();
                });

            let handle = bob.wire.call(ProcedureIndex(0), ())$(.$await)?;
            assert!(handle.is_ok());
            let mut rx = handle.unwrap().wait_chunk_stream()$(.$await)?.unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            assert_eq!(rx.recv_raw()$(.$await)?.unwrap().unwrap(), [1]);
            assert_eq!(rx.recv_raw()$(.$await)?.unwrap().unwrap(), [2]);
            assert_eq!(rx.recv_raw()$(.$await)?.unwrap().unwrap(), [3]);

            assert!(rx.recv_raw()$(.$await)?.unwrap().is_none());
        }
        #[$test_attr]
        $($async)? fn terminating_wire_should_halt_waiting_call_handle() {
            let mut alice = Actor::new();
            let mut bob = Actor::new();
            alice
                .interface
                .add_sequence_procedure(0, move |yielder, (): ()| {
                    // This procedure will never finish, so we'd be waiting forever if
                    // the wire didn't terminate
                    yielder(1, false).unwrap();
                });

            let handle = bob.wire.call(ProcedureIndex(0), ())$(.$await)?;
            assert!(handle.is_ok());

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            // After we mark the wire as terminated, all further yields on ongoing streaming
            // procedures should fail, signalling them to terminate at their discretion
            alice.wire.signal_termination();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            drop(alice);
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            // The handle should also have been poisoned
            assert!(handle.unwrap().wait_bytes()$(.$await)?.is_err());
        }
        #[$test_attr]
        $($async)? fn terminating_wire_should_halt_streaming_procedure() {
            use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

            let mut alice = Actor::new();
            let mut bob = Actor::new();
            let yield_failed = Arc::new(AtomicBool::new(false));
            let yf = yield_failed.clone();
            alice
                .interface
                .add_sequence_procedure(0, move |yielder, (): ()| {
                    yielder(1, false).unwrap();
                    let yf = yf.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        // By now, the wire should have been terminated
                        yf.store(yielder(2, true).is_err(), Ordering::Release);
                    });
                });

            let handle = bob.wire.call(ProcedureIndex(0), ())$(.$await)?;
            assert!(handle.is_ok());
            let mut rx = handle.unwrap().wait_chunk_stream()$(.$await)?.unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            // After we mark the wire as terminated, all further yields on ongoing streaming
            // procedures should fail, signalling them to terminate at their discretion
            alice.wire.signal_termination();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            drop(alice);
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            std::thread::sleep(std::time::Duration::from_millis(15));
            assert!(yield_failed.load(Ordering::Acquire));

            // Even though the receiver is now poisoned, we should be able to get the last sent
            // value
            assert_eq!(rx.recv_raw()$(.$await)?.unwrap().unwrap(), [1]);

            // After that, though, there should be nothing
            assert!(rx.recv_raw()$(.$await)?.is_err());
        }
        #[$test_attr]
        $($async)? fn too_many_accumulating_calls_should_fail() {
            let mut alice = Actor::new();
            alice.wire = alice.wire.max_ongoing_procedures(2);
            let mut bob = Actor::new();
            alice
                .interface
                .add_procedure(0, move |(num,): (u32,)| {
                    num + 42
                });

            let call_idx_1 = bob.wire.start_call_with_partial_args(ProcedureIndex(0), (0,))$(.$await)?.unwrap();
            let call_idx_2 = bob.wire.start_call_with_partial_args(ProcedureIndex(0), (0,))$(.$await)?.unwrap();
            // This should be rejected
            let call_idx_3 = bob.wire.start_call_with_partial_args(ProcedureIndex(0), (0,))$(.$await)?.unwrap();

            let handle_1 = bob.wire.end_given_call(ProcedureIndex(0), call_idx_1).unwrap();
            let handle_2 = bob.wire.end_given_call(ProcedureIndex(0), call_idx_2).unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();
            alice.input = Cursor::new(Vec::new());
            bob.input = Cursor::new(Vec::new());

            assert_eq!(handle_1.wait_bytes()$(.$await)?.unwrap()[0], [42]);
            assert_eq!(handle_2.wait_bytes()$(.$await)?.unwrap()[0], [42]);

            // A new call should work perfectly well
            let handle_4 = bob.wire.call(ProcedureIndex(0), (0,))$(.$await)?.unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();
            alice.input = Cursor::new(Vec::new());
            bob.input = Cursor::new(Vec::new());

            assert_eq!(handle_4.wait_bytes()$(.$await)?.unwrap()[0], [42]);

            // But, as our original third call was rejected, trying to finish it will fail
            let _ = bob.wire.end_given_call(ProcedureIndex(0), call_idx_3).unwrap();
            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            assert!(alice.wire.fill(&mut alice.input)$(.$await)?.is_err());
        }
        #[$test_attr]
        $($async)? fn under_limit_of_simultaneous_calls_should_succeed() {
            use std::sync::{Arc, atomic::{AtomicU8, Ordering}};

            let mut alice = Actor::new();
            let mut bob = Actor::new();
            let num_calls = Arc::new(AtomicU8::new(0));
            let nc = num_calls.clone();
            alice
                .interface
                .add_sequence_procedure(0, move |yielder, (): ()| {
                    let nc = nc.clone();
                    std::thread::spawn(move || {
                        nc.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(15));
                        yielder(42, true).unwrap();
                    });
                });

            // We won't wait for any of these, we'll just directly check how many times the procedure has executed
            let _ = bob.wire.call(ProcedureIndex(0), ())$(.$await)?.unwrap();
            let _ = bob.wire.call(ProcedureIndex(0), ())$(.$await)?.unwrap();
            let _ = bob.wire.call(ProcedureIndex(0), ())$(.$await)?.unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            // NOTE: Timing is a bit tight here, spurious failures *may* occur (try raising the 15ms above and this)
            std::thread::sleep(std::time::Duration::from_millis(50));
            assert_eq!(num_calls.load(Ordering::SeqCst), 3);
        }
        #[$test_attr]
        $($async)? fn too_many_simultaneous_calls_should_fail() {
            use std::sync::{Arc, atomic::{AtomicU8, Ordering}};

            let mut alice = Actor::new();
            alice.wire = alice.wire.max_ongoing_procedures(2);
            let mut bob = Actor::new();
            let num_calls = Arc::new(AtomicU8::new(0));
            let nc = num_calls.clone();
            alice
                .interface
                .add_sequence_procedure(0, move |yielder, (): ()| {
                    let nc = nc.clone();
                    std::thread::spawn(move || {
                        nc.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(15));
                        yielder(42, true).unwrap();
                    });
                });

            // We won't wait for any of these, we'll just directly check how many times the procedure has executed
            let _ = bob.wire.call(ProcedureIndex(0), ())$(.$await)?.unwrap();
            let _ = bob.wire.call(ProcedureIndex(0), ())$(.$await)?.unwrap();
            let _ = bob.wire.call(ProcedureIndex(0), ())$(.$await)?.unwrap();

            bob.wire.flush_end(&mut alice.input)$(.$await)?.unwrap();
            alice.input.set_position(0);
            alice.wire.fill(&mut alice.input)$(.$await)?.unwrap();
            alice.wire.flush_end(&mut bob.input)$(.$await)?.unwrap();
            bob.input.set_position(0);
            bob.wire.fill(&mut bob.input)$(.$await)?.unwrap();

            std::thread::sleep(std::time::Duration::from_millis(50));
            assert_eq!(num_calls.load(Ordering::SeqCst), 2);
        }
    };
}
