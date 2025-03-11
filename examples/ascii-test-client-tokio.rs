use std::io::{stdin, stdout, BufRead, Write};

use skycurrent::tokio::{SkyCurrentStreamReceiver, SkyCurrentStreamSender};
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
    skycurrent::tokio::set_global_project_dir(current_dir);

    // Spawn task to handle receiving messages
    let receive_handle = tokio::spawn(async move {
        let mut message_consumer = SkyCurrentStreamReceiver::get_or_init(should_collect).iter_stream();
        while let mut message = message_consumer.next().await {
            if let Some(payload) = message.claim() {
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
        if let Err(e) = SkyCurrentStreamSender::get_or_init().close().await {
            error!("Error during shutdown: {}", e);
        }
        if let Err(e) = SkyCurrentStreamReceiver::get_or_init(should_collect).close().await {
            error!("Error during shutdown: {}", e);
        }
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
                    SkyCurrentStreamSender::get_or_init().close().await?;
                    SkyCurrentStreamReceiver::get_or_init(should_collect).close().await?;
                } else if input == "reset" {
                    println!("\x1b[31m[Sender] Resetting stream sender and receiver...\x1b[0m");
                    SkyCurrentStreamSender::get_or_init().close().await?;
                    SkyCurrentStreamReceiver::get_or_init(should_collect).close().await?;
                    let _ = SkyCurrentStreamSender::get_or_init();
                    let _ = SkyCurrentStreamReceiver::get_or_init(should_collect);
                } else {
                    let mut to_send = input.into_bytes();
                    while let Err(e) = SkyCurrentStreamSender::get_or_init().send_stream(to_send, 1).await {
                        error!("[Sender] Error sending, will retry: {:?}", e);
                        to_send = e.0;
                    }
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

