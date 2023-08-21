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
