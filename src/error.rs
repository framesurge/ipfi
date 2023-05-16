use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Encode(#[from] rmp_serde::encode::Error),
    #[error(transparent)]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("the message at local index {index} has already been marked as completed (no further data may be sent to it)")]
    AlreadyCompleted { index: usize },
    #[error("the local message index {index} is presently out-of-bounds, and more data must be sent before this buffer will become available (current maximum index is {max_idx})")]
    OutOfBounds {
        /// The index that was out-of-bounds.
        index: usize,
        /// The highest index in the interface (i.e. length - 1).
        max_idx: usize,
    },
    #[error("attempted to retrieve local response buffer for a procedure call (with remote procedure index {index} and local call index {call_idx}) that has not yet been started")]
    NoResponseBuffer {
        index: usize,
        call_idx: usize,
    },
    #[error("attempted to send zero-length payload in non-terminating function (sending zero-length payloads would be interpreted by IPFI as a message termination directive)")]
    ZeroLengthInNonTerminating,
    #[error(transparent)]
    IoError(#[from] std::io::Error)
}
