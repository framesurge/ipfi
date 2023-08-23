mod error;
#[cfg(feature = "wire")]
mod integer;
mod interface_common;
#[cfg(feature = "serde")]
mod procedure_args;
mod roi_queue;
mod wire_common;
#[cfg(feature = "wire")]
mod wire_utils;

#[cfg(feature = "async")]
mod r#async;
/// A blocking version of IPFI. This supports extreme concurrency, however, and may be preferred in many cases.
///
/// Note that the only difference between the blocking and asynchronous APIs is their use of asynchronous read/write
/// types (in the wire), and an alternate, Tokio-based implementation of the internal completion lock in the asynchronous
/// API. Otherwise, both implementations use the same atomic and concurrent primitives, and there is little performance
/// difference between the two: choose the flavour you want based on what kind of existing system you want to integrate with.
/// If you're not already using a Tokio runtime, you probably don't need the asynchronous API.
#[cfg(feature = "blocking")]
pub mod blocking;

// We use async by default
#[cfg(feature = "async")]
pub use r#async::*;

// --- Internal integer type control ---
pub use int::*;

#[cfg(feature = "int-u8")]
mod int {
    pub type IpfiInteger = u8;
    pub type IpfiAtomicInteger = std::sync::atomic::AtomicU8;
}
#[cfg(feature = "int-u16")]
mod int {
    pub type IpfiInteger = u16;
    pub type IpfiAtomicInteger = std::sync::atomic::AtomicU16;
}
#[cfg(feature = "int-u32")]
mod int {
    pub type IpfiInteger = u32;
    pub type IpfiAtomicInteger = std::sync::atomic::AtomicU32;
}
// Only allow u64s on 64-bit platforms, otherwise `as usize` conversions could panic
#[cfg(all(feature = "int-u64", target_pointer_width = "64"))]
mod int {
    pub type IpfiInteger = u64;
    pub type IpfiAtomicInteger = std::sync::atomic::AtomicU64;
}
#[cfg(all(feature = "int-u64", target_pointer_width = "32"))]
compile_error!("attempted to use 64-bit integers on a 32-bit platform, which may lead to panics; do you really need to send over {} messages?", u32::MAX + 1);

// The following are newtype wrappers that can deliberately only be constructed internally to avoid confusion.
// This also allows us to keep the internals as implementation details, and to change them without a breaking
// change if necessary.
/// A call index. This is its own type to avoid confusion.
///
/// For information about call indices, see [`Wire`].
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct CallIndex(pub(crate) IpfiInteger);
/// A procedure index. This is its own type to avoid confusion.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct ProcedureIndex(pub(crate) IpfiInteger);
// Users need to be able to construct these manually
impl ProcedureIndex {
    /// Creates a new procedure index as given.
    pub fn new(idx: IpfiInteger) -> Self {
        Self(idx)
    }
}
/// An identifier for a wire.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct WireId(pub(crate) IpfiInteger);

/// The status each call/response message holds about how it is terminating its
/// series of messages. Generally, this would be a boolean (`Terminator::Complete`
/// or `Terminator::None`), but `Terminator::Chunk` is introduced to allow for
/// procedures that stream discrete values, which must later be deserialised, which
/// requires knowing how many of them there are. Explicit chunk marking allows for
/// both flexible operation and simultaneously clear typing.
///
/// This is exposed to end-users because, sometimes, one may wish to write a streaming
/// procedure that sends partial chunks, in which case granular control can be provided
/// through manually setting this on each call to the yielder closure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    Complete,
    Chunk,
    None,
}

/// The state of a completion lock.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub(crate) enum CompleteLockState {
    /// Still waiting for someone to mark this lock as complete.
    Incomplete,
    /// The lock has been marked as complete.
    Complete,
    /// The lock has been poisoned: whatever it was guarding is almost certainly in
    /// an invalid state.
    Poisoned,
}
