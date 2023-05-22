use ipfi::{signal_termination, Interface, Wire};
use once_cell::sync::Lazy;
use rayon::ThreadPoolBuilder;
use std::{
    net::{Shutdown, TcpListener},
    time::Duration,
};

static INTERFACE: Lazy<Interface> = Lazy::new(|| {
    let interface = Interface::new();
    interface.add_procedure(0, |(name,): (String,)| format!("Hello, {}!", name));

    interface
});

/// The maximum length of time for which a single read operation can block. If this is reached by any
/// read operation in IPFI, all subsequent reads will fail, meaning this will not be compounded.
const TCP_READ_TIMEOUT: u64 = 5; // Milliseconds

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:8000")?;

    let thread_pool = ThreadPoolBuilder::new().build()?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => thread_pool.spawn(move || {
                // This will only fail if we pass a zero duration
                stream
                    .set_read_timeout(Some(Duration::from_millis(TCP_READ_TIMEOUT)))
                    .unwrap();

                // Since this is a server, we disable unnecessary functionality to reduce attack surface and improve security
                let wire = Wire::new_module(&INTERFACE);
                // Read everything the user sends, responding to their queries
                //
                // Any attacks relying on holding this up can't hold it up for more than 5ms, and then we'll keep going,
                // probably with valid responses to any actual procedure calls
                let _ = wire.fill(&mut stream);
                // And flush all our responses thereto
                let _ = wire.flush(&mut stream);
                // We don't use end-of-input because we aren't going to maybe keep going later, this is the end of this
                // wire
                let _ = signal_termination(&mut stream);

                // Terminate the stream neatly
                let _ = stream.shutdown(Shutdown::Write);

                // `wire` goes out of scope here and its identifier is gracefully relinquished
            }),
            Err(err) => eprintln!("Failed to attach to stream: {}", err),
        }
    }

    Ok(())
}
