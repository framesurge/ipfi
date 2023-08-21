use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;

crate::define_wire!(impl AsyncRead + Unpin, impl AsyncWrite + Unpin, populate_from_async_reader, async, await);

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
        mut reader: impl AsyncRead + Unpin + Send + Sync + 'static,
        mut writer: impl AsyncWrite + Unpin + Send + Sync + 'static,
    ) -> AutonomousWireHandle {
        let self_reader = self.clone();
        let reader =
            tokio::task::spawn(async move { while self_reader.fill(&mut reader).await.is_ok() {} });
        let self_writer = self.clone();
        let writer = tokio::task::spawn(async move {
            // TODO Spinning...
            while self_writer.flush_partial(&mut writer).await.is_ok() {
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
    pub async fn wait(self) {
        // TODO Join these together
        // We propagate any thread panics to the caller, there shouldn't be any
        self.reader.await.unwrap();
        self.writer.await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    crate::define_wire_tests!(tokio::test, async, await);
}
