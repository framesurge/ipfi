use crate::interface::Interface;
use crate::wire::Wire;
use std::io::{Read, Write};

/// A wire implementation using synchronous Rust I/O buffers.
pub struct BufferWire<'a> {
    /// A lock on `stdin` to prevent other threads from accessing it.
    input: Box<dyn Read + 'a>,
    /// A lock on `stdout`
    output: Box<dyn Write + 'a>,
    /// The interface to interact with.
    interface: &'a Interface,
}
impl<'a> BufferWire<'a> {
    /// Creates a new buffer-based wire from the given input, output, and interface.
    pub fn new(input: impl Read + 'a, output: impl Write + 'a, interface: &'a Interface) -> Self {
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
    pub fn from_stdio(interface: &'a Interface) -> Self {
        let stdin = std::io::stdin().lock();
        let stdout = std::io::stdout().lock();

        Self::new(stdin, stdout, interface)
    }
    /// Creates a new buffer-based wire with the given input source and a dummy writer. Any attempts to write
    /// output to this wire will silently do nothing.
    ///
    /// This can be extremely useful for threads that solely read the input a process is receiving.
    pub fn new_read_only(input: impl Read + 'a, interface: &'a Interface) -> Self {
        Self::new(input, DummyWriter, interface)
    }
    /// Creates a new buffer-based wire with the given output source and a dummy reader. Any attempts to read
    /// input from this wire will silently do nothing.
    ///
    /// This can be extremely useful for threads that only write output to other processes.
    pub fn new_write_only(output: impl Write + 'a, interface: &'a Interface) -> Self {
        Self::new(DummyReader, output, interface)
    }
    /// Same as `::from_stdio()`, but this will create a read-only wire that only locks stdin, leaving stdout free.
    /// Any attempts to write output to this wire wil silently do nothing.
    ///
    /// This can be extremely useful for threads that solely read the input a process is receiving.
    pub fn from_stdio_read_only(interface: &'a Interface) -> Self {
        let stdin = std::io::stdin().lock();
        Self::new(stdin, DummyWriter, interface)
    }
    /// Same as `::from_stdio()`, but this will create a write-only wire that only locks stdout, leaving stdin free.
    /// Any attempts to read input from this wire wil silently do nothing.
    ///
    /// This can be extremely useful for threads that only write output to other processes.
    pub fn from_stdio_write_only(interface: &'a Interface) -> Self {
        let stdout = std::io::stdout().lock();
        Self::new(DummyReader, stdout, interface)
    }
}
impl<'a> Wire for BufferWire<'a> {
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

struct DummyReader;
impl Read for DummyReader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

struct DummyWriter;
impl Write for DummyWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
