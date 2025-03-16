# skycurrent

## Usage

```rust
use tokio;
use skycurrent;

fn should_collect(header: &[u8]) -> bool {
    // Use the header to filter out incoming messages that you are not interested in for better performance
    header[0] == 'a' as u8  // This example tells skycurrent to only collect messages where the first byte is 'a'
}

#[tokio::main]
async fn main() -> tokio::io::Result<()> {
    // SkyCurrent works on a per-project-directory basis
    let current_dir = std::env::current_dir()?;

    // Set the global parameters and initialize
    skycurrent::set_global_project_dir(current_dir);
    skycurrent::set_global_should_collect(should_collect);
    skycurrent::init().await;

    // Send messages like this
    let to_send = String::from("example message").as_bytes().to_vec();
    skycurrent::send_stream(&to_send, 1);  // The 1 here is the header size in bytes

    // Receive messages like this
    let mut message_consumer = skycurrent::iter_stream();
    loop {
        let message = message_consumer.next().await;

        println!("{:?}", *message.read().expect("messages that have been claimed or recycled will return None here."));
    }
}
```

