use hello::greeter_client::GreeterClient;
use hello::HelloRequest;
use std::time::Instant;

pub mod hello {
    tonic::include_proto!("hello");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let now = Instant::now();

    let mut client = GreeterClient::connect("http://127.0.0.1:8000").await?;

    let after_conn = now.elapsed();

    let request = tonic::Request::new(HelloRequest {
        name: "John Doe".into(),
    });

    let response = client.say_hello(request).await?;
    let greeting = response.into_inner().message;
    assert_eq!(greeting, "Hello, John Doe!");

    let end = now.elapsed();

    println!("connect_time: {}", after_conn.as_micros());
    println!("call_time: {}", end.as_micros());
    println!("---");
    println!("total_time: {}", (after_conn + end).as_micros());

    Ok(())
}
