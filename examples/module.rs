use ipfi::{Interface, BufferWire, Wire};
use once_cell::sync::Lazy;

const MESSAGES: usize = 2;

/// The interface we hold with the host. This will be initialized when the program starts.
static INTERFACE: Lazy<Interface<MESSAGES>> = Lazy::new(|| Interface::new());

fn main() {
    // Spawn a thread for managing input from stdin, whcih will feed to the rest of the program
    std::thread::spawn(|| {
        let mut wire = BufferWire::from_stdio(&INTERFACE);
        while let Ok(_) = wire.receive_one() {}
    });

    let first_name: String = INTERFACE.get(0).unwrap();
    eprintln!("Got first name from host: {}!", first_name);
    let last_name: String = INTERFACE.get(1).unwrap();
    eprintln!("Got last name from host: {}!", last_name);
}
