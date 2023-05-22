use ipfi::{Interface, ProcedureIndex, Wire};
use once_cell::sync::Lazy;
use std::{net::TcpStream, time::Instant};

static INTERFACE: Lazy<Interface> = Lazy::new(|| Interface::new());

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let now = Instant::now();

    let mut stream = TcpStream::connect("127.0.0.1:8000")?;

    let after_conn = now.elapsed();

    // We could split `stream` into readable and writeable using `.try_clone()`, but using the synchronous
    // API without threads is almost twice as fast (avg. call time 118.7µs vs 203.5µs), so, since this is
    // a benchmark, we don't. Nonetheless, the threaded API is still around 87.8% faster than gRPC.
    //
    // (The above numbers may be out-of-date, as the threadded API is not regularly benchmarked against the
    // synchronous one at present.)
    let wire = Wire::new(&INTERFACE);
    let greeting_handle = wire.call(ProcedureIndex::new(0), ("John Doe".to_string(),))?;
    wire.flush_end(&mut stream)?;
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
