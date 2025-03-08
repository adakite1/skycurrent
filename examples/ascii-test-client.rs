use std::env;
use std::io::{self, BufRead, Write};
use std::thread;

use tracing::{trace, info, error};
use tracing_subscriber;

fn main() {
    // Initialize tracing subscriber at the TRACE level.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();
    info!("Starting test client...");

    // Use the current directory as the project folder.
    let current_dir = env::current_dir().expect("Failed to get current directory");
    trace!("Current directory: {:?}", current_dir);

    // Spawn the sender thread.
    let send_dir = current_dir.clone();
    let sender = thread::spawn(move || {
        // Initialize IPC for this thread.
        skycurrent::init(send_dir.as_path(), skycurrent::InitFlags::IOX2_CREAT_PUBLISHER | skycurrent::InitFlags::IOX2_CREAT_NOTIFIER).expect("Initialization failed in sender thread");

        // Use stdin for reading input.
        let stdin = io::stdin();
        let mut stdout = io::stdout();

        println!("\x1b[32m[Sender] Ready. Type your message and press Enter:\x1b[0m");
        for line in stdin.lock().lines() {
            match line {
                Ok(input) => {
                    // Log and print the message.
                    // trace!("[Sender] Sending message: {:?}", input);
                    println!("\x1b[32m[send] {}\x1b[0m", input);
                    stdout.flush().unwrap();

                    // Send the payload.
                    if let Err(e) = skycurrent::send_stream(input.as_bytes(), 1) {
                        error!("[Sender] Error sending: {:?}", e);
                        eprintln!("\x1b[31m[Sender] Error sending: {:?}\x1b[0m", e);
                    }
                }
                Err(e) => {
                    error!("[Sender] Error reading input: {:?}", e);
                    eprintln!("\x1b[31m[Sender] Error reading input: {:?}\x1b[0m", e);
                    break;
                }
            }
        }
    });

    // Spawn the receiver thread.
    let recv_dir = current_dir.clone();
    let receiver = thread::spawn(move || {
        // Initialize IPC for this thread.
        // As you can see, we are running `init` once again. `init` can and must be called on a per thread basis, with each thread having a separate set of notifiers/listeners/publishers/subscribers. Here, we use two threads, one for notifying/publishing, and the other, this one, for listening/subscribing.
        skycurrent::init(recv_dir.as_path(), skycurrent::InitFlags::IOX2_CREAT_SUBSCRIBER | skycurrent::InitFlags::IOX2_CREAT_LISTENER).expect("Initialization failed in receiver thread");

        loop {
            // Wait for a message.
            match skycurrent::recv_stream(|payload_header| {
                payload_header[0] == 'a' as u8
            }) {
                Ok(payload) => {
                    // Convert the raw bytes into a String.
                    let message = String::from_utf8_lossy(&payload).to_string();
                    // trace!("[Receiver] Received message: {:?}", message);
                    // Print messages.
                    println!("\x1b[34m[recv] {} \x1b[0m", message);
                }
                Err(e) => {
                    // Nothing received, wait until more events arrive.
                    error!("[Receiver] Error receiving: {:?}", e);
                    eprintln!("\x1b[31m[Receiver] Error receiving: {:?}\x1b[0m", e);
                }
            }
        }
    });

    // Join both threads.
    sender.join().unwrap();
    println!("\x1b[31mSender stopped.\x1b[0m");
    receiver.join().unwrap();
    println!("\x1b[31mReceiver stopped.\x1b[0m");
}

