use crate::IpfiInteger;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[cfg(feature = "serde")]
    #[error(transparent)]
    Encode(#[from] rmp_serde::encode::Error),
    #[cfg(feature = "serde")]
    #[error(transparent)]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("the message at local index {index} has already been marked as completed (no further data may be sent to it)")]
    AlreadyCompleted { index: IpfiInteger },
    #[error("the local message index {index} does not currently exist (but it may in future)")]
    OutOfBounds {
        /// The index that was out-of-bounds.
        index: IpfiInteger,
    },
    #[error("attempted to retrieve local response buffer for a procedure call (with remote procedure index {index} and local call index {call_idx}) that has not yet been started")]
    NoResponseBuffer {
        index: IpfiInteger,
        call_idx: IpfiInteger,
    },
    #[error("attempted to send zero-length payload in non-terminating function (sending zero-length payloads would be interpreted by IPFI as a message termination directive)")]
    ZeroLengthInNonTerminating,
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("no procedure has been registered with index {index}")]
    NoSuchProcedure { index: IpfiInteger },
    #[error("no local message buffer exists with index {index} for provided procedure call (attempted to call procedure before argument preparation)")]
    NoCallBuffer { index: IpfiInteger },
    #[error("procedure call buffer with index {index} is incomplete (attempted to call procedure before argument reception was complete)")]
    CallBufferIncomplete { index: IpfiInteger },
    #[error("procedure call buffer with index {index} has been poisoned (attempted to call procedure with argments in an invalid state)")]
    CallBufferPoisoned { index: IpfiInteger },
    // This should basically never occur
    #[error("couldn't write length marker for accumulated arguments (likely spontaneous failure)")]
    WriteLenMarkerFailed,
    #[error("this wire has been terminated from the other side, and operations can no longer be performed")]
    WireTerminated,
    #[error("attempted to use wire created with `Wire::new_module()` to call a procedure, which is disabled (module-style wires can only respond to procedure calls, not make their own)")]
    CallFromModule,
    #[error("read an index that didn't fit into local integer type (perhaps switch to a higher `int-*` feature?)")]
    IdxTooBig,
    #[error("read a non-compliant terminator `01` (valid values are `00`, `10`, and `11`)")]
    InvalidTerminator,
    #[error("found a poisoned creation/completion lock when trying to wait on the message with index {index} (it is unlikely that this message will ever be completed, but it is possible in rare cases)")]
    LockPoisoned { index: IpfiInteger },
}
