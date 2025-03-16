use std::{path::{Path, PathBuf}, sync::{Arc, LazyLock}, thread::{self, JoinHandle}, time::Duration};

use parking_lot::{Mutex, RwLock};
use pollster::FutureExt;
use thiserror::Error;
use bitflags::bitflags;
use bitvec::prelude as bv;

use super::common::{actor_call, actor_join, TryRecvStreamResult};

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum Backing {
    Iox2 = 0,
    WsGateway = 1,
    _Count = 2,
}
impl Backing {
    pub fn from(backing: u8) -> Backing {
        match backing {
            0 => Backing::Iox2,
            1 => Backing::WsGateway,
            _ => panic!("this should never happen.")
        }
    }
}
impl std::fmt::Display for Backing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backing::Iox2 => write!(f, "iox2"),
            Backing::WsGateway => write!(f, "ws-gateway"),
            Backing::_Count => write!(f, "<INVALID>"),
        }
    }
}

#[cfg(feature = "backing-iox2")]
enum Iox2BackingMessage {
    Init {
        path: PathBuf,
        flags: super::iox2::InitFlags,
        respond_to: crossbeam::channel::Sender<Result<(), super::iox2::InitError>>,
    },
    TryRecvStream {
        should_collect: ShouldCollectCallback,
        respond_to: crossbeam::channel::Sender<Result<super::common::TryRecvStreamResult, super::iox2::IpcError>>,
    },
    WaitStream,
    ShutdownSignal {
        respond_to: crossbeam::channel::Sender<Result<(), super::iox2::IpcError>>,
    },
    SendStream {
        payload: Arc<[u8]>,
        header_size: usize,
        respond_to: crossbeam::channel::Sender<Result<(), super::iox2::IpcError>>,
    },
    Close {
        respond_to: crossbeam::channel::Sender<()>,
    },
    _WaitForExit {
        respond_to: tokio::sync::oneshot::Sender<()>,
    },
}

pub type ShouldCollectCallback = fn(&[u8]) -> bool;

static GLOBAL_PROJECT_DIR: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));
static GLOBAL_SHOULD_COLLECT: LazyLock<Mutex<Option<ShouldCollectCallback>>> = LazyLock::new(|| Mutex::new(None));

#[cfg(feature = "backing-iox2")]
static IOX2_RECV_THREAD_JOIN_HANDLE: LazyLock<Mutex<Option<JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(None));
#[cfg(feature = "backing-iox2")]
static IOX2_RECV_SENDER_TX: LazyLock<RwLock<Option<crossbeam::channel::Sender<Iox2BackingMessage>>>> = LazyLock::new(|| RwLock::new(None));
#[cfg(feature = "backing-iox2")]
static IOX2_SEND_THREAD_JOIN_HANDLE: LazyLock<Mutex<Option<JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(None));
#[cfg(feature = "backing-iox2")]
static IOX2_SEND_SENDER_TX: LazyLock<RwLock<Option<crossbeam::channel::Sender<Iox2BackingMessage>>>> = LazyLock::new(|| RwLock::new(None));

static WAIT_STREAM_LOCKS: LazyLock<RwLock<bv::BitVec>> = std::sync::LazyLock::new(|| RwLock::new(bv::bitvec![0; Backing::_Count as usize]));
static WAIT_STREAM_RECEIVER: LazyLock<Mutex<Option<tokio::sync::mpsc::Receiver<Result<Backing, IpcError>>>>> = LazyLock::new(|| Mutex::new(None));

bitflags! {
    /// Flags used during the initialization of SkyCurrent.
    /// 
    /// The following flags are currently supported:
    /// 
    /// - [`InitFlags::INIT_IOX2_RECV`]          (0b00000001): Initialize for receiving on the iceoryx2 backing.
    /// - [`InitFlags::INIT_IOX2_SEND`]          (0b00000010): Initialize for sending on the iceoryx2 backing.
    /// - [`InitFlags::INIT_WS_GATEWAY`]         (0b00000100): Initialize the websocket gateway backing.
    pub struct InitFlags: u32 {
        /// Initialize for receiving on the iceoryx2 backing.
        const INIT_IOX2_RECV = 0b00000001;

        /// Initialize for sending on the iceoryx2 backing.
        const INIT_IOX2_SEND = 0b00000010;

        /// Initialize the websocket gateway backing.
        const INIT_WS_GATEWAY = 0b00000100;
    }
}

/// Sets the global project directory, required for certain backings.
pub fn set_global_project_dir(project_dir: impl AsRef<Path>) {
    *GLOBAL_PROJECT_DIR.lock() = Some(project_dir.as_ref().to_path_buf());
}

/// Sets the global `should_collect` callback, required for certain backings.
pub fn set_global_should_collect(should_collect: ShouldCollectCallback) {
    *GLOBAL_SHOULD_COLLECT.lock() = Some(should_collect);
}

/// Possible errors during initialization.
#[derive(Error, Debug, Clone)]
pub enum InitError {
    /// Some of the enabled backings require a global project directory to be set. Set one with [`set_global_project_dir`] first before calling [`init`].
    #[error("backing {0:?} requires a global project directory! set one with `set_global_project_dir` first before calling `init`")]
    MissingGlobalProjectDirectory(String),
    /// Encountered an initialization error in the common code.
    #[error("encountered an initialization error in the common code")]
    CommonInitError(#[from] super::common::InitError),
    /// Backing crashed during initialization routine.
    #[error("backing '{0}' crashed during initialization routine")]
    BackingCrashed(Backing),
    /// Failed to initialize the iceoryx2 backing.
    #[cfg(feature = "backing-iox2")]
    #[error("failed to initialize iceoryx2 backing")]
    Iox2InitError(#[from] super::iox2::InitError),
    /// Failed to initialize the websocket gateway backing.
    #[cfg(feature = "backing-ws-gateway")]
    #[error("failed to initialize websocket gateway backing")]
    WsGatewayInitError(#[from] super::ws_gateway::InitError),
}
/// Initializes SkyCurrent with the given flags.
/// 
/// Certain backings require a project directory. Use [`set_global_project_dir`] to set that first.
/// 
/// If [`init`] has already been called once before for this thread, unless [`close`] was called, this is a no-op.
pub async fn init(flags: InitFlags) -> Result<(), InitError> {
    let project_dir = (&*GLOBAL_PROJECT_DIR.lock()).clone();

    // Create special communication channels for `wait_stream`.
    // This is because the `wait_stream`'s of the backings, unlike the other actor methods, need to all send to a single consumer, our combined `wait_stream` method.
    let (wait_stream_tx, wait_stream_rx) = tokio::sync::mpsc::channel(100);
    *WAIT_STREAM_RECEIVER.lock() = Some(wait_stream_rx);

    // Initialize iceoryx2 backing.
    #[cfg(feature = "backing-iox2")]
    {
        let build_actor_main_loop = |sender_rx: crossbeam::channel::Receiver<Iox2BackingMessage>, respond_to_wait_stream: Option<tokio::sync::mpsc::Sender<Result<Backing, IpcError>>>| {
            move || {
                // Listen for calls.
                // Note that we are using blocking operations because this backend requires it.
                let mut holding_until_exit = Vec::with_capacity(1);
                while let Ok(call) = sender_rx.recv() {
                    match call {
                        Iox2BackingMessage::Init { path, flags, respond_to } => {
                            let _ = respond_to.send(super::iox2::init(&path, flags));
                        },
                        Iox2BackingMessage::TryRecvStream { should_collect, respond_to } => {
                            let _ = respond_to.send(super::iox2::try_recv_stream(should_collect));
                        },
                        Iox2BackingMessage::WaitStream => {
                            if let Some(respond_to_wait_stream) = respond_to_wait_stream.as_ref() {
                                let _ = respond_to_wait_stream.blocking_send(super::iox2::wait_stream().map(|_| Backing::Iox2).map_err(|e| IpcError::Iox2IpcError(e)));
                            } else {
                                panic!("this should never happen.");
                            }
                        },
                        Iox2BackingMessage::ShutdownSignal { respond_to } => {
                            let _ = respond_to.send(super::iox2::shutdown_signal());
                        },
                        Iox2BackingMessage::SendStream { payload, header_size, respond_to } => {
                            let _ = respond_to.send(super::iox2::send_stream(&payload, header_size));
                        },
                        Iox2BackingMessage::Close { respond_to } => {
                            let _ = respond_to.send(super::iox2::close());
                            break;
                        },
                        Iox2BackingMessage::_WaitForExit { respond_to } => {
                            // When the thread exits, all oneshot channels still being held will close, thereby providing a signal for those waiting for this thread to exit.
                            holding_until_exit.push(respond_to);
                        }
                    }
                }
            }
        };
        if let Some(global_project_dir) = project_dir.clone() {
            if flags.intersects(InitFlags::INIT_IOX2_RECV) {
                let iox2_flags = super::iox2::InitFlags::IOX2_CREAT_SUBSCRIBER | super::iox2::InitFlags::IOX2_CREAT_LISTENER;
    
                // Create communication channels.
                let (sender_tx, sender_rx) = crossbeam::channel::bounded::<Iox2BackingMessage>(100);
                let respond_to_wait_stream = wait_stream_tx.clone();
    
                // Create thread.
                let join_handle = thread::spawn(build_actor_main_loop(sender_rx, Some(respond_to_wait_stream)));
                
                // Store the join handle.
                *IOX2_RECV_THREAD_JOIN_HANDLE.lock() = Some(join_handle);

                // Initialize.
                let (send, recv) = crossbeam::channel::bounded(1);
                if let Err(_) = sender_tx.send(Iox2BackingMessage::Init { path: global_project_dir.clone(), flags: iox2_flags, respond_to: send }) { return Err(InitError::BackingCrashed(Backing::Iox2)); }
                if let Ok(result) = recv.recv() { result?; } else { return Err(InitError::BackingCrashed(Backing::Iox2)); }
                drop(recv);
    
                // Store the sender.
                *IOX2_RECV_SENDER_TX.write() = Some(sender_tx);
            }
            if flags.intersects(InitFlags::INIT_IOX2_SEND) {
                let iox2_flags = super::iox2::InitFlags::IOX2_CREAT_PUBLISHER | super::iox2::InitFlags::IOX2_CREAT_NOTIFIER;
    
                // Create communication channels.
                let (sender_tx, sender_rx) = crossbeam::channel::bounded::<Iox2BackingMessage>(100);
    
                // Create thread.
                let join_handle = thread::spawn(build_actor_main_loop(sender_rx, None));
    
                // Store the join handle.
                *IOX2_SEND_THREAD_JOIN_HANDLE.lock() = Some(join_handle);

                // Initialize.
                let (send, recv) = crossbeam::channel::bounded(1);
                if let Err(_) = sender_tx.send(Iox2BackingMessage::Init { path: global_project_dir, flags: iox2_flags, respond_to: send }) { return Err(InitError::BackingCrashed(Backing::Iox2)); }
                if let Ok(result) = recv.recv() { result?; } else { return Err(InitError::BackingCrashed(Backing::Iox2)); }
                drop(recv);
    
                // Store the sender.
                *IOX2_SEND_SENDER_TX.write() = Some(sender_tx);
            }
        } else {
            return Err(InitError::MissingGlobalProjectDirectory(String::from("iox2")));
        }
    }

    // Initialize websocket gateway backing.
    #[cfg(feature = "backing-ws-gateway")]
    if flags.intersects(InitFlags::INIT_WS_GATEWAY) {
        super::ws_gateway::init().await?;
    }

    Ok(())
}

/// Possible errors during normal operations.
#[derive(Error, Debug, Clone)]
pub enum IpcError {
    /// IPC not initialized on this thread.
    #[error("SkyCurrent is not initialized on this thread - call `init` first")]
    NotInitialized,
    /// Some of the enabled backings require a global `should_collect` callback to be set. Set one with [`set_global_should_collect`] first before calling [`try_recv_stream`] or [`recv_stream`].
    #[error("backing {0:?} requires a global `should_collect` callback! set one with `set_global_should_collect` first before calling `try_recv_stream` or `recv_stream`")]
    MissingGlobalShouldCollect(String),
    /// Backing crashed or is not started.
    #[error("{0}: backing crashed or is not started")]
    BackingCrashedOrNotStarted(&'static str),
    /// IPC error encountered in the iceoryx2 backing.
    #[cfg(feature = "backing-iox2")]
    #[error("ipc error encountered in the iceoryx2 backing")]
    Iox2IpcError(#[from] super::iox2::IpcError),
    /// IPC error encountered in the websocket gateway backing.
    #[cfg(feature = "backing-ws-gateway")]
    #[error("ipc error encountered in the websocket gateway backing")]
    WsGatewayIpcError(#[from] super::ws_gateway::IpcError),
}

static CURRENT_BACKING: LazyLock<Mutex<(Backing, std::time::Instant)>> = LazyLock::new(|| Mutex::new((Backing::_Count, std::time::Instant::now())));

/// Try to receive a payload of arbitrary size.
/// 
/// On certain backings, this requires assembly of data on the receiver-side; thus a global `should_collect` callback might need to be set. The header of each incoming chunk is supplied to this callback, and with it the callback should indicate whether it wants the backing to collect said chunk and start a local merge.
/// 
/// This function never blocks, instead attempting to get a single accessible page and merging it if told to do so, then returning immediately.
/// 
/// # Implementation Notes
/// 
/// Internally, each backing, once they return a non-[`TryRecvStreamResult::OutOfAccessible`] value, will get a 10ms period of time where subsequent [`try_recv_stream`] calls will first ask that backing for the next message.
/// 
/// When time is up, the next backing that returns a non-[`TryRecvStreamResult::OutOfAccessible`] value will be asked first, and so on.
/// 
/// This alloted time can end early if the backing, at any point during it, returns [`TryRecvStreamResult::OutOfAccessible`].
/// 
/// Each backing will only ever be queried once per call to [`try_recv_stream`], and if they all return [`TryRecvStreamResult::OutOfAccessible`], the same will be returned.
pub fn try_recv_stream() -> Result<TryRecvStreamResult, IpcError> {
    let should_collect = (*GLOBAL_SHOULD_COLLECT.lock()).clone();

    let try_recv_stream = |backing: Backing, should_collect: &Option<ShouldCollectCallback>| {
        match backing {
            #[cfg(feature = "backing-iox2")]
            Backing::Iox2 => {
                let (send, recv) = crossbeam::channel::bounded(1);
                actor_call!(IOX2_RECV_SENDER_TX, Iox2BackingMessage::TryRecvStream { should_collect: should_collect.ok_or(IpcError::MissingGlobalShouldCollect(String::from("iox2")))?, respond_to: send }, recv)
            },
            #[cfg(not(feature = "backing-iox2"))]
            Backing::Iox2 => panic!("not built with backing '{}'. this should never happen.", backing),
            #[cfg(feature = "backing-ws-gateway")]
            Backing::WsGateway => super::ws_gateway::try_recv_copy().map(|try_recv_copy_result| {
                match try_recv_copy_result {
                    super::common::TryRecvCopyResult::NewMessage(items) => TryRecvStreamResult::NewCompleted((items, 0)),
                    super::common::TryRecvCopyResult::OutOfAccessible => TryRecvStreamResult::OutOfAccessible,
                }
            }).map_err(|e| IpcError::WsGatewayIpcError(e)),
            #[cfg(not(feature = "backing-ws-gateway"))]
            Backing::WsGateway => panic!("not built with backing '{}'. this should never happen.", backing),
            Backing::_Count => panic!("this should never happen."),
        }
    };

    let (current_backing, entered) = &mut *CURRENT_BACKING.lock();

    // Determine which backing to start on.
    if matches!(current_backing, Backing::_Count) {
        *current_backing = Backing::Iox2;
        *entered = std::time::Instant::now();
    } else if (std::time::Instant::now() - *entered) > Duration::from_millis(10) {
        *current_backing = Backing::from(((*current_backing as usize + 1) % (Backing::_Count as usize)) as u8);
        *entered = std::time::Instant::now();
    }  // Otherwise, previous backing still has precedence.

    // Before trying any of the backings, drain any `wait_stream` results to update the list of backings not currently blocked and unable to respond to our calls.
    wait_stream_drain()?;

    // Try backings.
    for _ in 0..Backing::_Count as usize {
        // IMPORTANT: Make sure to *not* do `try_recv_stream` if this backing is currently blocked on `wait_stream`. This will block our try call which is not what we want at all.
        let blocked = WAIT_STREAM_LOCKS.read()[*current_backing as usize];
        if !blocked {
            #[cfg(not(feature = "backing-iox2"))]
            if matches!(current_backing, Backing::Iox2) {
                continue;
            }
            #[cfg(not(feature = "backing-ws-gateway"))]
            if matches!(current_backing, Backing::WsGateway) {
                continue;
            }
            match try_recv_stream(*current_backing, &should_collect)? {
                TryRecvStreamResult::NewCompleted((mut payload, header_size)) => {
                    match current_backing {
                        Backing::Iox2 => {
                            #[cfg(feature = "backing-ws-gateway")]
                            {
                                // When we receive a message on this backing, we want to forward it to the websocket gateway backing.
                                let mut payload_copy = payload.clone();
                                payload_copy.extend_from_slice(&(header_size as u64).to_le_bytes());
                                super::ws_gateway::send_copy(payload_copy);
                            }

                            return Ok(TryRecvStreamResult::NewCompleted((payload, header_size)));
                        },
                        Backing::WsGateway => {
                            // For the websocket gateway backing, any messages received are not immediately given to the client, but instead sent to the mesh (iox2) backing.
                            // Once the echo of that message comes back, we will hear the message proper.
                            // This mirrors the behavior of sending directly with [`send_stream`].
                            // This is where the naming of the websocket gateway client as the websocket leaf comes from.
                            #[cfg(feature = "backing-iox2")]
                            macro_rules! blocking_retry_after_small_timeout {
                                ($error:ident) => {{
                                    eprintln!("{:?}", $error);
                                    eprintln!("Retrying in 100ms...");
                                    thread::sleep(Duration::from_millis(100));
                                }};
                            }

                            // Last 8 bytes of websocket payload should be header size (little-endian).
                            let header_size = u64::from_le_bytes((&payload)[payload.len()-8..].try_into().unwrap()) as usize;
                            
                            #[cfg(not(feature = "backing-iox2"))]
                            #[cfg(feature = "backing-ws-gateway")]
                            {
                                super::ws_gateway::send_copy(payload.clone());
                                payload.truncate(payload.len()-8);
                                return Ok(TryRecvStreamResult::NewCompleted((payload, header_size)));
                            }
                            
                            // Cut off last 8 bytes to get the actual payload.
                            payload.truncate(payload.len()-8);

                            loop {
                                #[cfg(feature = "backing-iox2")]
                                match send_stream(&payload, header_size) {
                                    Ok(_) => break,
                                    Err(e) => match e {
                                        #[cfg(feature = "backing-iox2")]
                                        IpcError::Iox2IpcError(e) => match e {
                                            crate::backing::iox2::IpcError::Iox2NotifierNotifyError(_) => blocking_retry_after_small_timeout!(e),
                                            crate::backing::iox2::IpcError::Iox2PublisherLoanError(_) => blocking_retry_after_small_timeout!(e),
                                            crate::backing::iox2::IpcError::Iox2PublisherSendError(_) => blocking_retry_after_small_timeout!(e),
                                            _ => panic!("{:?}", e),
                                        },
                                        _ => panic!("{:?}", e),
                                    },
                                }
                            }

                            return Ok(TryRecvStreamResult::PotentiallyAvailable);
                        },
                        Backing::_Count => panic!("this should never happen."),
                    }
                },
                TryRecvStreamResult::PotentiallyAvailable => {
                    return Ok(TryRecvStreamResult::PotentiallyAvailable);
                },
                TryRecvStreamResult::OutOfAccessible => {
                    // Backing's buffer was empty, try the next one.
                    *current_backing = Backing::from(((*current_backing as usize + 1) % (Backing::_Count as usize)) as u8);
                    *entered = std::time::Instant::now();
                },
            }
        }
    }

    Ok(TryRecvStreamResult::OutOfAccessible)
}

/// Wait until new item is available on stream asynchronously.
/// 
/// Returns early if the shutdown signal is received, but only if that signal is sent from a different sending thread.
/// 
/// Only one thread can be calling [`wait_stream`] at a time. The behavior when it is called at the same time from more than one thread is undefined.
/// 
/// When using in a loop to receive messages, call to get a Future first before calling [`try_recv_stream`], and await the Future if [`try_recv_stream`] returns with [`TryRecvStreamResult::OutOfAccessible`].
/// 
/// # Implementation Notes
/// 
/// Internally, calling this function will always cause every receiver backing to get blocked on their own `wait_stream` functions.
/// 
/// Thus, [`try_recv_stream`] will only ask those backings which are not currently blocked on `wait_stream` for new messages.
pub async fn wait_stream() -> Result<(), IpcError> {
    // We avoid calling `wait_stream` on backings that we know are still blocked on it.
    // We also send the requests out first, potentially missing out on backings that are now free to process the request because we want as fewest number of threads stuck at `wait_stream` as possible. This makes a cleaner exit for each thread more likely, though it's only an extra precaution in practice because the shutdown signal exists.
    {
        let mut cell = WAIT_STREAM_LOCKS.write();

        // IOX2_RECV_SENDER_TX
        #[cfg(feature = "backing-iox2")]
        if !cell[Backing::Iox2 as usize] {
            actor_call!(IOX2_RECV_SENDER_TX, Iox2BackingMessage::WaitStream)?;
            cell.set(Backing::Iox2 as usize, true);
        }
    }
    if let Some(wait_stream_rx) = WAIT_STREAM_RECEIVER.lock().as_mut() {
        #[cfg(feature = "backing-iox2")]
        #[cfg(not(feature = "backing-ws-gateway"))]
        {
            // Wait until at least one `wait_stream` response comes back.
            if let Some(backing) = wait_stream_rx.recv().await {
                handle_wait_stream_return(backing)?;
            }
            // Now, we drain the receiving end to get out any remaining easily accessible responses.
            while let Ok(backing) = wait_stream_rx.try_recv() {
                handle_wait_stream_return(backing)?;
            }
        }
        #[cfg(feature = "backing-iox2")]
        #[cfg(feature = "backing-ws-gateway")]
        tokio::select! {
            backing_some = wait_stream_rx.recv() => {
                // Wait until at least one `wait_stream` response comes back.
                if let Some(backing) = backing_some {
                    handle_wait_stream_return(backing)?;
                }
                // Now, we drain the receiving end to get out any remaining easily accessible responses.
                while let Ok(backing) = wait_stream_rx.try_recv() {
                    handle_wait_stream_return(backing)?;
                }
            }
            _ = super::ws_gateway::wait_stream() => {
                // If the ws-gateway backing returned, there's no need to set the WAIT_STREAM_LOCKS flag since it was never blocked anyways.
            }
        }
        Ok(())
    } else {
        Err(IpcError::NotInitialized)
    }
}
fn handle_wait_stream_return(backing: Result<Backing, IpcError>) -> Result<(), IpcError> {
    let backing = backing?;

    // Make sure to make note that this specific backing's `wait_stream` has now returned.

    // Running multiple backings require multiple threads.
    // This presents a challenge when implementing `wait_stream`, as any FFI users will still expect it to function as it does for individual backings.
    // Namely, it should return if any one of the backings unblocks.
    // To achieve this, we keep track of if each backing is currently blocked on `wait_stream` using a BitVec.
    WAIT_STREAM_LOCKS.write().set(backing as usize, false);

    Ok(())
}
fn wait_stream_drain() -> Result<(), IpcError> {
    if let Some(mut lock) = WAIT_STREAM_RECEIVER.try_lock() {
        if let Some(wait_stream_rx) = lock.as_mut() {
            // Drain the receiving end to get out any easily accessible responses.
            while let Ok(backing) = wait_stream_rx.try_recv() {
                handle_wait_stream_return(backing)?;
            }
            Ok(())
        } else {
            Err(IpcError::NotInitialized)
        }
    } else {
        Ok(())
    }
}

/// Sends a shutdown signal.
/// 
/// Note that this won't actually shut anything down! Do that by calling [`close`].
/// 
/// This simply causes certain blocking functions to return early. Most notably, [`wait_stream`] should return early when this signal is sent.
pub fn shutdown_signal() -> Result<(), IpcError> {
    // IOX2_SEND_SENDER_TX
    #[cfg(feature = "backing-iox2")]
    {
        let (send, recv) = crossbeam::channel::bounded(1);
        actor_call!(IOX2_SEND_SENDER_TX, Iox2BackingMessage::ShutdownSignal { respond_to: send }, recv)?;
    }

    // websocket gateway backing does not need a shutdown signal to break out of `wait_stream` since it isn't blocking.

    Ok(())
}

/// Close SkyCurrent.
/// 
/// On some backings, this function is a no-op, but it should always be called before the thread using the library exits to give SkyCurrent a chance to clean up.
pub fn close() {
    // IOX2_RECV_SENDER_TX (part 1)
    #[cfg(feature = "backing-iox2")]
    {
        let (send, _) = crossbeam::channel::bounded(1);
        match actor_call!(IOX2_RECV_SENDER_TX, Iox2BackingMessage::Close { respond_to: send }) {
            Err(e) => match e {
                IpcError::NotInitialized | IpcError::BackingCrashedOrNotStarted(_) => (),
                IpcError::Iox2IpcError(_) => panic!("iox2 backend close is a no-op. this should never happen."),
                #[cfg(feature = "backing-ws-gateway")]
                IpcError::WsGatewayIpcError(_) => panic!("ws-gateway backend close is a no-op. this should never happen."),
                IpcError::MissingGlobalShouldCollect(_) => panic!("`IpcError::MissingGlobalShouldCollect` should not be possible from trying to call `close`. this should never happen."),
            },
            Ok(_) => (),
        }
        let _ = IOX2_RECV_SENDER_TX.write().take();
    }

    // All possibly blocked threads have now been sent a Close actor call. Now to get them to break out of their blocked states, we send the shutdown signal.
    while let Err(e) = shutdown_signal() {
        match e {
            IpcError::NotInitialized => {
                eprintln!("WARNING: combined runtime was not initialized with the ability to send shutdown signals; `close` will block until `wait_stream` calls currently executing in other threads naturally exits!");
                break;
            },
            IpcError::BackingCrashedOrNotStarted(_) => {
                eprintln!("WARNING: some backings were not running or crashed while delivering the shutdown signal; `close` will block until `wait_stream` calls currently executing in other threads naturally exits!");
                break;
            },
            #[cfg(feature = "backing-iox2")]
            IpcError::Iox2IpcError(e) => match e {
                super::iox2::IpcError::NotInitialized => panic!("if send flag was set, the thread meant for iox2 sending would definitely have been initialized for sending. this should never happen."),
                super::iox2::IpcError::Iox2NotifierNotifyError(notifier_notify_error) => {
                    eprintln!("WARNING: iox2 backend notify error encountered while trying to send the shutdown signal; will retry in 100ms! {:?}", notifier_notify_error);

                    // Sleep for a while, then retry.
                    thread::sleep(Duration::from_millis(100));

                    continue;
                },
                e => panic!("error '{:?}' should not be possible from trying to send a shutdown signal. this should never happen.", e),
            },
            #[cfg(feature = "backing-ws-gateway")]
            IpcError::WsGatewayIpcError(e) => panic!("error '{:?}' should not be possible from trying to send a shutdown signal. the ws-gateway backing does not even need the shutdown signal. this should never happen.", e),
            IpcError::MissingGlobalShouldCollect(_) => panic!("`IpcError::MissingGlobalShouldCollect` should not be possible from trying to call `close`. this should never happen."),
        }
    }

    // Join all receiver threads.
    // IOX2_RECV_SENDER_TX (part 2)
    #[cfg(feature = "backing-iox2")]
    {
        actor_join!(IOX2_RECV_THREAD_JOIN_HANDLE);
    }

    // IOX2_SEND_SENDER_TX
    #[cfg(feature = "backing-iox2")]
    {
        let (send, _) = crossbeam::channel::bounded(1);
        match actor_call!(IOX2_SEND_SENDER_TX, Iox2BackingMessage::Close { respond_to: send }) {
            Err(e) => match e {
                IpcError::NotInitialized | IpcError::BackingCrashedOrNotStarted(_) => (),
                IpcError::Iox2IpcError(_) => panic!("iox2 backend close is a no-op. this should never happen."),
                #[cfg(feature = "backing-ws-gateway")]
                IpcError::WsGatewayIpcError(_) => panic!("ws-gateway backend close is a no-op. this should never happen."),
                IpcError::MissingGlobalShouldCollect(_) => panic!("`IpcError::MissingGlobalShouldCollect` should not be possible from trying to call `close`. this should never happen."),
            },
            Ok(_) => (),
        }
        let _ = IOX2_SEND_SENDER_TX.write().take();
        actor_join!(IOX2_SEND_THREAD_JOIN_HANDLE);
    }

    // Reset WAIT_STREAM_LOCKS to zeroes.
    WAIT_STREAM_LOCKS.write().fill(false);

    // Reset WAIT_STREAM_RECEIVER.
    *WAIT_STREAM_RECEIVER.lock() = None;

    // Reset CURRENT_BACKING.
    {
        let (current_backing, entered) = &mut *CURRENT_BACKING.lock();
        *current_backing = Backing::_Count;
        *entered = std::time::Instant::now();
    }
}

/// Receive a payload of arbitrary size asynchronously.
/// 
/// On certain backings, this requires assembly of data on the receiver-side; thus a global `should_collect` callback might need to be set. The header of each incoming chunk is supplied to this callback, and with it the callback should indicate whether it wants the backing to collect said chunk and start a local merge.
/// 
/// Only one thread can be calling [`recv_stream`] at a time (as [`recv_stream`] calls [`wait_stream`] under-the-hood). The behavior when it is called at the same time from more than one thread is undefined.
pub async fn recv_stream() -> Result<Vec<u8>, IpcError> {
    loop {
        // Get a future in case we run out of accessible messages in the next part.
        let future = wait_stream();
        match try_recv_stream() {
            Ok(did_finish_new_merged_message_before_ran_out_of_accessible_pages) => match did_finish_new_merged_message_before_ran_out_of_accessible_pages {
                super::common::TryRecvStreamResult::NewCompleted(merged_message) => return Ok(merged_message.0),
                super::common::TryRecvStreamResult::PotentiallyAvailable => {
                    #[cfg(feature = "tokio")]
                    {
                        tokio::task::yield_now().await;  // Provide tokio with an yield point.
                    }
                    continue;
                },
                super::common::TryRecvStreamResult::OutOfAccessible => {
                    future.await?;
                }
            },
            Err(e) => return Err(e),
        }
    }
}

/// Send an arbitrarily-sized payload.
/// 
/// Since this might require reassembly of data on the receiver-side on certain backings, all payloads should have a small header section so that receivers can decide if they want to reconstruct the message or pass on it, saving memory and execution time.
/// 
/// The `header_size` specifies how many bytes from the head of the payload corresponds to the header; every fragment sent will include the header but have different sections of the body.
pub fn send_stream(payload: &[u8], header_size: usize) -> Result<(), IpcError> {
    let payload_arc: Arc<[u8]> = Arc::from(payload);

    // IOX2_SEND_SENDER_TX
    #[cfg(feature = "backing-iox2")]
    {
        let (send, recv) = crossbeam::channel::bounded(1);
        actor_call!(IOX2_SEND_SENDER_TX, Iox2BackingMessage::SendStream { payload: payload_arc.clone(), header_size, respond_to: send }, recv)?;
    }

    // Websocket gateway will get sent the data automatically once we hear back our own echo.
    // Unless, that is, ws-gateway is the only backing enabled.
    #[cfg(not(feature = "backing-iox2"))]
    #[cfg(feature = "backing-ws-gateway")]
    {
        let mut payload_copy = Vec::from(payload);
        payload_copy.extend_from_slice(&(header_size as u64).to_le_bytes());
        super::ws_gateway::send_copy_to_self(payload_copy)?;
    }

    Ok(())
}

#[cfg(feature = "tokio")]
#[cfg(feature = "backing-iox2")]
pub(crate) async fn _wait_for_possible_crashes() {
    let iox2_recv_thread;
    {
        let send;
        (send, iox2_recv_thread) = tokio::sync::oneshot::channel();
        if let Err(_) = actor_call!(IOX2_RECV_SENDER_TX, Iox2BackingMessage::_WaitForExit { respond_to: send }) {
            return;
        }
    }
    let iox2_send_thread;
    {
        let send;
        (send, iox2_send_thread) = tokio::sync::oneshot::channel();
        if let Err(_) = actor_call!(IOX2_SEND_SENDER_TX, Iox2BackingMessage::_WaitForExit { respond_to: send }) {
            return;
        }
    }
    tokio::select! {
        _ = iox2_recv_thread => {}
        _ = iox2_send_thread => {}
    }
}

/// Blocking version of [`wait_stream`].
pub fn blocking_wait_stream() -> Result<(), IpcError> {
    wait_stream().block_on()
}

/// Blocking version of [`recv_stream`].
pub fn blocking_recv_stream() -> Result<Vec<u8>, IpcError> {
    recv_stream().block_on()
}

