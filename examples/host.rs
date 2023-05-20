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
    let mut module = Command::new(if wasm { "wasmtime" } else { module_path })
        // Needed for `wasmtime`, irrelevant for non-Wasm
        .arg(module_path)
        // Pipe stdio so we control it
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn module");

    // The module will be able to call this function
    INTERFACE.add_procedure(0, |(msg,): (String,)| {
        println!("(From module:) {}", msg);
    });

    // The interface is the core communication type, but the wire is what establishes the actual link between
    // the module and the host
    let wire = Wire::new(&INTERFACE);
    // When we have a `'static` reference to the interface, we can automatically spawn threads to handle reading
    // and writing for us, making this simpler and more ergonomic
    wire.start(module.stdout.take().unwrap(), module.stdin.take().unwrap());

    // IPFI is based on procedure calls, so we'll call some from the module now
    let first_name_handle = wire
        .call(ProcedureIndex::new(1), ("John".to_string(),))
        .unwrap();
    // We wait here to show how Wasm is different to non-Wasm in terms of response grouping, purely for educational
    // purposes
    std::thread::sleep(std::time::Duration::from_secs(1));
    let last_name_handle = wire
        .call(ProcedureIndex::new(2), ("Doe".to_string(),))
        .unwrap();

    // This procedure takes no arguments at all
    let magic_number_handle = wire.call(ProcedureIndex::new(0), ()).unwrap();

    // If we're deaing with a single-threaded remote program that reads all its input at once, we have to tell it
    // when we're done, which would require either dropping `module.stdin` here (impossible because the wire has
    // taken ownership) or or sending some kind of manual EOF-like signal. This is the latter, and can be used
    // to manually break out of read loops on the client-side. See the method docs for further details.
    if wasm {
        wire.signal_end_of_input().unwrap();
    }

    // Now we can wait on all those handles. If we were communicating with a multi-threaded program, we
    // could wait on them as we make the calls (i.e. `.call(..).unwrap().wait::<()>().unwrap()`).
    let _: () = first_name_handle.wait().unwrap();
    let _: () = last_name_handle.wait().unwrap();
    let magic_number: u32 = magic_number_handle.wait().unwrap();

    println!("Magic number was {}!", magic_number);

    // Wasm will automatically finish when it's done, but the multi-threaded non-Wasm module will hang around
    // waiting for further messages, so we'll explicitly signal a termination
    if !wasm {
        // We could have done this with `ipfi::signal_termination()` if we could get the module's stdin back, but
        // we can't, which is why this method exists
        wire.signal_termination().unwrap();
    }

    // Wait for the module to finish so we don't leave it hanging around
    let _ = module.wait();
}
