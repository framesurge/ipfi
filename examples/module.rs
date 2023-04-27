use ipfi::{BufferWire, Interface, Wire};
use once_cell::sync::Lazy;

/// The interface we hold with the host. This will be initialized when the program starts.
static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

fn main() {
    let reader = || {
        let mut read_wire = BufferWire::from_stdio_read_only(&INTERFACE);
        while read_wire.receive_one().is_ok() {}
    };
    // On Wasm, we have no threads, so we're forced to read all messages at once (we
    // could read a specified number at a time, or we could read until EOF, which is why
    // the corresponding host must drop our stdin handle in this example)
    #[cfg(target_arch = "wasm32")]
    reader();
    // Otherwise, we do the much more sensible thing of using threads, which do away with
    // all that stdin handle funny business
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(reader);

    let mut write_wire = BufferWire::from_stdio_write_only(&INTERFACE);

    // We expect a few messages from the host: this code will block until the above thread has
    // read each of them into the interface (which is thread-safe)
    //
    // Again, on Wasm we'll read these sequentially, but we'll read everything concurrently
    // anywhere else (not necessary, just shows the thread safety).
    let r1 = || {
        let first_name: String = INTERFACE.get(0).unwrap();
        eprintln!("Got first name from host: {}!", first_name);
    };
    let r2 = || {
        let last_name: String = INTERFACE.get(1).unwrap();
        eprintln!("Got last name from host: {}!", last_name);
    };
    #[cfg(target_arch = "wasm32")]
    {
        r1();
        r2();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t1 = std::thread::spawn(r1);
        let t2 = std::thread::spawn(r2);
        t1.join().unwrap();
        t2.join().unwrap();
    }

    // And write a response to the host (remember that the message indices are separated for read and write)
    write_wire
        .send_full_message(&"Thanks very much!".to_string(), 0)
        .unwrap();
}
