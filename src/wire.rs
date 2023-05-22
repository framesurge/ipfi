use crate::integer::*;
#[cfg(feature = "serde")]
use crate::procedure_args::ProcedureArgs;
use crate::{
    error::Error,
    interface::{CallIndex, Interface, ProcedureIndex, WireId},
    IpfiInteger,
};
use crossbeam_queue::SegQueue;
use dashmap::DashMap;
#[cfg(feature = "serde")]
use serde::de::DeserializeOwned;
use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

/// A mechanism to interact ergonomically with an interface using synchronous Rust I/O buffers.
///
/// This wire sets up an internal message queue of data needing to be sent, allowing bidirectional messaging even over locked buffers (such as stdio)
/// without leading to race conditions.
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
    remote_call_counter: Arc<DashMap<ProcedureIndex, IpfiInteger>>,
    /// A map of procedure and call indices (respectively) to local response buffer indices. Once an entry is added here, it should never be changed until
    /// it is removed.
    ///
    /// This serves as a secuirty mechanism to ensure that response messages are mapped *locally* to response buffer indices, which means we can be sure
    /// that a remote cannot access and control arbitrary message buffers on our local interface, thereby compromising other wires.
    response_idx_map: Arc<DashMap<(ProcedureIndex, CallIndex), IpfiInteger>>,
    /// A flag for whether or not this wire has been terminated. Once it has been, *all* further operations will fail.
    terminated: Arc<AtomicBool>,
}
#[cfg(not(target_arch = "wasm32"))]
impl Wire<'static> {
    /// Starts an autonomous version of the wire by starting two new threads, one for reading and one for writing. If you call this,
    /// it is superfluous to call `.fill()`/`.flush()`, as they will be automatically called from here on.
    ///
    /// This method is only available when a `'static` reference to the [`Interface`] is held, since only that can be passed safely between
    /// threads. You must also own both the reader and writer in order to use this method (which typically means this method must hold
    /// those two exclusively).
    pub fn start(
        &self,
        mut reader: impl Read + Send + Sync + 'static,
        mut writer: impl Write + Send + Sync + 'static,
    ) -> AutonomousWireHandle {
        let self_reader = self.clone();
        let reader = std::thread::spawn(move || while self_reader.fill(&mut reader).is_ok() {});
        let self_writer = self.clone();
        let writer = std::thread::spawn(move || {
            // TODO Spinning...
            while self_writer.flush(&mut writer).is_ok() {
                std::hint::spin_loop();
            }
        });

        AutonomousWireHandle { reader, writer }
    }
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
            remote_call_counter: Arc::new(DashMap::new()),
            response_idx_map: Arc::new(DashMap::new()),

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
            remote_call_counter: Arc::new(DashMap::new()),
            response_idx_map: Arc::new(DashMap::new()),

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
    pub fn call(
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
        self.call_with_bytes(procedure_idx, &args)
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
    pub fn start_call_with_partial_args(
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
        self.start_call_with_partial_bytes(procedure_idx, &args)
    }
    /// Same as `.start_call_with_partial_args()`, but this works directly with bytes, allowing you to send strange things
    /// like a two-thirds of an argument.
    ///
    /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
    pub fn start_call_with_partial_bytes(
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
        let response_idx = self.interface.push();
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
    pub fn call_with_bytes(
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
        let response_idx = self.interface.push();
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
            terminator: true,
        };
        // Convert that message into bytes and place it in the queue
        let bytes = msg.to_bytes()?;
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
            terminator: false,
        };
        // Convert that message into bytes and place it in the queue
        let bytes = msg.to_bytes()?;
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
            terminator: true,
        };
        // Convert that message into bytes and place it in the queue
        let bytes = msg.to_bytes()?;
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

    /// Receives a single message from the given reader, sending it into the interface as appropriate. This contains
    /// the core logic that handles messages, partial procedure calls, etc.
    ///
    /// If a procedure call is completed in this read, this method will automatically block waiting for the response,
    /// and it will followingly add said response to the internal writer queue.
    ///
    /// This returns whether or not it read a message/call termination message). Alternately, `None` will be returned
    /// if there was a manual end of input message, or on a wire termination.
    pub fn receive_one(&self, reader: &mut impl Read) -> Result<Option<bool>, Error> {
        if self.is_terminated() {
            return Err(Error::WireTerminated);
        }

        // First is the type of message
        let mut ty_buf = [0u8];
        reader.read_exact(&mut ty_buf)?;
        let ty = u8::from_le_bytes(ty_buf);
        match ty {
            // Expressly ignorre general messages if we're a module
            2 if self.module_style => Ok(Some(false)),
            // Procedure call or response thereto (same base format)
            1 | 2 => {
                // First is the multiflag
                let mut multiflag_buf = [0u8; 1];
                reader.read_exact(&mut multiflag_buf)?;
                let bits = u8_to_bool_array(multiflag_buf[0]);
                // The second-last bit represents whether or not this is the final message in its series
                let is_final = bits[6];

                // Then the procedure index
                let procedure_idx = Integer::from_flag((bits[0], bits[1]))
                    .populate_from_reader(reader)?
                    .into_int()
                    .ok_or(Error::IdxTooBig)?;
                let procedure_idx = ProcedureIndex(procedure_idx);
                // Then the call index
                let call_idx = Integer::from_flag((bits[2], bits[3]))
                    .populate_from_reader(reader)?
                    .into_int()
                    .ok_or(Error::IdxTooBig)?;
                let call_idx = CallIndex(call_idx);
                // Then the number of bytes to expect (this will only be too big if there's a pointer width disparity)
                let num_bytes = Integer::from_flag((bits[4], bits[5]))
                    .populate_from_reader(reader)?
                    .into_usize()
                    .ok_or(Error::IdxTooBig)?;

                // Read the number of bytes we expect
                let mut bytes = vec![0u8; num_bytes];
                reader.read_exact(&mut bytes)?;

                // Now diverge
                match ty {
                    // Call
                    1 => {
                        // We need to know where to put argument information
                        let call_buf_idx =
                            self.interface
                                .get_call_buffer(procedure_idx, call_idx, self.id);
                        // And now put it there, accumulating across many messages as necessary
                        self.interface.send_many(&bytes, call_buf_idx)?;

                        // If this message was the last in its series, we should call the procedure, assuming we have all the arguments
                        // we need
                        if is_final {
                            self.interface.terminate_message(call_buf_idx)?;

                            // This will actually execute!
                            // Note that this will remove the `call_buf_idx` mapping and drain the arguments out of that buffer
                            let ret =
                                self.interface
                                    .call_procedure(procedure_idx, call_idx, self.id)?;
                            let ret_msg = Message::Response {
                                procedure_idx,
                                call_idx,
                                message: &ret,
                                terminator: true,
                            };
                            let ret_msg_bytes = ret_msg.to_bytes()?;
                            self.queue.push(ret_msg_bytes);
                            Ok(Some(true))
                        } else {
                            Ok(Some(false))
                        }
                    }
                    // Response
                    2 => {
                        // Get the local message buffer index that we said we'd put the response into
                        let response_idx = self.get_response_idx(procedure_idx, call_idx)?;
                        self.interface.send_many(&bytes, response_idx)?;

                        // If this is marked as the last in its series, we should round off the response message and
                        // implicitly signal to any waiting call handles that it's ready to be read
                        if is_final {
                            self.interface.terminate_message(response_idx)?;
                            // If that succeeded, the user should be ready to fetch that, and the index will be reused
                            // after they have, so we should remove it from this map (this is to save space only, because we
                            // know we won't be querying this procedure call again)
                            self.response_idx_map.remove(&(procedure_idx, call_idx));

                            Ok(Some(true))
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
    pub fn flush(&self, writer: &mut impl Write) -> Result<(), Error> {
        if self.is_terminated() {
            return Err(Error::WireTerminated);
        }

        // This will remove the written messages from the queue
        while let Some(msg_bytes) = self.queue.pop() {
            writer.write_all(&msg_bytes)?;
        }
        writer.flush()?;

        Ok(())
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
    pub fn fill(&self, reader: &mut impl Read) -> Result<(), Error> {
        if self.is_terminated() {
            return Err(Error::WireTerminated);
        }

        // Read until end-of-input is sent
        loop {
            match self.receive_one(reader) {
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
    /// Writes a manual end-of-input signal to the output, which, when flushed (potentially automatically if you've called `wire.start()`),
    /// will cause any `wire.fill()` calls in the remote program to return `Ok(false)`, which can be checked for termination. This is
    /// necessary when communicating with single-threaded programs, which must read all their input at once, to tell them to stop reading
    /// and start doing other work. This does not signal the termination of the wire, or even that there will not be any input in future,
    /// it simply allows you to signal to the remote that it should start doing something else. Internally, the reception of this case is
    /// generally not handled, and it is up to you as a user to handle it.
    pub fn signal_end_of_input(&self) -> Result<(), std::io::Error> {
        let msg = Message::EndOfInput;
        let bytes = msg.to_bytes()?;
        self.queue.push(bytes);

        Ok(())
    }
    /// Writes a termination signal to the output, which will permanently neuter this connection, and all further operations on this wire
    /// will fail.
    ///
    /// Generally, [`signal_termination`] should be preferred by single-threaded programs, although multi-threaded programs will typically
    /// use `.start()`, which takes ownership of the writer that has to be used to signal termination. This method can be combined with
    /// `.start()` to avoid such pitfalls. In short, use this if you're using `wire.open()` as well, otherwise use [`signal_termination`].
    pub fn signal_termination(&self) -> Result<(), std::io::Error> {
        let msg = Message::Termination;
        let bytes = msg.to_bytes()?;
        self.queue.push(bytes);

        Ok(())
    }
    /// Sets the internal termination flag to `true`, causing all subsequent operations to fail.
    fn mark_terminated(&self) {
        // We use `Ordering::Release` to make sure that all subsequent reads see this
        self.terminated.store(true, Ordering::Release);
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
#[must_use]
pub struct CallHandle<'a> {
    /// The message index to wait for. If this is improperly initialised, we will probably get completely different and almost
    /// certainly invalid data.
    response_idx: IpfiInteger,
    /// The interface where the response will appear.
    interface: &'a Interface,
}
impl<'a> CallHandle<'a> {
    /// Waits for the procedure call to complete and returns the result. This will block.
    #[cfg(feature = "serde")]
    #[inline]
    pub fn wait<T: DeserializeOwned>(&self) -> Result<T, Error> {
        self.interface.get(self.response_idx)
    }
    /// Same as `.wait()`, but this works directly with bytes.
    #[inline]
    pub fn wait_bytes(&self) -> Vec<u8> {
        self.interface.get_raw(self.response_idx)
    }
}

/// A handle representing the reader/writer threads started by [`Wire::start`].
pub struct AutonomousWireHandle {
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}
impl AutonomousWireHandle {
    /// Waits for both threads to be done, which will occur once the wire is expressly terminated. Generally, this is not needed
    /// when communication patterns are predictable, although in host-module scenarios, the module should generally call this
    /// to wait until the host expressly terminates it.
    pub fn wait(self) {
        // We propagate any thread panics to the caller, there shouldn't be any
        self.reader.join().unwrap();
        self.writer.join().unwrap();
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
pub fn signal_termination(writer: &mut impl Write) -> Result<(), std::io::Error> {
    let msg = Message::Termination;
    let bytes = msg.to_bytes()?;
    writer.write_all(&bytes)?;

    Ok(())
}

/// A typed representation of a message to be sent down the wire, which can be transformed into a byte vector
/// easily.
enum Message<'b> {
    /// A message that indicates the sending program is about to terminate, and that all future messages will
    /// not be received.
    ///
    /// Generally, this will be written manually to a stream in a program's cleanup, when it no longer maintains
    /// a wire or interface of its own.
    Termination,
    /// A message that calls a procedure. This, like any other message, may have a partial payload.
    Call {
        procedure_idx: ProcedureIndex, // Remote
        call_idx: CallIndex,           // Remote
        terminator: bool,

        args: &'b [u8],
    },
    /// A response to a `Call` message that contains the return type. This is not necessarily self-contained, and
    /// requires an explicit termination signal, as procedures may stream data in the future.
    Response {
        // These properties are the same as we used in the `Call`, meaning the caller has no influence over our message
        // buffers whatsoever, allowing the `Wire` to act as a security layer over the `Interface`
        procedure_idx: ProcedureIndex,
        call_idx: CallIndex,
        terminator: bool,

        message: &'b [u8],
    },
    /// A message that indicates that the sender will not send any more data. This is different from termination
    /// in that it implies that the channel is still open for the sender to receive more data, this merely means
    /// that the sender will not send anything further. This message is irrevocable, and generally useless, except
    /// when communicating with a single-threaded program that has to read all its input at once.
    ///
    /// It is occasionally useful for such a program to read batches of input, in which case this could be used to
    /// signify the ends of batches. In essence, its meaning is case-dependent.
    EndOfInput,
}
impl<'b> Message<'b> {
    /// Writes the message in byte form to the given writer, according to the IPFI Binary Format (ipfiBuF). Where `IpfiInteger` is
    /// stated below, this is subject to detection of where a smaller payload size can be used.
    ///
    /// First, a single byte indicating the message type is sent.
    ///
    /// ## Termination messages (type 0)
    /// No data is sent, these act as an indication that the program is now terminating.
    ///
    /// ## Procedure call messages (type 1)
    /// 1. Multiflag (3 2-bit integers for sizings of next three steps, 1 bit for terminator status, final bit empty)
    /// 2. Procedure index
    /// 3. Call index
    /// 4. Number of message bytes to expect
    /// 5. Raw message bytes
    ///
    /// It is expected that a new message buffer will be allocated for each call index of a procedure, allowing
    /// subsequent call messages that complete this call to be added to the correct buffer, without the caller
    /// knowing the index of that buffer (the receiver should hold an internal mapping of call indices to
    /// buffer indices).
    ///
    /// As with response messages, if step 4 transmitted length zero, the given call index should be terminated,
    /// and the procedure called.
    ///
    /// ## Response messages (type 2)
    /// 1. Multiflag (3 2-bit integers for sizings of next three steps, 1 bit for terminator status, final bit empty)
    /// 2. Procedure index (IpfiInteger in LE byte order)
    /// 3. Call index (IpfiInteger in LE byte order)
    /// 4. Number of message bytes to expect (IpfiInteger in LE byte order)
    /// 5. Raw message bytes
    ///
    /// If step 2 transmitted length zero, the given message index should be terminated.
    fn to_bytes(&self) -> Result<Vec<u8>, std::io::Error> {
        let mut buf = Vec::new();

        match &self {
            Self::Termination => {
                buf.write_all(&[0])?;
            }
            Self::Call {
                procedure_idx,
                call_idx,
                args: message,
                terminator,
            }
            | Self::Response {
                procedure_idx,
                call_idx,
                message,
                terminator,
            } => {
                match &self {
                    Self::Call { .. } => buf.write_all(&[1])?,
                    Self::Response { .. } => buf.write_all(&[2])?,
                    _ => unreachable!(),
                };

                let procedure_idx = get_as_smallest_int(procedure_idx.0);
                let call_idx = get_as_smallest_int(call_idx.0);
                let num_bytes = get_as_smallest_int_from_usize(message.len());

                // Step 1
                let multiflag = make_multiflag(&procedure_idx, &call_idx, &num_bytes, *terminator);
                buf.write_all(&[multiflag])?;
                // Step 2
                let procedure_idx = procedure_idx.to_le_bytes();
                buf.write_all(&procedure_idx)?;
                // Step 3
                let call_idx = call_idx.to_le_bytes();
                buf.write_all(&call_idx)?;
                // Step 4
                let num_bytes = num_bytes.to_le_bytes();
                buf.write_all(&num_bytes)?;
                // Step 5 (only bother if it's not empty)
                if !message.is_empty() {
                    buf.write_all(message)?;
                }
            }
            Self::EndOfInput => {
                buf.write_all(&[3])?;
            }
        }

        Ok(buf)
    }
}

/// Creates a one-byte multiflag containing metadata about a message.
fn make_multiflag(
    procedure_idx: &Integer,
    call_idx: &Integer,
    num_bytes: &Integer,
    is_terminator: bool,
) -> u8 {
    let flag_1 = procedure_idx.to_flag();
    let flag_2 = call_idx.to_flag();
    let flag_3 = num_bytes.to_flag();

    let bool_arr = [
        flag_1.0,
        flag_1.1,
        flag_2.0,
        flag_2.1,
        flag_3.0,
        flag_3.1,
        is_terminator,
        false,
    ];
    bool_array_to_u8(&bool_arr)
}

fn bool_array_to_u8(arr: &[bool]) -> u8 {
    let mut result: u8 = 0;
    for (i, &bit) in arr.iter().enumerate() {
        if bit {
            result |= 1 << i;
        }
    }
    result
}
fn u8_to_bool_array(value: u8) -> [bool; 8] {
    let mut result: [bool; 8] = [false; 8];
    for (i, item) in result.iter_mut().enumerate() {
        *item = (value >> i) & 1 != 0;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::Wire;
    use crate::{Interface, IpfiInteger, ProcedureIndex};
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
        fn reset_input(&mut self) {
            self.input.set_position(0);
        }
    }
}
