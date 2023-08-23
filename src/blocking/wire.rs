use std::{
    io::{Read, Write},
    thread::JoinHandle,
};

crate::define_wire!(impl Read, impl Write, populate_from_reader);
// Dropping the wire must mark it as terminated, with all the consequences thereof, and then relinquish the
// wire's identifier so it can be reused on the interface (we don't do that in termination because the wire
// is still technically active, and we don't want to risk a possibly very bad cross-wire race condition)
impl Drop for Wire<'_> {
    fn drop(&mut self) {
        // For the blocking API, this is trivial!
        self.mark_terminated();
        // Relinquish the ID last to prevent nasty race conditions
        self.interface.relinquish_id(self.id);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Wire<'static> {
    /// Starts an autonomous version of the wire by starting two new threads, one for reading and one for writing. If you call this,
    /// it is superfluous to call `.fill()`/`.flush()`, as they will be automatically called from here on.
    ///
    /// This method is only available when a `'static` reference to the [`Interface`] is held, since only that can be passed safely between
    /// threads. You must also own both the reader and writer in order to use this method (which typically means this method must hold
    /// those two exclusively).
    pub fn start(
        &self,
        mut reader: impl Read + Send + Sync + 'static,
        mut writer: impl Write + Send + Sync + 'static,
    ) -> AutonomousWireHandle {
        let self_reader = self.clone();
        let reader = std::thread::spawn(move || while self_reader.fill(&mut reader).is_ok() {});
        let self_writer = self.clone();
        let writer = std::thread::spawn(move || {
            // TODO Spinning...
            while self_writer.flush_partial(&mut writer).is_ok() {
                std::hint::spin_loop();
            }
        });

        AutonomousWireHandle { reader, writer }
    }
}

/// A handle representing the reader/writer threads started by [`Wire::start`].
pub struct AutonomousWireHandle {
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}
impl AutonomousWireHandle {
    /// Waits for both threads to be done, which will occur once the wire is expressly terminated. Generally, this is not needed
    /// when communication patterns are predictable, although in host-module scenarios, the module should generally call this
    /// to wait until the host expressly terminates it.
    pub fn wait(self) {
        // We propagate any thread panics to the caller, there shouldn't be any
        self.reader.join().unwrap();
        self.writer.join().unwrap();
    }
}

#[cfg(test)]
mod tests {
    crate::define_wire_tests!(test);

    #[test]
    fn wire_termination_should_halt_ongoing_receiver() {
        let mut alice = Actor::new();
        let mut bob = Actor::new();
        alice.interface.add_procedure(0, move |(): ()| 42);

        let handle = bob.wire.call(ProcedureIndex(0), ()).unwrap();
        let mut rx = handle.wait_chunk_stream().unwrap();
        let thread = std::thread::spawn(move || {
            // This will start immediately, so we're testing the ongoing checking of the complete lock
            rx.recv::<u32>()
        });

        alice.input.set_position(0);
        // Immediately terminate Alice's wire before Bob's procedure call can go through
        alice.wire.signal_termination();
        alice.wire.flush_end(&mut bob.input).unwrap();
        drop(alice);
        bob.input.set_position(0);
        bob.wire.fill(&mut bob.input).unwrap();

        // Wait for the thread's result (it won't have panicked)
        let result = thread.join().unwrap();
        // It should be that the receiver failed
        assert!(result.is_err());
    }
}
