mod complete_lock;
mod interface;
#[cfg(feature = "wire")]
mod wire;

pub use interface::{ChunkReceiver, Interface};
#[cfg(feature = "wire")]
pub use wire::{signal_termination, AutonomousWireHandle, CallHandle, Wire, ChunkReceiverHandle};

// Re-export these to avoid the user having to use two lines of imports for using the blocking API
pub use crate::{CallIndex, ProcedureIndex, WireId};
