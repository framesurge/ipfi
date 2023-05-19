use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Encode(#[from] rmp_serde::encode::Error),
    #[error(transparent)]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("the message at local index {index} has already been marked as completed (no further data may be sent to it)")]
    AlreadyCompleted { index: usize },
    #[error("the local message index {index} does not currently exist (but it may in future)")]
    OutOfBounds {
        /// The index that was out-of-bounds.
        index: usize,
    },
    #[error("attempted to retrieve local response buffer for a procedure call (with remote procedure index {index} and local call index {call_idx}) that has not yet been started")]
    NoResponseBuffer { index: usize, call_idx: usize },
    #[error("attempted to send zero-length payload in non-terminating function (sending zero-length payloads would be interpreted by IPFI as a message termination directive)")]
    ZeroLengthInNonTerminating,
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("no procedure has been registered with index {index}")]
    NoSuchProcedure { index: usize },
    #[error("no local message buffer exists with index {index} for provided procedure call (attempted to call procedure before argument preparation)")]
    NoCallBuffer { index: usize },
    #[error("procedure call buffer with index {index} is incomplete (attempted to call procedure before argument reception was complete)")]
    CallBufferIncomplete { index: usize },
    // This should basically never occur
    #[error("couldn't write length marker for accumulated arguments (likely spontaneous failure)")]
    WriteLenMarkerFailed,
    #[error("this wire has been terminated from the other side, and operations can no longer be performed")]
    WireTerminated,
}
