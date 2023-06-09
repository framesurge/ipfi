use ipfi::{Interface, Wire};
use once_cell::sync::Lazy;
use std::{net::Shutdown, time::Duration};
use tokio::{net::TcpListener, time::timeout};

static INTERFACE: Lazy<Interface> = Lazy::new(|| {
    let interface = Interface::new();
    interface.add_procedure(0, |(name,): (String,)| format!("Hello, {}!", name));

    interface
});

/// The maximum length of time for which a single read operation can block. If this is reached by any
/// read operation in IPFI, all subsequent reads will fail, meaning this will not be compounded.
const TCP_READ_TIMEOUT: u64 = 5; // Milliseconds

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:8000").await?;

    loop {
        match listener.accept().await {
            Ok((mut stream, _)) => tokio::task::spawn(async move {
                // Since this is a server, we disable unnecessary functionality to reduce attack surface and improve security
                let wire = Wire::new_module(&INTERFACE);
                // Read everything the user sends, responding to their queries
                //
                // Any attacks relying on holding this up can't hold it up for more than 5ms, and then we'll keep going,
                // probably with valid responses to any actual procedure calls
                let _ = timeout(
                    Duration::from_millis(TCP_READ_TIMEOUT),
                    wire.fill(&mut stream),
                )
                .await;
                // And flush all our responses thereto, and we don't use end-of-input because we aren't going to maybe keep going
                // later, this is the end of this wire
                let _ = wire.flush_terminate(&mut stream).await;

                // // Terminate the stream neatly
                // let _ = stream.shutdown(Shutdown::Write);

                // `wire` goes out of scope here and its identifier is gracefully relinquished
            }),
            Err(err) => {
                eprintln!("Failed to attach to stream: {}", err);
                continue;
            }
        };
    }
}
