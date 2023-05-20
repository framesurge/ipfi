mod complete_lock;
mod error;
mod interface;
mod procedure_args;
mod roi_queue;
mod wire;

pub use crate::interface::{CallIndex, Interface, ProcedureIndex, WireId};
pub use crate::wire::{signal_termination, AutonomousWireHandle, Wire};
