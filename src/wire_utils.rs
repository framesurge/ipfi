use crate::{integer::{Integer, get_as_smallest_int, get_as_smallest_int_from_usize}, ProcedureIndex, CallIndex};

/// Creates a one-byte multiflag containing metadata about a message.
pub(crate) fn make_multiflag(
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

pub(crate) fn bool_array_to_u8(arr: &[bool]) -> u8 {
    let mut result: u8 = 0;
    for (i, &bit) in arr.iter().enumerate() {
        if bit {
            result |= 1 << i;
        }
    }
    result
}
pub(crate) fn u8_to_bool_array(value: u8) -> [bool; 8] {
    let mut result: [bool; 8] = [false; 8];
    for (i, item) in result.iter_mut().enumerate() {
        *item = (value >> i) & 1 != 0;
    }
    result
}

/// A typed representation of a message to be sent down the wire, which can be transformed into a byte vector
/// easily.
pub(crate) enum Message<'b> {
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
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        match &self {
            Self::Termination => {
                buf.push(0);
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
                    Self::Call { .. } => buf.push(1),
                    Self::Response { .. } => buf.push(2),
                    _ => unreachable!(),
                };

                let procedure_idx = get_as_smallest_int(procedure_idx.0);
                let call_idx = get_as_smallest_int(call_idx.0);
                let num_bytes = get_as_smallest_int_from_usize(message.len());

                // Step 1
                let multiflag = make_multiflag(&procedure_idx, &call_idx, &num_bytes, *terminator);
                buf.extend(&[multiflag]);
                // Step 2
                let procedure_idx = procedure_idx.to_le_bytes();
                buf.extend(&procedure_idx);
                // Step 3
                let call_idx = call_idx.to_le_bytes();
                buf.extend(&call_idx);
                // Step 4
                let num_bytes = num_bytes.to_le_bytes();
                buf.extend(&num_bytes);
                // Step 5 (only bother if it's not empty)
                if !message.is_empty() {
                    buf.extend(*message);
                }
            }
            Self::EndOfInput => {
                buf.push(3);
            }
        }

        buf
    }
}
