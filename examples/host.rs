use ipfi::{BufferWire, Interface, Wire};
use once_cell::sync::Lazy;
use std::process::{Command, Stdio};

static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

fn main() {
    // Get the name of the module executable from arguments (not necessary in most real programs)
    let args = std::env::args().collect::<Vec<_>>();
    let module_path = &args[1];
    let wasm = matches!(args.get(2).map(|x| x.as_str()), Some("true"));

    // --- REALISTIC CODE ---

    // Spawn the module process so we can talk to it (if you're communicating between two already-running
    // programs, you'll probably have another way of getting messages to and fro)
    //
    // The weird parts of this just allow this example to execute the module as Wasm
    // or not
    let mut child = Command::new(if wasm { "wasmtime" } else { module_path })
        // Needed for `wasmtime`, irrelevant for non-Wasm
        .arg(module_path)
        // Pipe stdio so we control it
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn module");
    // Move into a new scope, where we'll create the wire from mutable borrows of the child's stdio,
    // which we have to drop before we can wait on it
    {
        {
            // Instantiate a buffer wire to communicate with the child over its stdio
            //
            // You could create a read/write buffer that would also work, but we want to
            // be able to explicitly drop the stdin, so we have to do it this way
            let mut child_wire_w =
                BufferWire::new_write_only(child.stdin.as_mut().unwrap(), &INTERFACE);

            // Send some messages, waiting in between to show how the child will block waiting where it wants to
            child_wire_w
                .send_full_message(&"John".to_string(), 0)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_secs(1));
            child_wire_w
                .send_full_message(&"Doe".to_string(), 1)
                .unwrap();
        }

        // Drop the stdin, letting the module know we're done sending messages (only necessary
        // if you're not using a separate reader thread, and reading all messages at once,
        // as you would in Wasm)
        let _ = child.stdin.take();

        let mut child_wire_r =
            BufferWire::new_read_only(child.stdout.as_mut().unwrap(), &INTERFACE);
        // We expect a response from the child now, so wait until its finished sending messages to us (through its stdout)
        while child_wire_r.receive_one().is_ok() {}

        // We can then fetch what should be the child's first message to us from the interface
        let response: String = INTERFACE.get(0).unwrap();
        eprintln!("(From module:) {}", response);
    }

    let _ = child.wait();
}
