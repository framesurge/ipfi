use std::io::{Write, Read};
use crate::interface::Interface;
use crate::wire::Wire;

/// A wire implementation using synchronous Rust I/O buffers.
pub struct BufferWire<'a, const MESSAGES: usize> {
    /// A lock on `stdin` to prevent other threads from accessing it.
    input: Box<dyn Read + 'a>,
    /// A lock on `stdout`
    output: Box<dyn Write + 'a>,
    /// The interface to interact with.
    interface: &'a Interface<MESSAGES>,
}
impl<'a, const MESSAGES: usize> BufferWire<'a, MESSAGES> {
    /// Creates a new buffer-based wire from the given input, output, and interface.
    pub fn new(input: impl Read + 'a, output: impl Write + 'a, interface: &'a Interface<MESSAGES>) -> Self {
        Self {
            input: Box::new(input),
            output: Box::new(output),
            interface,
        }
    }
    /// Creates a new buffer-based wire from the given interface, using stdin as an input and stdout as an output. Generally,
    /// this should be used by modules executed as child processes by the hosts they want to communicate with.
    ///
    /// **WARNING:** This will lock both stdin and stdout, preventing any other operations from using them until this
    /// wire is dropped! After calling this, make sure to output any logs or the like to stderr, or your application will
    /// hang.
    pub fn from_stdio(interface: &'a Interface<MESSAGES>) -> Self {
        let stdin = std::io::stdin().lock();
        let stdout = std::io::stdout().lock();

        Self {
            input: Box::new(stdin),
            output: Box::new(stdout),
            interface,
        }
    }
}
impl<'a, const MESSAGES: usize> Wire for BufferWire<'a, MESSAGES> {
    type Error = std::io::Error;

    fn send_bytes(&mut self, bytes: &[u8], message_idx: usize) -> Result<(), Self::Error> {
        // IPFI expects the message index first as a `u32`, expressed here as an array
        // of four `u8`s, in little endian order
        let message_idx = (message_idx as u32).to_le_bytes();
        self.output.write_all(&message_idx)?;

        // Then we send the number of bytes to expect next
        let num_bytes = bytes.len() as u32;
        let num_bytes = num_bytes.to_le_bytes();
        self.output.write_all(&num_bytes)?;

        // And finally send the actual bytes
        self.output.write_all(&bytes)?;

        Ok(())
    }
    fn end_message(&mut self, message_idx: usize) -> Result<(), Self::Error> {
        // Message index as usual
        let message_idx = (message_idx as u32).to_le_bytes();
        self.output.write_all(&message_idx)?;

        // Then a length of zero
        self.output.write_all(&[0u8; 4])?;

        Ok(())
    }
    fn receive_one(&mut self) -> Result<(), Self::Error> {
        // First is the message index
        let mut idx_buf = [0u8; 4];
        self.input.read_exact(&mut idx_buf)?;
        let message_idx = u32::from_le_bytes(idx_buf) as usize;

        // Then the number of bytes to expect
        let mut len_buf = [0u8; 4];
        self.input.read_exact(&mut len_buf)?;
        let num_bytes = u32::from_le_bytes(len_buf) as usize;

        // If that's zero, we should end the message
        if num_bytes == 0 {
            self.interface.terminate_message(message_idx);
        } else {
            let mut bytes = vec![0u8; num_bytes];
            self.input.read_exact(&mut bytes)?;

            self.interface.send_many(&bytes, message_idx);
        }

        Ok(())
    }
}
