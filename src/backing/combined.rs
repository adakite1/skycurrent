use std::{cell::LazyCell, path::{Path, PathBuf}, sync::{Arc, LazyLock}, thread::{self, JoinHandle}, time::Duration};

use parking_lot::{Mutex, RwLock};
use thiserror::Error;
use bitflags::bitflags;
use bitvec::prelude as bv;

use super::common::{actor_call, actor_join, TryRecvStreamResult};

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum Backing {
    Iox2 = 0,
    _Count = 1
}
impl Backing {
    pub fn from(backing: u8) -> Backing {
        match backing {
            0 => Backing::Iox2,
            _ => panic!("this should never happen.")
        }
    }
}
impl std::fmt::Display for Backing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backing::Iox2 => write!(f, "iox2"),
            Backing::_Count => write!(f, "<INVALID>"),
        }
    }
}

enum Iox2BackingMessage {
    Init {
        path: PathBuf,
        flags: super::iox2::InitFlags,
        respond_to: crossbeam::channel::Sender<Result<(), super::iox2::InitError>>,
    },
    TryRecvStream {
        should_collect: Box<dyn FnMut(&[u8]) -> bool + Send + 'static>,
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
        respond_to: crossbeam::channel::Sender<Result<(), super::iox2::IpcError>>,
    },
}

static GLOBAL_PROJECT_DIR: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));
static IOX2_RECV_THREAD_JOIN_HANDLE: LazyLock<Mutex<Option<JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(None));
static IOX2_RECV_SENDER_TX: LazyLock<RwLock<Option<crossbeam::channel::Sender<Iox2BackingMessage>>>> = LazyLock::new(|| RwLock::new(None));
static IOX2_SEND_THREAD_JOIN_HANDLE: LazyLock<Mutex<Option<JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(None));
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
}
/// Initializes SkyCurrent with the given flags.
/// 
/// Certain backings require a project directory. Use [`set_global_project_dir`] to set that first.
/// 
/// If [`init`] has already been called once before for this thread, this is a no-op.
pub fn init(flags: InitFlags) -> Result<(), InitError> {
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
                while let Ok(call) = sender_rx.recv() {
                    match call {
                        Iox2BackingMessage::Init { path, flags, respond_to } => {
                            respond_to.send(super::iox2::init(&path, flags));
                        },
                        Iox2BackingMessage::TryRecvStream { should_collect, respond_to } => {
                            respond_to.send(super::iox2::try_recv_stream(should_collect));
                        },
                        Iox2BackingMessage::WaitStream => {
                            if let Some(respond_to_wait_stream) = respond_to_wait_stream.as_ref() {
                                respond_to_wait_stream.blocking_send(super::iox2::wait_stream().map(|_| Backing::Iox2).map_err(|e| IpcError::Iox2IpcError(e)));
                            } else {
                                panic!("this should never happen.");
                            }
                        },
                        Iox2BackingMessage::ShutdownSignal { respond_to } => {
                            respond_to.send(super::iox2::shutdown_signal());
                        },
                        Iox2BackingMessage::SendStream { payload, header_size, respond_to } => {
                            respond_to.send(super::iox2::send_stream(&payload, header_size));
                        },
                        Iox2BackingMessage::Close { respond_to } => {
                            respond_to.send(super::iox2::close());
                            break;
                        },
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

    Ok(())
}

/// Possible errors during normal operations.
#[derive(Error, Debug, Clone)]
pub enum IpcError {
    /// IPC not initialized on this thread.
    #[error("SkyCurrent is not initialized on this thread - call `init` first")]
    NotInitialized,
    /// Backing crashed during call.
    #[error("{0}: backing crashed during call")]
    BackingCrashed(&'static str),
    /// IPC error encountered in the iceoryx2 backing.
    #[cfg(feature = "backing-iox2")]
    #[error("ipc error encountered in the iceoryx2 backing")]
    Iox2IpcError(#[from] super::iox2::IpcError),
}

static CURRENT_BACKING: LazyLock<Mutex<(Backing, std::time::Instant)>> = LazyLock::new(|| Mutex::new((Backing::_Count, std::time::Instant::now())));

/// Try to receive a payload of arbitrary size.
/// 
/// Note that since this requires assembly of data on the receiver-side, the `should_collect` callback is supplied with the header of each incoming chunk so that it may indicate whether it wants to collect said chunk and start a local merge.
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
pub fn try_recv_stream<I: FnMut(&[u8]) -> bool + Send + 'static + Clone>(should_collect: I) -> Result<TryRecvStreamResult, IpcError> {
    let should_collect = Box::from(should_collect);

    let try_recv_stream = |backing: Backing, should_collect: Box<I>| {
        let (send, recv) = crossbeam::channel::bounded(1);
        match backing {
            Backing::Iox2 => actor_call!(IOX2_RECV_SENDER_TX, Iox2BackingMessage::TryRecvStream { should_collect, respond_to: send }, recv),
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
            match try_recv_stream(*current_backing, should_collect.clone())? {
                TryRecvStreamResult::NewCompleted(payload) => {
                    return Ok(TryRecvStreamResult::NewCompleted(payload));
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
/// Will return early if the shutdown signal is received, but only if that signal is sent from a different sending thread.
/// 
/// Note that only one thread can be calling [`wait_stream`] at a time. The behavior when it is called at the same time from more than one thread is undefined.
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
        if !cell[Backing::Iox2 as usize] {
            actor_call!(IOX2_RECV_SENDER_TX, Iox2BackingMessage::WaitStream)?;
            cell.set(Backing::Iox2 as usize, true);
        }
    }
    if let Some(wait_stream_rx) = WAIT_STREAM_RECEIVER.lock().as_mut() {
        // Wait until at least one `wait_stream` response comes back.
        if let Some(backing) = wait_stream_rx.recv().await {
            handle_wait_stream_return(backing)?;
        }
        // Now, we drain the receiving end to get out any remaining easily accessible responses.
        while let Ok(backing) = wait_stream_rx.try_recv() {
            handle_wait_stream_return(backing)?;
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
    let (send, recv) = crossbeam::channel::bounded(1);
    actor_call!(IOX2_SEND_SENDER_TX, Iox2BackingMessage::ShutdownSignal { respond_to: send }, recv)?;

    Ok(())
}

/// Close SkyCurrent.
/// 
/// On some backings, this function is a no-op, but it should always be called before the thread using the library exits to give SkyCurrent a chance to clean up.
pub fn close() -> Result<(), IpcError> {
    // IOX2_RECV_SENDER_TX (part 1)
    let (send, iox2_recv_close_recv) = crossbeam::channel::bounded(1);
    match actor_call!(IOX2_RECV_SENDER_TX, Iox2BackingMessage::Close { respond_to: send }) {
        Err(e) => match e {
            IpcError::NotInitialized | IpcError::BackingCrashed(_) => (),
            IpcError::Iox2IpcError(_) => panic!("iox2 backend close is a no-op. this should never happen."),
        },
        Ok(_) => (),
    }
    let _ = IOX2_RECV_SENDER_TX.write().take();

    // All possibly blocked threads have now been sent a Close actor call. Now to get them to break out of their blocked states, we send the shutdown signal.
    while let Err(e) = shutdown_signal() {
        match e {
            IpcError::NotInitialized => {
                eprintln!("WARNING: combined runtime was not initialized with the ability to send shutdown signals; `close` will block until `wait_stream` calls currently executing in other threads naturally exits!");
                break;
            },
            IpcError::BackingCrashed(_) => {
                eprintln!("WARNING: some backings crashed while delivering the shutdown signal; `close` will block until `wait_stream` calls currently executing in other threads naturally exits!");
                break;
            },
            IpcError::Iox2IpcError(e) => match e {
                super::iox2::IpcError::NotInitialized => panic!("if send flag was set, the thread meant for iox2 sending would definitely have been initialized for sending. this should never happen."),
                super::iox2::IpcError::Iox2NotifierNotifyError(notifier_notify_error) => {
                    eprintln!("WARNING: iox2 backend notify error encountered while trying to send the shutdown signal; will retry in 100ms! {:?}", notifier_notify_error);

                    // Sleep for a while, then retry.
                    thread::sleep(Duration::from_millis(100));

                    continue;
                },
                e => panic!("error '{:?}' should not be possible from trying to send a shutdown signal. this should never happen.", e)
            },
        }
    }

    // Join all receiver threads.
    // IOX2_RECV_SENDER_TX (part 2)
    let _ = iox2_recv_close_recv.recv();
    actor_join!(IOX2_RECV_THREAD_JOIN_HANDLE)?;

    // IOX2_SEND_SENDER_TX
    let (send, recv) = crossbeam::channel::bounded(1);
    actor_call!(IOX2_SEND_SENDER_TX, Iox2BackingMessage::Close { respond_to: send }, recv)?;
    let _ = IOX2_SEND_SENDER_TX.write().take();
    actor_join!(IOX2_SEND_THREAD_JOIN_HANDLE)?;

    // Reset WAIT_STREAM_LOCKS to zeroes.
    WAIT_STREAM_LOCKS.write().fill(false);

    // Reset WAIT_STREAM_RECEIVER.
    *WAIT_STREAM_RECEIVER.lock() = None;

    Ok(())
}

/// Receive a payload of arbitrary size asynchronously.
/// 
/// Since this requires assembly of data on the receiver-side, the `should_collect` callback is supplied with the header of each incoming chunk so that it may indicate whether it wants to collect said chunk and start a local merge.
/// 
/// Only one thread can be calling [`recv_stream`] at a time (as [`recv_stream`] calls [`wait_stream`] under-the-hood). The behavior when it is called at the same time from more than one thread is undefined.
pub async fn recv_stream<I: FnMut(&[u8]) -> bool + Send + 'static + Clone>(should_collect: I) -> Result<Vec<u8>, IpcError> {
    loop {
        match try_recv_stream(should_collect.clone()) {
            Ok(did_finish_new_merged_message_before_ran_out_of_accessible_pages) => match did_finish_new_merged_message_before_ran_out_of_accessible_pages {
                super::common::TryRecvStreamResult::NewCompleted(merged_message) => return Ok(merged_message),
                super::common::TryRecvStreamResult::PotentiallyAvailable => {
                    tokio::task::yield_now().await;  // Provide tokio with an yield point.
                    continue;
                },
                super::common::TryRecvStreamResult::OutOfAccessible => {
                    wait_stream().await?;
                }
            },
            Err(e) => return Err(e),
        }
    }
}

/// Send an arbitrarily-sized payload.
/// 
/// Note that since this requires assembly of data on the receiver-side, all payloads should have a small header section so that receivers can decide if they want to reconstruct the message or pass on it, saving memory and execution time.
/// 
/// The `header_size` specifies how many bytes from the head of the payload corresponds to the header; every fragment sent will include the header but have different sections of the body.
pub fn send_stream(payload: &[u8], header_size: usize) -> Result<(), IpcError> {
    let payload: Arc<[u8]> = Arc::from(payload);

    // IOX2_SEND_SENDER_TX
    let (send, recv) = crossbeam::channel::bounded(1);
    actor_call!(IOX2_SEND_SENDER_TX, Iox2BackingMessage::SendStream { payload: payload.clone(), header_size, respond_to: send }, recv)?;

    Ok(())
}

thread_local! {
    static TOKIO_RT: LazyCell<tokio::runtime::Runtime> = LazyCell::new(|| tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("this should never happen."));
}

/// Blocking version of [`wait_stream`].
pub fn blocking_wait_stream() -> Result<(), IpcError> {
    TOKIO_RT.with(|cell| {
        cell.block_on(wait_stream())
    })
}

/// Blocking version of [`recv_stream`].
pub fn blocking_recv_stream<I: FnMut(&[u8]) -> bool + Send + 'static + Clone>(should_collect: I) -> Result<Vec<u8>, IpcError> {
    TOKIO_RT.with(|cell| {
        cell.block_on(recv_stream(should_collect))
    })
}

