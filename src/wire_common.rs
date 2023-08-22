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
        use dashmap::DashMap;
        use fxhash::FxBuildHasher;
        use nohash_hasher::BuildNoHashHasher;
        #[cfg(feature = "serde")]
        use serde::de::DeserializeOwned;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use super::ChunkReceiver;

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
            /// This serves as a secuirty mechanism to ensure that response messages are mapped *locally* to response buffer indices, which means we can be sure
            /// that a remote cannot access and control arbitrary message buffers on our local interface, thereby compromising other wires.
            response_idx_map: Arc<DashMap<(ProcedureIndex, CallIndex), IpfiInteger, FxBuildHasher>>,
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

                    // If we detect an EOF, this will be set
                    terminated: Arc::new(AtomicBool::new(false)),
                }
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
                Ok(CallHandle {
                    response_idx,
                    interface: self.interface,
                })
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
                Ok(CallHandle {
                    response_idx,
                    interface: self.interface,
                })
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
            pub fn signal_end_of_input(&self) {
                let msg = Message::EndOfInput;
                let bytes = msg.to_bytes();
                self.queue.push(bytes);
            }
            /// Writes a termination signal to the output, which will permanently neuter this connection, and all further operations on this wire
            /// will fail.
            ///
            /// This does a similar thing to [`signal_termination`], except that this also requires flushing, and it can be more useful if `wire.open`
            /// has consumed the writer handle. However, if you need to send a termination signal when your program shuts down after any errors,
            /// [`signal_termination`] should be preferred.
            pub fn signal_termination(&self) {
                let msg = Message::Termination;
                let bytes = msg.to_bytes();
                self.queue.push(bytes);
            }
            /// Sets the internal termination flag to `true`, causing all subsequent operations to fail.
            fn mark_terminated(&self) {
                // We use `Ordering::Release` to make sure that all subsequent reads see this
                self.terminated.store(true, Ordering::Release);
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
                    // Expressly ignorre general messages if we're a module
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
                                // We need to know where to put argument information
                                let call_buf_idx = self
                                    .interface
                                    .get_call_buffer(procedure_idx, call_idx, self.id)$(.$await)?;
                                // And now put it there, accumulating across many messages as necessary
                                self.interface.send_many(&bytes, call_buf_idx)$(.$await)??;

                                // If this message was the last in its series, we should call the procedure, assuming we have all the arguments
                                // we need
                                if terminator == Terminator::Complete {
                                    self.interface.terminate_message(call_buf_idx)$(.$await)??;

                                    // This will actually execute!
                                    // Note that this will remove the `call_buf_idx` mapping and drain the arguments out of that buffer
                                    let self_queue = self.queue.clone();
                                    let ret = self
                                        .interface
                                        .call_procedure(procedure_idx, call_idx, self.id, move |bytes, terminator| {
                                            // TODO Need a way for this to send errors too...(third argument?)
                                            let ret_msg = Message::Response {
                                                procedure_idx,
                                                call_idx,
                                                message: &bytes,
                                                terminator,
                                            };
                                            let ret_msg_bytes = ret_msg.to_bytes();
                                            self_queue.push(ret_msg_bytes);
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
                        self.mark_terminated();

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

                self.signal_end_of_input();
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
                            self.mark_terminated();
                            break Err(Error::IoError(err));
                        }
                        // Any unexpected errors should be propagated
                        Err(err) => break Err(err),
                    }
                }
            }
        }
        // Dropping the wire must also relinquish the unique wire identifier
        impl Drop for Wire<'_> {
            fn drop(&mut self) {
                self.interface.relinquish_id(self.id);
            }
        }

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
        }
        impl<'a> CallHandle<'a> {
            /// Waits for the procedure call to complete and returns the result.
            #[cfg(feature = "serde")]
            #[inline]
            pub $($async)? fn wait<T: DeserializeOwned>(&self) -> Result<T, Error> {
                self.interface.get(self.response_idx)$(.$await)?
            }
            /// Same as `.wait()`, but this will use chunk-based deserialisation, and is intended for use with procedures that send
            /// sequences of discrete values gradually over the wire. For more information, see [`Interface::get_chunks`].
            #[cfg(feature = "serde")]
            #[inline]
            pub $($async)? fn wait_chunks<T: DeserializeOwned>(&self) -> Result<Option<Vec<T>>, Error> {
                self.interface.get_chunks(self.response_idx)$(.$await)?
            }
            /// Creates a [`ChunkReceiver`] for incrementally receiving each chunk as it arrives. This can be used for accessing
            /// data in a semi-realtime way while still abstracting over the issue of partial chunks. This will wait until the
            /// first response data is received before creating a channel.
            #[inline]
            pub $($async)? fn wait_chunk_stream(&self) -> ChunkReceiver {
                self.interface.get_chunk_stream(self.response_idx)$(.$await)?
            }
            /// Same as `.wait()`, but this works directly with bytes. This will return the chunks in the message (to learn more about
            /// chunks, see [`Interface`]).
            #[inline]
            pub $($async)? fn wait_bytes(&self) -> Vec<Vec<u8>> {
                self.interface.get_raw(self.response_idx)$(.$await)?
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

            let result = handle.unwrap().wait_bytes()$(.$await)?;
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

            let result = handle.wait_bytes()$(.$await)?;
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

            let result = handle.wait_bytes()$(.$await)?;
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

            let result = handle.unwrap().wait_bytes()$(.$await)?;
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

            let result = handle.unwrap().wait_bytes()$(.$await)?;
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
    };
}
