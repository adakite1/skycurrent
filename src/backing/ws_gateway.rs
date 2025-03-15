use std::sync::{LazyLock, OnceLock};

use fastwebsockets::{upgrade, Frame, OpCode, WebSocketError};
use http_body_util::Empty;
use hyper::{body::{Bytes, Incoming}, server::conn::http1, service::service_fn, Request, Response};
use parking_lot::{Mutex, RwLock};
use postage::{broadcast, sink::Sink, stream::Stream};
use thiserror::Error;
use tokio::{io, sync::mpsc};

use crate::backing::common::bind_tcp_listener;

use super::common::TryRecvStreamResult;

const GATEWAY_PORT: usize = 8367;
const ALLOWED_ORIGINS: &[&str] = &[
    "http://127.0.0.1",
    "http://localhost",
];

static BROADCAST_CHANNEL: LazyLock<RwLock<broadcast::Sender<Vec<u8>>>> = LazyLock::new(|| {
    let (tx, _) = broadcast::channel(100);
    RwLock::new(tx)
});
static RECEIVING_CHANNEL_TX: OnceLock<RwLock<mpsc::Sender<Vec<u8>>>> = OnceLock::new();
static RECEIVING_CHANNEL_RX: OnceLock<Mutex<mpsc::Receiver<Vec<u8>>>> = OnceLock::new();

/// Possible errors during initialization.
#[derive(Error, Debug)]
pub enum InitError {
    /// Encountered an initialization error in the common code.
    #[error("encountered an initialization error in the common code")]
    CommonInitError(#[from] super::common::InitError),
}
/// Initializes SkyCurrent.
/// 
/// If [`init`] has already been called once before for this thread, this is a no-op.
pub async fn init() -> Result<(), InitError> {
    match bind_tcp_listener(format!("0.0.0.0:{}", GATEWAY_PORT)).await {
        Ok(listener) => {
            let (tx, rx) = mpsc::channel(100);
            
            RECEIVING_CHANNEL_TX.get_or_init(|| RwLock::new(tx));
            RECEIVING_CHANNEL_RX.get_or_init(|| Mutex::new(rx));

            tokio::spawn(async move {
                loop {
                    if let io::Result::Ok((stream, _)) = listener.accept().await {
                        tokio::spawn(async move {
                            let io = hyper_util::rt::TokioIo::new(stream);
                            let conn_fut = http1::Builder::new()
                                .serve_connection(io, service_fn(server_upgrade))
                                .with_upgrades();
                            if let Err(e) = conn_fut.await {
                                eprintln!("An error occurred: {:?}", e);
                            }
                        });
                    }
                }
            });

            Ok(())
        },
        Err(e) => Err(InitError::CommonInitError(e)),
    }
}

async fn server_upgrade(mut req: Request<Incoming>) -> Result<Response<Empty<Bytes>>, WebSocketError> {
    // Check the "Origin" header.
    if let Some(origin) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
        let origin_lower = origin.to_ascii_lowercase();
        if !ALLOWED_ORIGINS.iter().any(|&allowed| allowed == origin_lower || origin_lower.starts_with(&format!("{}:", allowed))) {
            // Return 403 Forbidden if unknown origin.
            return Ok(Response::builder()
                .status(403)
                .body(Empty::new())
                .expect("failed to build response. this should never happen."));
        }
    }  // If no origin header present, request likely ok. (local file or same origin)

    let (response, fut) = upgrade::upgrade(&mut req)?;
    tokio::task::spawn(async move {
        if let Err(e) = tokio::task::unconstrained(handle_client(fut)).await {
            eprintln!("Error in websocket connection: {}", e);
        }
    });

    Ok(response)
}

async fn handle_client(fut: upgrade::UpgradeFut) -> Result<(), WebSocketError> {
    let mut ws = fastwebsockets::FragmentCollector::new(fut.await?);

    let mut broadcast_subscriber = BROADCAST_CHANNEL.read().subscribe();
    let message_forwarder;
    if let Some(receiving_channel) = RECEIVING_CHANNEL_TX.get() {
        message_forwarder = receiving_channel.read().clone();
    } else {
        panic!("`handle_client` will never be called unless `init` has already run, and `init` should have initialized this. this should never happen.");
    }

    loop {
        tokio::select! {
            frame = ws.read_frame() => {
                let frame = frame?;

                match frame.opcode {
                    OpCode::Close => break,
                    OpCode::Binary => {
                        message_forwarder.send(frame.payload.into()).await.expect("receiving channel should not drop until thread itself exits. this should never happen.");
                    },
                    _ => continue
                }
            }
            payload = broadcast_subscriber.recv() => {
                let payload = payload.expect("broadcast source should not drop until thread itself exits. this should never happen.");

                ws.write_frame(Frame::binary(payload.into())).await?;
            }
        }
    }

    Ok(())
}

/// Possible errors during normal operations.
#[derive(Error, Debug)]
pub enum IpcError {
    /// IPC not initialized on this thread.
    #[error("SkyCurrent is not initialized on this thread - call `init` first")]
    NotInitialized,
}

/// Try to receive a payload of arbitrary size.
/// 
/// This function never blocks, instead attempting to get a single accessible message, then returning immediately.
pub fn try_recv_copy() -> Result<TryRecvStreamResult, IpcError> {
    if let Some(receiving_channel_rx) = RECEIVING_CHANNEL_RX.get() {
        match receiving_channel_rx.lock().try_recv() {
            Ok(payload) => Ok(TryRecvStreamResult::NewCompleted(payload)),
            Err(e) => match e {
                mpsc::error::TryRecvError::Empty => Ok(TryRecvStreamResult::OutOfAccessible),
                mpsc::error::TryRecvError::Disconnected => panic!("a sender is always kept undropped as a global variable, so a disconnect should not happen until the entire thread exits. this should never happen."),
            },
        }
    } else {
        Err(IpcError::NotInitialized)
    }
}

/// Receive a payload of arbitrary size asynchronously.
pub async fn recv_copy() -> Result<Vec<u8>, IpcError> {
    if let Some(receiving_channel_rx) = RECEIVING_CHANNEL_RX.get() {
        match receiving_channel_rx.lock().recv().await {
            Some(payload) => Ok(payload),
            None => panic!("a sender is always kept undropped as a global variable, so a disconnect should not happen until the entire thread exits. this should never happen."),
        }
    } else {
        Err(IpcError::NotInitialized)
    }
}

/// Close SkyCurrent.
/// 
/// On some backings, this function is a no-op, but it should always be called before the thread using the library exits to give SkyCurrent a chance to clean up.
pub fn close() {  }

/// Send a payload of arbitrary size.
pub fn send_copy(payload: Vec<u8>) {
    let _ = BROADCAST_CHANNEL.write().send(payload);
}

