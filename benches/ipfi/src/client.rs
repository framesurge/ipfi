use ipfi::{Interface, ProcedureIndex, Wire};
use once_cell::sync::Lazy;
use std::{net::TcpStream, time::Instant};

static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let now = Instant::now();

    let mut stream = TcpStream::connect("127.0.0.1:8000")?;

    let after_conn = now.elapsed();

    let wire = Wire::new(&INTERFACE);
    let greeting_handle = wire.call(ProcedureIndex::new(0), ("John Doe".to_string(),))?;
    wire.signal_end_of_input()?;
    wire.flush(&mut stream)?;
    wire.fill(&mut stream)?;
    let greeting: String = greeting_handle.wait()?;
    assert_eq!(greeting, "Hello, John Doe!");

    let end = now.elapsed();

    println!("connect_time: {}", after_conn.as_micros());
    println!("call_time: {}", end.as_micros());
    println!("---");
    println!("total_time: {}", (after_conn + end).as_micros());

    Ok(())
}
