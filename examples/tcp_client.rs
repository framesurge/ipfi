use ipfi::{Interface, ProcedureIndex, Wire};
use once_cell::sync::Lazy;
use std::net::TcpStream;

static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect("127.0.0.1:8000")?;
    let wire = Wire::new(&INTERFACE);

    let greeting_handle = wire.call(ProcedureIndex::new(0), ("John Doe".to_string(),))?;

    // Omitting this will just mean we get to the maximum read timeout for the server, and this
    // cannot cause a DoS attack
    wire.signal_end_of_input()?;
    wire.flush(&mut stream)?;
    wire.fill(&mut stream)?;

    let greeting: String = greeting_handle.wait()?;
    println!("{}", greeting);

    Ok(())
}
