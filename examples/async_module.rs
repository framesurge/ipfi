use ipfi::{Interface, ProcedureIndex, Wire};
use once_cell::sync::Lazy;

/// The interface we hold with the host. This will be initialized when the program starts.
static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

// NOTE: Right now, `tokio` doesn't support some features on Wasm, meaning we can't use the async API there just yet.
#[tokio::main]
async fn main() {
    let wire = prepare();

    // Since we're in a multi-threaded environment, we can spawn threads to do our reading and writing for us!
    let handle = wire.start(tokio::io::stdin(), tokio::io::stdout());

    // We can also call a function on the host (all-in-one because it's definitely multithreaded)
    wire.call(ProcedureIndex::new(0), ("Thanks very much!".to_string(),))
        .await
        .unwrap()
        .wait::<()>()
        .await
        .unwrap();

    // Because we're a module, we should wait until the host closes our streams to know we're done. On the
    // host side, this is done with `wire.signal_termination()` or `ipfi::signal_termination()`.
    handle.wait().await;
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
    INTERFACE.add_sequence_procedure(3, |yielder, (): ()| {
        tokio::task::spawn(async move {
            yielder("This is a test".to_string(), false).unwrap();
            yielder("of the system".to_string(), false).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(1));
            yielder("And this is a second test!".to_string(), true).unwrap();
        });
    });

    // If we didn't need to call any procedures on the host, this could be `::new_module()` instead to
    // improve security
    Wire::new(&INTERFACE)
}
