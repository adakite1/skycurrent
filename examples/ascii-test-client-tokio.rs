use std::io::{stdin, stdout, BufRead, Write};

use tokio::{self, io::Result, signal};
use tracing::error;

fn should_collect(header: &[u8]) -> bool {
    header[0] == 'a' as u8
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();
    
    let current_dir = std::env::current_dir()?;
    // Set the global project directory.
    skycurrent::set_global_project_dir(current_dir);
    skycurrent::set_global_should_collect(should_collect);
    skycurrent::init().await;

    // Spawn task to handle receiving messages
    let receive_handle = tokio::spawn(async move {
        let mut message_consumer = skycurrent::iter_stream();
        loop {
            let message = message_consumer.next().await;

            if let Some(payload) = message.read() {
                let message_text = String::from_utf8_lossy(&payload);
                println!("\x1b[34m[recv] {}\x1b[0m", message_text);
            }
        }
    });

    // Spawn task to watch for Ctrl+C
    let shutdown = tokio::spawn(async {
        if let Err(e) = signal::ctrl_c().await {
            error!("Failed to listen for ctrl+c: {}", e);
            return;
        }

        println!("\nReceived Ctrl+C, shutting down...");
        skycurrent::backing::combined::close();
    });

    // Handle sending in the main task
    let stdin = stdin();
    let mut stdout = stdout();

    println!("\x1b[32m[Sender] Ready. Type your message and press Enter:\x1b[0m");
    for line in stdin.lock().lines() {
        match line {
            Ok(input) => {
                println!("\x1b[32m[send] {}\x1b[0m", input);
                stdout.flush().unwrap();
                
                if input == "exit" {
                    println!("\x1b[31m[Sender] Closing stream sender and receiver... (note that the sender will be restarted on next send but the receiver is no longer running)\x1b[0m");
                    skycurrent::backing::combined::close();
                } else {
                    let to_send = input.into_bytes();
                    skycurrent::send_stream(&to_send, 1);
                }
            }
            Err(e) => {
                error!("[Sender] Error reading input: {:?}", e);
                break;
            }
        }
    }

    Ok(())
}

