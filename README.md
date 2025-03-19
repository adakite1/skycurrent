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

    // SkyCurrent keeps track of all active MessageConsumers.
    // When a new message arrives, a snapshot of all active consumers is taken.
    // When all consumers pertaining to a message in this way are either dropped or passes on the message, the message is automatically dropped/claimed.
    // Messages can be claimed manually by calling `message.claim()` instead of `message.read()`.

    /// Send an arbitrarily-sized payload, expecting responses to that payload afterwards.
    /// 
    /// This is an example for how you might structure this. `MessageConsumer`'s must be created before sending the message.
    fn dlg_stream(payload: &[u8], header_size: usize) -> MessageConsumer {
        let consumer = skycurrent::iter_stream();
        skycurrent::send_stream(payload, header_size);
        consumer
    }

    // Send and receive messages together like this
    // (If a reply is expected from a message, you must use this pattern to capture any replies)
    let to_send = String::from("example message").as_bytes().to_vec();
    let mut seeker = dlg_stream(&to_send, 1);  // Same parameters as `send_stream`.
    let reply = loop {
        let message = seeker.next().await;
        if *message.read().unwrap() == ['a' as u8, ' ' as u8, 'r' as u8, 'e' as u8, 'p' as u8, 'l' as u8, 'y' as u8] {
            break message.claim().unwrap();
        }
    };
    drop(seeker);
    println!("{:?}", reply);

    // Clean up before exit.
    skycurrent::close();

    // By default, the "backing-ws-gateway" feature is also enabled, which creates a websocket server on port 8367 (only accessible from localhost or null origin client), allowing webpages to also participate in the messaging.
    // A client library is provided under the `skycurrent-js` folder, and the API mirrors the Rustlang tokio API closely. The wire format for communication over WebSocket is likewise straightforward:
    // - Send and receive ArrayBuffers through the WebSocket WebAPI
    // - Messages follow the format "<actual binary data><8-bytes little-endian representing header size>"

    Ok(())
}
```

