use std::process::{Command, Stdio};
use ipfi::{BufferWire, Interface, Wire};
use once_cell::sync::Lazy;

const MESSAGES: usize = 2;
static INTERFACE: Lazy<Interface<MESSAGES>> = Lazy::new(|| Interface::new());

fn main() {
    // Get the name of the module executable from arguments (not necessary in most real programs)
    let args = std::env::args().collect::<Vec<_>>();
    let module_path = &args[1];

    // --- REALISTIC CODE ---

    // Spawn the module process so we can talk to it (if you're communicating between two already-running
    // programs, you'll probably have another way of getting messages to and fro)
    let mut child = Command::new(module_path)
        // Pipe stdio so we control it
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn module");
    // Move into a new scope, where we'll create the wire from mutable borrows of the child's stdio,
    // which we have to drop before we can wait on it
    {
        let mut child_wire = BufferWire::new(
            // Notice that we receive from the child's stdout, and send to their stdin
            child.stdout.as_mut().unwrap(),
            child.stdin.as_mut().unwrap(),
            &INTERFACE,
        );

        child_wire.send_full_message(&"John".to_string(), 0).unwrap();
        // std::thread::sleep(std::time::Duration::from_secs(1));
        child_wire.send_full_message(&"Doe".to_string(), 1).unwrap();
    }

    let _ = child.wait();
}

struct DummyReader;

impl std::io::Read for DummyReader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}
