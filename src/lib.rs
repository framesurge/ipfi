mod complete_lock;
mod error;
mod interface;
#[cfg(feature = "serde")]
mod procedure_args;
mod roi_queue;
#[cfg(feature = "wire")]
mod wire;

pub use crate::interface::{CallIndex, Interface, ProcedureIndex, WireId};
#[cfg(feature = "wire")]
pub use crate::wire::{signal_termination, AutonomousWireHandle, CallHandle, Wire};
