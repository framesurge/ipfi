mod complete_lock;
mod interface;
#[cfg(feature = "wire")]
mod wire;

pub use interface::Interface;
#[cfg(feature = "wire")]
pub use wire::{signal_termination, AutonomousWireHandle, CallHandle, Wire};
