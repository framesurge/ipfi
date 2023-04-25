use serde::Serialize;

/// A generic implementation of a *wire*, over which data can be sent and received between processes. Generally, a program will have
/// one or two wires, with the reception wire consigned to its own thread for an infinite loop, while a send wire will be maintained
/// by the main thread for transmitting events to the other process. Programs communicating with many other programs may maintain
/// a separate sending wire for each.
///
/// Individual wires may choose to handle termination signals in different ways. The canonical way is to send a negative input,
/// however wires using pure byte arrays may achieve this using other means, such as zero-length messages.
pub trait Wire {
    /// The error type this wire has.
    type Error;

    /// Sends the given bytes over the wire. This will not send a termination signal.
    fn send_bytes(&mut self, bytes: &[u8], message_idx: usize) -> Result<(), Self::Error>;
    /// Sends a termination signal for the given message index.
    fn end_message(&mut self, message_idx: usize) -> Result<(), Self::Error>;
    /// Receives a single message from the wire into the local interface. Deserialization etc. is the responsibility
    /// of the interface.
    ///
    /// This does not expect to receive a full message, and it may receive anything from a partial message to a single byte, depending
    /// on the implementation.
    fn receive_one(&mut self) -> Result<(), Self::Error>;

    /// Sends the given data over the wire with the given message index, serializing it to bytes using MessagePack first.
    /// This will not send a termination signal.
    fn send<T: Serialize>(&mut self, data: &T, message_idx: usize) -> Result<(), Self::Error> {
        let bytes = rmp_serde::encode::to_vec(data).unwrap(); // TODO
        self.send_bytes(&bytes, message_idx)
    }
    /// Sends a full message over the wire, including a termination signal.
    fn send_full_message<T: Serialize>(&mut self, data: &T, message_idx: usize) -> Result<(), Self::Error> {
        self.send(data, message_idx)?;
        self.end_message(message_idx)?;

        Ok(())
    }
}
