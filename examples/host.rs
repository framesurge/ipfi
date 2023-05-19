use ipfi::{Interface, ProcedureIndex, Wire};
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

    // Allocate our first buffer for the message we'll get later from the module (this makes that number predictable,
    // as method calls will allocate buffers autonomously)
    INTERFACE.push();

    // The interface is the core communication type, but the wire is what establishes the actual link between
    // the module and the host
    let wire = Wire::new(&INTERFACE);
    // When we have a `'static` reference to the interface, we can automatically spawn threads to handle reading
    // and writing for us, making this simpler and more ergonomic
    wire.start(child.stdout.take().unwrap(), child.stdin.take().unwrap());

    // IPFI is based on message buffers, which can be written to arbitrarily for as long as you like. Here,
    // we're writing to two separate buffers, and we're writing the entire message at once, which means we'll
    // write and then close the buffer. Once a buffer has been closed, no more data can be written to it.
    // Internally, our interface will retain nothing about these messages after it has written them.
    wire.send_full_message(&"John".to_string(), 0).unwrap();
    // That message will now be in a queue and will be sent autonomously, so let's wait a moment for this
    // next one to show how the module will block.
    //
    // If you run this example with Wasm, everything will be delayed, because we have to perform all reads at once.
    std::thread::sleep(std::time::Duration::from_secs(1));
    wire.send_full_message(&"Doe".to_string(), 1).unwrap();

    let greeting_handle = wire.call(ProcedureIndex::new(0), ()).unwrap();

    // If we're deaing with a single-threaded remote program that reads all its input at once, we have to tell it
    // when we're done, which would require either dropping `child.stdin` here (impossible because the wire has
    // taken ownership) or or sending some kind of manual EOF-like signal. This is the latter, and can be used
    // to manually break out of read loops on the client-side. See the method docs for further details.
    if wasm {
        wire.signal_end_of_input().unwrap();
    }

    let _: () = greeting_handle.wait().unwrap();

    // Now we're using that buffer we pre-allocated earlier
    let msg_from_child: String = INTERFACE.get(0).unwrap();
    println!("(From module:) {}", msg_from_child);

    // Wait for the module to finish so we don't leave it hanging around
    let _ = child.wait();
}
