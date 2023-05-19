use ipfi::{Interface, Wire};
use once_cell::sync::Lazy;

/// The interface we hold with the host. This will be initialized when the program starts.
static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

// Non-wasm, we have threads
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    INTERFACE.add_procedure(0, print_hello);
    // TODO Make these remote reservations instead
    INTERFACE.push();
    INTERFACE.push();

    let wire = Wire::new(&INTERFACE);
    wire.start(std::io::stdin(), std::io::stdout());

    // We expect a few messages from the host: this code will block until the above thread has
    // read each of them into the interface (which is thread-safe).
    //
    // We use threads here to show support for concurrency, there is little practical benefit in
    // this example.
    let r1 = std::thread::spawn(|| {
        let first_name: String = INTERFACE.get(0).unwrap();
        eprintln!("Got first name from host: {}!", first_name);
    });
    let r2 = std::thread::spawn(|| {
        let last_name: String = INTERFACE.get(1).unwrap();
        eprintln!("Got last name from host: {}!", last_name);
    });
    r1.join().unwrap();
    r2.join().unwrap();

    // And write a response to the host (remember that the message indices are separated for read and write)
    wire.send_full_message(&"Thanks very much!".to_string(), 0)
        .unwrap();
}

// Wasm, we have no threads
#[cfg(target_arch = "wasm32")]
fn main() {
    INTERFACE.add_procedure(0, print_hello);
    // TODO Make these remote reservations instead
    INTERFACE.push();
    INTERFACE.push();

    let wire = Wire::new(&INTERFACE);
    // We have no threads, so we'll read until the host sends end-of-input
    wire.fill(&mut std::io::stdin()).unwrap();

    // We have to read these sequentially in Wasm, hoping the above has gotten the messages to back them,
    // otherwise this would hang forever
    let first_name: String = INTERFACE.get(0).unwrap();
    eprintln!("Got first name from host: {}!", first_name);
    let last_name: String = INTERFACE.get(1).unwrap();
    eprintln!("Got last name from host: {}!", last_name);

    // And write a response to the host (remember that the message indices are separated for read and write)
    wire.send_full_message(&"Thanks very much!".to_string(), 0)
        .unwrap();
    wire.flush(&mut std::io::stdout()).unwrap();
}

fn print_hello(_: ()) {
    eprintln!("Hey there!");
}
