use ipfi::{Interface, ProcedureIndex, Wire};
use once_cell::sync::Lazy;

/// The interface we hold with the host. This will be initialized when the program starts.
static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

// Non-Wasm, we have threads
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let wire = prepare();

    // Since we're in a multi-threaded environment, we can spawn threads to do our reading and writing for us!
    let handle = wire.start(std::io::stdin(), std::io::stdout());

    // We can also call a function on the host (all-in-one because it's definitely multithreaded)
    wire.call(ProcedureIndex::new(0), ("Thanks very much!".to_string(),))
        .unwrap()
        .wait::<()>()
        .unwrap();

    // Because we're a module, we should wait until the host closes our streams to know we're done. On the
    // host side, this is done with `wire.signal_termination()` or `ipfi::signal_termination()`.
    handle.wait();
}

// Wasm, no threads
#[cfg(target_arch = "wasm32")]
fn main() {
    let wire = prepare();

    // But if we are on Wasm, we don't have threads, so we'll have to manually read all our input at
    // once (actually this reads until the host sends a special end-of-input message (different from EOF))
    wire.fill(&mut std::io::stdin()).unwrap();

    // We can also call a function on the host
    let _ = wire
        .call(ProcedureIndex::new(0), ("Thanks very much!".to_string(),))
        .unwrap();
    // Calling procedures involves first sending a call message, and then waiting for a response message,
    // but this function returns nothing, so we don't have to worry about that in this particular case
    wire.flush_end(&mut std::io::stdout()).unwrap();
}

fn prepare() -> Wire<'static> {
    // These are the functions that will be available for the host to call
    INTERFACE.add_procedure(0, |(): ()| -> u32 { 42 });
    INTERFACE.add_procedure(1, |(first_name,): (String,)| {
        // Stdout is captured by the host, so we use stderr here
        eprintln!("Got first name from host: {}!", first_name);
    });
    INTERFACE.add_procedure(2, |(last_name,): (String,)| {
        eprintln!("Got last name from host: {}!", last_name);
    });

    // If we didn't need to call any procedures on the host, this could be `::new_module()` instead to
    // improve security
    Wire::new(&INTERFACE)
}
