use serde::{de::DeserializeOwned, Serialize};

use crate::{interface::Interface, procedure_args::ProcedureArgs, error::Error};
use std::{io::{self, Read, Write}, sync::{Arc, Mutex}, collections::HashMap};

/// A mechanism to interact ergonomically with an interface using synchronous Rust I/O buffers.
///
/// This wire sets up an internal message queue of data needing to be sent, allowing bidirectional messaging even over locked buffers (such as stdio)
/// without leading to race conditions.
///
/// # Procedure calls
///
/// ## Call indices
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
pub struct Wire<'a> {
    /// The unique identifier the attached interface has assigned to this wire.
    id: usize,
    /// The interface to interact with.
    interface: &'a Interface,
    /// An internal message queue, used for preventing interleaved writing, where part of one message is sent, and then part of another, due
    /// to race conditions. This would be fine if the headers were kept intact, although race conditions are rarely so courteous.
    queue: Arc<Mutex<Vec<Vec<u8>>>>,
    /// A map that keeps track of how many times each remote procedure has been called, allowing call indices to be intelligently
    /// and largely internally handled.
    remote_call_counter: Arc<Mutex<HashMap<usize, usize>>>,
    /// A map of procedure and call indices (respectively) to local response buffer indices. Once an entry is added here, it should never be changed.
    response_idx_map: Arc<Mutex<HashMap<(usize, usize), usize>>>,
}
impl<'a> Wire<'a> {
    /// Creates a new buffer-based wire to work with the given interface.
    pub fn new(interface: &'a Interface) -> Self {
        Self {
            id: interface.get_id(),
            interface,
            queue: Arc::new(Mutex::new(Vec::new())),
            remote_call_counter: Arc::new(Mutex::new(HashMap::new())),
            response_idx_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    // /// Creates a new buffer-based wire from the given interface, using stdin as an input and stdout as an output. Generally,
    // /// this should be used by modules executed as child processes by the hosts they want to communicate with.
    // ///
    // /// **WARNING:** This will lock both stdin and stdout, preventing any other operations from using them until this
    // /// wire is dropped! After calling this, make sure to output any logs or the like to stderr, or your application will
    // /// hang.
    // pub fn from_stdio(interface: &'a Interface) -> Self {
    //     let stdin = std::io::stdin().lock();
    //     let stdout = std::io::stdout().lock();

    //     Self::new(stdin, stdout, interface)
    // }

    /// Calls the procedure with the given remote procedure index. This will return a handle you can use to block waiting
    /// for the return value of the procedure.
    ///
    /// Generally, this should be preferred as a high-level method, although several lower-level methods are available for
    /// sending one argument at a time, or similar piecemeal use-cases.
    pub fn call(&self, procedure_idx: usize, args: impl ProcedureArgs) -> Result<CallHandle, Error> {
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
    pub fn start_call_with_partial_args(&self, procedure_idx: usize, args: impl ProcedureArgs) -> Result<usize, Error> {
        let args = args.into_bytes()?;
        self.start_call_with_partial_bytes(procedure_idx, &args)
    }
    /// Same as `.start_call_with_partial_args()`, but this works directly with bytes, allowing you to send strange things
    /// like a two-thirds of an argument.
    ///
    /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
    pub fn start_call_with_partial_bytes(&self, procedure_idx: usize, args: &[u8]) -> Result<usize, Error> {
        if args.len() == 0 {
            return Err(Error::ZeroLengthInNonTerminating);
        }

        let mut rcc = self.remote_call_counter.lock().unwrap();
        // Create a new entry in the remote call counter for this call index
        let call_idx = if let Some(last_call_idx) = rcc.get(&procedure_idx).as_mut() {
            // We need to update what's in there
            let new_call_idx = *last_call_idx + 1;
            *last_call_idx = &new_call_idx;
            new_call_idx
        } else {
            // We need to insert a new entry
            rcc.insert(procedure_idx, 0);
            0
        };
        // Allocate a new message buffer on the interface that we'll receive the response into
        let response_idx = self.interface.push();
        // Add that to the remote index map so we can retrieve it for later continutation and termination
        // of this call
        {
            let mut rim = self.response_idx_map.lock().unwrap();
            rim.insert((procedure_idx, call_idx), response_idx);
        }

        // This doesn't need to access the remote call counter, so we can leave it be safely
        self.continue_given_call_with_bytes(procedure_idx, call_idx, args)?;

        Ok(call_idx)
    }
    /// Same as `.call()`, but this works directly with bytes. You must be careful to provide the full byte serialization here,
    /// and be sure to follow the above guidance on this! (I.e. you must not include the length marker.)
    ///
    /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
    pub fn call_with_bytes(&self, procedure_idx: usize, args: &[u8]) -> Result<CallHandle, Error> {
        // We allow zero-length payloads here (for functions with no arguments in particular)
        let call_idx = self.start_call_with_partial_bytes(procedure_idx, args)?;
        self.end_given_call(procedure_idx, call_idx)
    }
    /// Continues the procedure call with the given remote procedure index and call index by sending the given arguments.
    /// This will not terminate the message, and will leave it open for calling.
    ///
    /// For an explanation of how call indices work, see [`Wire`].
    ///
    /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
    pub fn continue_given_call_with_args(&self, procedure_idx: usize, call_idx: usize, args: impl ProcedureArgs) -> Result<(), Error> {
        let args = args.into_bytes()?;
        self.continue_given_call_with_bytes(procedure_idx, call_idx, &args)
    }
    /// Same as `.continue_given_call_with_args()`, but this works directly with bytes.
    ///
    /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
    pub fn continue_given_call_with_bytes(&self, procedure_idx: usize, call_idx: usize, args: &[u8]) -> Result<(), Error> {
        if args.len() == 0 {
            return Err(Error::ZeroLengthInNonTerminating);
        }

        // Get the response index we're using
        let response_idx = self.get_response_idx(procedure_idx, call_idx)?;
        // Construct the message we want to send
        let msg = Message::Call {
            procedure_idx,
            call_idx,
            response_idx,
            args,
        };
        // Convert that message into bytes and place it in the queue
        let bytes = msg.to_bytes()?;
        self.queue.lock().unwrap().push(bytes);

        Ok(())
    }
    /// Terminates the given call by sending a zero-length argument payload.
    ///
    /// This will return a handle the caller can use to wait on the return value of the remote procedure.
    ///
    /// This is one of several low-level procedure calling methods, and you probably want to use `.call()` instead.
    pub fn end_given_call(&self, procedure_idx: usize, call_idx: usize) -> Result<CallHandle, Error> {
        // Get the response index we're using
        let response_idx = self.get_response_idx(procedure_idx, call_idx)?;
        // Construct the zero-length payload message we want to send
        let msg = Message::Call {
            procedure_idx,
            call_idx,
            response_idx,
            args: &[],
        };
        // Convert that message into bytes and place it in the queue
        let bytes = msg.to_bytes()?;
        self.queue.lock().unwrap().push(bytes);

        Ok(CallHandle {
            response_idx,
            interface: self.interface,
        })
    }
    /// Gets the local response buffer index for the given procedure and call indices. If no such buffer has been allocated,
    /// this will return an error.
    #[inline]
    fn get_response_idx(&self, procedure_idx: usize, call_idx: usize) -> Result<usize, Error> {
        let rim = self.response_idx_map.lock().unwrap();
        rim.get(&(procedure_idx, call_idx)).cloned().ok_or(Error::NoResponseBuffer { index: procedure_idx, call_idx })
    }

    /// Sends the given raw bytes over the wire, with the given message index, which will correspond to that index in the
    /// interface the other side maintains (it has no relation to our own interface!).
    ///
    /// This will not send a termination signal.
    pub fn send_bytes(&self, bytes: &[u8], message_idx: usize) -> Result<(), Error> {
        let msg = Message::General {
            message_idx,
            message: bytes,
        };
        let bytes = msg.to_bytes()?;
        self.queue.lock().unwrap().push(bytes);

        Ok(())
    }
    /// Sends the given full message over the wire to the given remote message buffer index.
    pub fn send_full_message<T: Serialize>(&self, msg: &T, message_idx: usize) -> Result<(), Error> {
        let bytes = rmp_serde::to_vec(msg)?;
        self.send_bytes(&bytes, message_idx)?;
        self.end_message(message_idx)
    }
    /// Sends a termination signal over the wire for the given message index. After this is called, further bytes will
    /// not be able to be sent for this message.
    pub fn end_message(&self, message_idx: usize) -> Result<(), Error> {
        let msg = Message::General {
            message_idx,
            message: &[],
        };
        let bytes = msg.to_bytes()?;
        self.queue.lock().unwrap().push(bytes);

        Ok(())
    }
    /// Receives a single message from the given reader, sending it into the interface as appropriate. This contains
    /// the core logic that handles messages, partial procedure calls, etc.
    ///
    /// If a procedure call is completed in this read, this method will automatically block waiting for the response,
    /// and it will followingly add said response to the internal writer queue.
    pub fn receive_one(&self, reader: &mut impl Read) -> Result<(), Error> {
        // First is the type of message
        let mut ty_buf = [0u8];
        reader.read_exact(&mut ty_buf)?;
        let ty = u8::from_le_bytes(ty_buf);
        match ty {
            // Procedure call
            1 => {
                // First is the procedure index
                let mut procedure_buf = [0u8; 4];
                reader.read_exact(&mut procedure_buf)?;
                let procedure_idx = u32::from_le_bytes(procedure_buf) as usize;

                // Second is the call index
                let mut call_buf = [0u8; 4];
                reader.read_exact(&mut call_buf)?;
                let call_idx = u32::from_le_bytes(call_buf) as usize;

                // Third is the message index *on the caller* we'll send the response to
                let mut response_buf = [0u8; 4];
                reader.read_exact(&mut response_buf)?;
                let response_idx = u32::from_le_bytes(response_buf) as usize;

                // Then the number of bytes to expect
                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf)?;
                let num_bytes = u32::from_le_bytes(len_buf) as usize;

                // We need to know where to put argument information
                let call_buf_idx = self.interface.get_call_buffer(procedure_idx, call_idx, self.id);

                // If there were no arguments, we should call the procedure (we either allegedly have everything, or
                // the procedure takes no arguments in the first place)
                if num_bytes == 0 {
                    self.interface.terminate_message(call_buf_idx)?;

                    // This will actually execute!
                    let ret = self.interface.call_procedure(procedure_idx, call_buf_idx)?;
                    let ret_msg = Message::General {
                        message_idx: response_idx,
                        message: &ret
                    };
                    let ret_msg_bytes = ret_msg.to_bytes()?;
                    let mut queue = self.queue.lock().unwrap();
                    queue.push(ret_msg_bytes);
                    // And now we need to terminate that result message
                    queue.push(Message::General {
                        message_idx: response_idx,
                        message: &[]
                    }.to_bytes()?);
                } else {
                    // We're continuing or starting a new partial procedure argument addition, so get a local message
                    // buffer for it if there isn't already one

                    let mut bytes = vec![0u8; num_bytes];
                    reader.read_exact(&mut bytes)?;

                    // This will accumulate argument bytes over potentially many continuations in the local call buffer
                    self.interface.send_many(&bytes, call_buf_idx)?;
                }
            },
            // General message
            2 => {
                // First is the message index
                let mut idx_buf = [0u8; 4];
                reader.read_exact(&mut idx_buf)?;
                let message_idx = u32::from_le_bytes(idx_buf) as usize;

                // Then the number of bytes to expect
                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf)?;
                let num_bytes = u32::from_le_bytes(len_buf) as usize;

                // If that's zero, we should end the message
                if num_bytes == 0 {
                    self.interface.terminate_message(message_idx)?;
                } else {
                    let mut bytes = vec![0u8; num_bytes];
                    reader.read_exact(&mut bytes)?;

                    self.interface.send_many(&bytes, message_idx)?;
                }
            },
            // Termination: the other program is shutting down and is no longer capable of receiving messages
            // This is the IPFI equivalent of 'expected EOF'
            0 => {
                todo!()
            },
            // Unknown message types will be ignored
            _ => {}
        }

        Ok(())
    }

    /// Writes all messages currently in the internal write queue to the given output stream.
    pub fn flush(&self, writer: &mut impl Write) -> Result<(), std::io::Error> {
        // This will remove the written messages from the queue
        for msg_bytes in self.queue.lock().unwrap().drain(..) {
            writer.write_all(&msg_bytes)?;
        }
        writer.flush()?;

        Ok(())
    }
    // TODO
    pub fn fill(&self) -> Result<(), std::io::Error> {
        todo!()
    }
}

/// A handle for waiting on the return values of remote procedure calls. This is necessary because calling a remote procedure
/// requires `.flush()` to be called, so waiting inside a function like `.call()` would make very little sense.
pub struct CallHandle<'a> {
    /// The message index to wait for. If this is improperly initialised, we will probably get completely different and almost
    /// certainly invalid data.
    response_idx: usize,
    /// The interface where the response will appear.
    interface: &'a Interface,
}
impl<'a> CallHandle<'a> {
    /// Waits for the procedure call to complete and returns the result. This will block.
    #[inline]
    pub fn wait<T: DeserializeOwned>(&self) -> Result<T, Error> {
        self.interface.get(self.response_idx)
    }
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
        procedure_idx: usize, // Remote
        call_idx: usize, // Remote
        response_idx: usize, // Local

        args: &'b [u8],
    },
    /// A general message that knows where it is heading. This is typically used for responses to procedure
    /// calls, but it can also be used for other, user-defined operations if necessary.
    ///
    /// If the message here is an empty vector, this will terminate the given message index.
    General {
        message_idx: usize, // Remote

        message: &'b [u8],
    },
}
impl<'b> Message<'b> {
    /// Writes the message in byte form to the given writer, according to the IPFI Binary Format (ipfiBuF).
    ///
    /// First, a single byte indicating the message type is sent.
    ///
    /// ## Termination messages (type 0)
    /// No data is sent, these act as an indication that the program is now terminating.
    ///
    /// ## Procedure call messages (type 1)
    /// 1. Procedure index (u32 in LE byte order)
    /// 2. Call index (u32 in LE byte order)
    /// 3. Index of local message buffer that response should be sent to (u32 in LE byte order)
    /// 4. Number of message bytes to expect (u32 in LE byte order)
    /// 5. Raw message bytes
    ///
    /// It is expected that a new message buffer will be allocated for each call index of a procedure, allowing
    /// subsequent call messages that complete this call to be added to the correct buffer, without the caller
    /// knowing the index of that buffer (the receiver should hold an internal mapping of call indices to
    /// buffer indices).
    ///
    /// As with general messages, if step 4 transmitted length zero, the given call index should be terminated,
    /// and the procedure called.
    ///
    /// ## General messages (type 2)
    /// 1. Message index (u32 in LE byte order)
    /// 2. Number of message bytes to expect (u32 in LE byte order)
    /// 3. Raw message bytes
    ///
    /// If step 2 transmitted length zero, the given message index should be terminated.
    fn to_bytes(&self) -> Result<Vec<u8>, std::io::Error> {
        let mut buf = Vec::new();

        match &self {
            Self::Termination => {
                buf.write_all(&[0])?;
            },
            Self::Call { procedure_idx, call_idx, response_idx, args } => {
                buf.write_all(&[1])?;
                // Step 1
                let procedure_idx = (*procedure_idx as u32).to_le_bytes();
                buf.write_all(&procedure_idx)?;
                // Step 2
                let call_idx = (*call_idx as u32).to_le_bytes();
                buf.write_all(&call_idx)?;
                // Step 3
                let response_idx = (*response_idx as u32).to_le_bytes();
                buf.write_all(&response_idx)?;
                // Step 4
                let num_bytes = args.len() as u32;
                let num_bytes = num_bytes.to_le_bytes();
                buf.write_all(&num_bytes)?;
                // Step 5 (only bother if we're not terminating)
                if args.len() > 0 {
                    buf.write_all(args)?;
                }
            },
            Self::General { message_idx, message } => {
                buf.write_all(&[2])?;
                // Step 1
                let message_idx = (*message_idx as u32).to_le_bytes();
                buf.write_all(&message_idx)?;
                // Step 2
                let num_bytes = message.len() as u32;
                let num_bytes = num_bytes.to_le_bytes();
                buf.write_all(&num_bytes)?;
                // Step 3 (only bother if we're not terminating)
                if message.len() > 0 {
                    buf.write_all(message)?;
                }
            }
        }

        Ok(buf)
    }
}

/// Signals the termination of this program to the given output stream. In programs that principally communicate with
/// other programs through IPFI, this should be sent when the program terminates, after both the interface and wire
/// have been cleaned up, in order to ensure that other programs do not attempt to send further messages to this one.
///
/// Typically, this will be called in the cleanup of module processes spawned by some host, and stdout will be the
/// chosen buf.
pub fn signal_termination(writer: &mut impl Write) -> Result<(), std::io::Error> {
    writer.write_all(&[0])?;
    writer.flush()?;

    Ok(())
}
