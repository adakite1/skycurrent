use std::{path::Path, sync::{Arc, OnceLock}};
use parking_lot::{Condvar, Mutex};
use tokio::sync::mpsc;
use std::thread;

pub use tokio::sync::mpsc::error::SendError;

use crate::{init, send_stream, InitFlags};

static GLOBAL_STREAM_SENDER: OnceLock<Arc<Mutex<SkyCurrentStreamSender>>> = OnceLock::new();

enum SendThreadMessage {
    Send {
        payload: Vec<u8>,
        header_size: usize
    },
    Terminate
}
pub struct SkyCurrentStreamSender {
    sender_tx: mpsc::Sender<SendThreadMessage>,
    sender_termination_pair: Arc<(Mutex<bool>, Condvar)>,
}
impl SkyCurrentStreamSender {
    fn is_closed(&self) -> bool {
        self.sender_tx.is_closed()
    }
    /// Creates a brand new stream sender with its own sending thread.
    fn new(project_dir: impl AsRef<Path>) -> Self {
        let project_dir = project_dir.as_ref().to_path_buf();

        // Channels for communication between threads and async context
        let (sender_tx, mut sender_rx) = mpsc::channel::<SendThreadMessage>(100);
        let sender_termination_pair = Arc::new((Mutex::new(false), Condvar::new()));

        // Spawn sender thread
        let send_dir = project_dir;
        let sender_termination_pair_thread = sender_termination_pair.clone();
        thread::spawn(move || {
            if let Err(e) = init(&send_dir, InitFlags::IOX2_CREAT_PUBLISHER | InitFlags::IOX2_CREAT_NOTIFIER) {
                panic!("Failed to initialize sender thread: {:?}", e);
            }
            while let Some(SendThreadMessage::Send{ payload, header_size }) = sender_rx.blocking_recv() {
                if let Err(e) = send_stream(&payload, header_size) {
                    panic!("Error in sender thread: {:?}", e);
                }
            }
            // Do this right before exiting to notify any on-lookers that the sender thread has exited.
            let (lock, cvar) = &*sender_termination_pair_thread;
            let mut exited = lock.lock();
            *exited = true;
            cvar.notify_all();
        });

        Self {
            sender_tx,
            sender_termination_pair,
        }
    }
    /// Get the global stream sender if it exists or initializes a new one if it doesn't.
    /// 
    /// Note that `get_or_init` will always block and only return once the global stream sender is in a ready state.
    pub fn get_or_init(project_dir: impl AsRef<Path>) -> parking_lot::lock_api::MutexGuard<'static, parking_lot::RawMutex, SkyCurrentStreamSender> {
        let project_dir = project_dir.as_ref();
        let lock = GLOBAL_STREAM_SENDER.get_or_init(|| Arc::new(Mutex::new(
            SkyCurrentStreamSender::new(project_dir)
        )));

        // Get a reference to the existing global_stream_sender.
        let mut global_stream_sender = lock.lock();

        // Check if the stream sender is still open. If it is, we can stop here and just return the current global stream sender.
        if !global_stream_sender.is_closed() {
            return global_stream_sender;
        }

        // If the existing sender is closed, create a new one.
        *global_stream_sender = SkyCurrentStreamSender::new(project_dir);
        global_stream_sender
    }
    /// Send termination signal to stream sender and then wait for it to close.
    pub async fn close(&self) -> tokio::io::Result<()> {
        // Send termination signal to thread.
        match self.sender_tx.send(SendThreadMessage::Terminate).await {
            Ok(_) => {
                // Sender thread hasn't exited yet, but the signal has been sent, so the thread should eventually see it and exit. Wait for that to happen.
                let (lock, cvar) = &*self.sender_termination_pair;
                let mut exited = lock.lock();
                if !*exited {
                    cvar.wait(&mut exited);
                }
            },
            Err(_) => {
                // Sender thread has already exited.
            },
        }
        Ok(())
    }
    /// Send a stream.
    /// 
    /// Note that since this requires assembly of data on the receiver's side, all payloads should have a small header section so that receivers can decide if they want to reconstruct the message or pass on it, saving memory and execution time.
    /// 
    /// The `header_size` specifies how many bytes from the head of the payload corresponds to the header; every fragment sent will include the header but have different sections of the body. The receiver can then filter incoming messages based on the header, not copying messages it does not need.
    pub async fn send_stream(&self, payload: Vec<u8>, header_size: usize) -> Result<(), SendError<Vec<u8>>> {
        self.sender_tx.send(SendThreadMessage::Send{ payload, header_size }).await
            .map_err(|e| if let SendThreadMessage::Send { payload, header_size: _ } = e.0 { SendError(payload) } else { panic!("this should never happen.") })
    }
}

