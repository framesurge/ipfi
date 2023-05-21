mod complete_lock;
mod error;
#[cfg(feature = "wire")]
mod integer;
mod interface;
#[cfg(feature = "serde")]
mod procedure_args;
mod roi_queue;
#[cfg(feature = "wire")]
mod wire;

pub use crate::interface::{CallIndex, Interface, ProcedureIndex, WireId};
#[cfg(feature = "wire")]
pub use crate::wire::{signal_termination, AutonomousWireHandle, CallHandle, Wire};

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
