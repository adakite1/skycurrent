use std::io::Write;

use tokio::{self, io::{self, BufReader, AsyncBufReadExt, Result}};
use tracing::error;

fn should_collect(header: &[u8]) -> bool {
    header[0] == 'a' as u8
}

#[tokio::main(flavor = "current_thread")]
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

    print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
    // Handle sending in an async manner
    println!("\x1b[32m[Sender] Ready. Type your message and press Enter:\x1b[0m");
    
    let mut stdin = BufReader::new(io::stdin());
    let mut line = String::new();
    
    loop {
        // Reset the line buffer
        line.clear();
        
        // Asynchronously read a line from stdin
        match stdin.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                // Trim the newline character
                let input = line.trim();
                println!("\x1b[32m[send] {}\x1b[0m", input);
                std::io::stdout().flush().unwrap();
                
                if input == "exit" {
                    println!("\x1b[31m[Sender] Closing stream sender and receiver...\x1b[0m");
                    skycurrent::backing::combined::close();
                } else {
                    let to_send = input.as_bytes().to_vec();
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

