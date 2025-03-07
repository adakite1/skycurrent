use std::{ffi::{c_char, c_int, CStr}, path::{Path, PathBuf}, sync::OnceLock, time::Duration};
use iceoryx2::{node::{Node, NodeBuilder, NodeWaitFailure}, service::ipc};
use iceoryx2_bb_container::semantic_string::SemanticString;
use thiserror::Error;
use bitflags::bitflags;

/// Maximum message size in bytes.
const PRESET_N: usize = 2048;
#[unsafe(no_mangle)]
pub static SC_PRESET_N: usize = PRESET_N;

/// A simple binary blob sized exactly to the IPC-expected length.
pub type BlobSized = Blob<PRESET_N>;

// The library does not require its users to create any structures. Instead, service is 
// thread-bound, meaning each thread will have its own set of local IPC configurations.
thread_local! {
    static PROJECT_ROOT_PATH: OnceLock<PathBuf> = OnceLock::new();
    static IOX2_CUSTOM_CONFIG: OnceLock<iceoryx2::config::Config> = OnceLock::new();
    static IOX2_NODE: OnceLock<Node<ipc::Service>> = OnceLock::new();
    static IOX2_SERVICE: OnceLock<iceoryx2::service::port_factory::publish_subscribe::PortFactory<ipc::Service, Blob<PRESET_N>, ()>> = OnceLock::new();
    static IOX2_PUB: OnceLock<iceoryx2::port::publisher::Publisher<ipc::Service, Blob<PRESET_N>, ()>> = OnceLock::new();
    static IOX2_SUB: OnceLock<iceoryx2::port::subscriber::Subscriber<ipc::Service, Blob<PRESET_N>, ()>> = OnceLock::new();
}

/// A simple binary blob with N bytes in it.
/// 
/// The default maximum message size is 2 KB (2048 Bytes), and messages must be padded to that length before being sent and received.
#[repr(C)]
#[derive(Debug)]
pub struct Blob<const N: usize>(pub [u8; N]);
impl<const N: usize> Blob<N> {
    /// Resizes the blob.
    pub fn resize<const T: usize>(self) -> Blob<T> {
        let mut new_data = [0u8; T];
        let copy_size = std::cmp::min(N, T);
        new_data[..copy_size].copy_from_slice(&self.0[..copy_size]);
        Blob(new_data)
    }
}

/// Flag indicating that the subscriber should be created during initialization.
/// Pass this to `sc_init`.
#[unsafe(no_mangle)]
pub static SC_INIT_FLAGS_CREAT_SUBSCRIBER: u32 = 0b00000001;
/// Flag indicating that the publisher should be created during initialization.
/// Pass this to `sc_init`.
#[unsafe(no_mangle)]
pub static SC_INIT_FLAGS_CREAT_PUBLISHER: u32 = 0b00000010;

bitflags! {
    /// Flags used during the initialization of SkyCurrent.
    /// 
    /// The following flags are currently supported:
    /// 
    /// - `CREAT_SUBSCRIBER` (0b00000001): Create a subscriber.
    /// - `CREAT_PUBLISHER` (0b00000010): Create a publisher.
    pub struct InitFlags: u32 {
        /// Create a subscriber.
        const CREAT_SUBSCRIBER = 0b00000001;

        /// Create a publisher.
        const CREAT_PUBLISHER = 0b00000010;
    }
}

/// Possible errors during initialization.
#[derive(Error, Debug)]
pub enum InitError {
    /// Expected a project folder, but instead was provided with a path that does not exist or is not a folder.
    #[error("the provided path '{0}' for initializing SkyCurrent does not exist or is not a folder!")]
    NotAFolder(PathBuf),
    /// Could not canonicalize the provided project path.
    #[error("failed to canonicalize the provided project root path '{0}': {1}")]
    FailedToCanonicalizeProjectPath(PathBuf, std::io::Error),
    /// Could not create a folder.
    #[error("failed to create directory at '{0}': {1}")]
    CreateDirError(PathBuf, std::io::Error),
    /// Failed to write the IPC config file.
    #[error("failed to write the IPC config file '{0}': {1}")]
    Iox2WriteIpcConfigError(PathBuf, std::io::Error),
    /// Failed to create iceoryx2 node for IPC.
    #[error("failed to create iceoryx2 node")]
    Iox2NodeCreationFailure(#[from] iceoryx2::node::NodeCreationFailure),
    /// Failed to open or create the iceoryx2 pub/sub client.
    #[error("failed to open or create the iceoryx2 pub/sub client")]
    Iox2PublishSubscribeOpenOrCreateError(#[from] iceoryx2::service::builder::publish_subscribe::PublishSubscribeOpenOrCreateError),
    /// Failed to create publisher.
    #[error("failed to create publisher")]
    Iox2PublisherCreateError(#[from] iceoryx2::port::publisher::PublisherCreateError),
    /// Failed to create subscriber.
    #[error("failed to create subscriber")]
    Iox2SubscriberCreateError(#[from] iceoryx2::port::subscriber::SubscriberCreateError),
    /// Invalid config file path error.
    #[error("invalid config file path error, path '{0}' might be too long or contain unsupported characters: {1}")]
    Iox2InvalidConfigFilePathError(PathBuf, iceoryx2_bb_container::semantic_string::SemanticStringError),
    /// Invalid iceoryx2 configuration file. Possibly manually edited since last time it was automatically generated.
    #[error("invalid iceoryx2 configuration file, was the file manually edited since it was last automatically generated?")]
    Iox2ConfigCreationError(#[from] iceoryx2::config::ConfigCreationError),
}
/// SkyCurrent works on a project basis, and a project is contained within a folder.
/// 
/// Initialize takes a `path` pointing to a project folder and sets up only the IPC part if it does not exist already. Otherwise, it is a no-op.
pub fn init(path: &Path, flags: InitFlags) -> Result<(), InitError> {
    // We are expecting a project folder. If the project folder does not exist or is not a directory, we cannot continue.
    if !path.is_dir() {
        return Err(InitError::NotAFolder(path.to_path_buf()));
    }
    // Canonicalize the project folder path.
    let path = dunce::canonicalize(path).map_err(|e| InitError::FailedToCanonicalizeProjectPath(path.to_path_buf(), e))?;

    // Set up common temporary directories.
    //  The res/ directory is for things that should not be checked into git but still makes sense to be kept around for a long time.
    //  The tmp/ directory is for things that should not be checked into git as they are temporary files.
    let res = path.join("res");
    let tmp = path.join("tmp");
    std::fs::create_dir_all(&res).map_err(|e| InitError::CreateDirError(res.to_path_buf(), e))?;
    std::fs::create_dir_all(&tmp).map_err(|e| InitError::CreateDirError(tmp.to_path_buf(), e))?;

    // IPC setup
    // Set up a root directory for iceoryx2.
    let root = tmp.join("iceoryx2");
    std::fs::create_dir_all(&root).map_err(|e| InitError::CreateDirError(root.to_path_buf(), e))?;

    // Set up the global configuration file for iceoryx2 if it does not already exist.
    let config_path = tmp.join("iceoryx2.toml");
    if !config_path.exists() {
        let config = format!("
[global]
root-path-unix                              = '{unix}'
root-path-windows                           = '{windows}'
prefix                                      = 'sc_'

[global.node]
directory                                   = 'nodes'
monitor-suffix                              = '.node_monitor'
static-config-suffix                        = '.details'
service-tag-suffix                          = '.service_tag'
cleanup-dead-nodes-on-creation              = true
cleanup-dead-nodes-on-destruction           = true

[global.service]
directory                                   = 'services'
publisher-data-segment-suffix               = '.publisher_data'
static-config-storage-suffix                = '.service'
dynamic-config-storage-suffix               = '.dynamic'
event-connection-suffix                     = '.event'
connection-suffix                           = '.connection'
creation-timeout.secs                       = 0
creation-timeout.nanos                      = 500000000

[defaults.request-response]
enable-safe-overflow-for-requests           = true
enable-safe-overflow-for-responses          = true
max-active-responses                        = 4
max-active-requests                         = 2
max-borrowed-responses                      = 4
max-borrowed-requests                       = 2
max-response-buffer-size                    = 2
max-request-buffer-size                     = 4
max-servers                                 = 2
max-clients                                 = 8
max-nodes                                   = 20

[defaults.publish-subscribe]
max-subscribers                             = 16
max-publishers                              = 16
max-nodes                                   = 20
publisher-history-size                      = 0
subscriber-max-buffer-size                  = 2
subscriber-max-borrowed-samples             = 2
publisher-max-loaned-samples                = 2
enable-safe-overflow                        = true
unable-to-deliver-strategy                  = 'Block' # or 'DiscardSample'
subscriber-expired-connection-buffer        = 128

[defaults.event]
max-listeners                               = 16
max-notifiers                               = 16
max-nodes                                   = 36
event-id-max-value                          = 4294967295
# deadline.secs                               = 1 # uncomment to enable deadline
# deadline.nanos                              = 0 # uncomment to enable deadline
# notifier-created-event                      = 1 # uncomment to enable setting
# notifier-dropped-event                      = 2 # uncomment to enable setting
# notifier-dead-event                         = 3 # uncomment to enable setting", 
            unix = if cfg!(unix) { root.to_string_lossy().to_string() } else { String::new() },
            windows = if cfg!(windows) { root.to_string_lossy().to_string() } else { String::new() }
            );
        std::fs::write(&config_path, config).map_err(|e| InitError::Iox2WriteIpcConfigError(config_path.to_path_buf(), e))?;
    }

    // Start ipc.
    let custom_config = iceoryx2::config::Config::from_file(
        &iceoryx2_bb_system_types::file_path::FilePath::new(config_path.to_string_lossy().to_string().as_bytes()).map_err(|e| InitError::Iox2InvalidConfigFilePathError(config_path.to_path_buf(), e))?
    )?;
    let node = NodeBuilder::new()
        .config(&custom_config)
        .create::<ipc::Service>()?;
    let service = node.service_builder(&"SkyCurrent".try_into().unwrap()) // Unwrap is fine here because the service name is static.
        .publish_subscribe::<BlobSized>()
        .open_or_create()?;

    if flags.intersects(InitFlags::CREAT_PUBLISHER) {
        let publisher = service.publisher_builder().create()?;
        
        IOX2_PUB.with(|cell| {
            cell.get_or_init(|| publisher);
        });
    }
    if flags.intersects(InitFlags::CREAT_SUBSCRIBER) {
        let subscriber = service.subscriber_builder().create()?;

        IOX2_SUB.with(|cell| {
            cell.get_or_init(|| subscriber);
        });
    }

    IOX2_SERVICE.with(|cell| {
        cell.get_or_init(|| service);
    });
    IOX2_NODE.with(|cell| {
        cell.get_or_init(|| node);
    });
    IOX2_CUSTOM_CONFIG.with(|cell| {
        cell.get_or_init(|| custom_config);
    });
    PROJECT_ROOT_PATH.with(|cell| {
        cell.get_or_init(|| path);
    });

    Ok(())
}

/// Interruption signals that might possibly be received.
#[derive(Debug)]
pub enum InterruptSignals {
    /// A `SIGINT` signal was received.
    Interrupt,
    /// A `SIGTERM` signal was received.
    TerminationRequest,
}
/// Possible errors during normal operations
#[derive(Error, Debug)]
pub enum IpcError {
    /// IPC not initialized on this thread.
    #[error("SkyCurrent is not initialized on this thread - call `init` first")]
    NotInitialized,
    /// An interrupt signal was received while waiting.
    #[error("an interrupt signal was received while waiting: {0:?}")]
    InterruptSignalReceivedError(InterruptSignals),
    /// Failed to send a payload because it was too large.
    #[error("payload was too large! size was {0} but maximum is {1}")]
    PayloadTooLarge(usize, usize),
    /// Failed to get a sample loan from the iceoryx2 publisher.
    #[error("failed to get a sample loan from the publisher")]
    Iox2PublisherLoanError(#[from] iceoryx2::port::publisher::PublisherLoanError),
    /// Failed to send the sample.
    #[error("failed to send the sample")]
    Iox2PublisherSendError(#[from] iceoryx2::port::publisher::PublisherSendError),
    /// Failed to receive a sample.
    #[error("failed to receive a sample")]
    Iox2SubscriberReceiveError(#[from] iceoryx2::port::subscriber::SubscriberReceiveError),
}

/// Wait for the provided duration, returning an error and exiting early when `SIGINT` or `SIGTERM` is received.
pub fn wait(cycle_time: Duration) -> Result<(), IpcError> {
    IOX2_NODE.with(|cell| {
        if let Some(node) = cell.get() {
            node.wait(cycle_time).map_err(|e| match e {
                NodeWaitFailure::Interrupt => IpcError::InterruptSignalReceivedError(InterruptSignals::Interrupt),
                NodeWaitFailure::TerminationRequest => IpcError::InterruptSignalReceivedError(InterruptSignals::TerminationRequest)
            })
        } else {
            Err(IpcError::NotInitialized)
        }
    })
}

/// Send a payload of arbitrary size. If the size is greater than PRESET_N-8 (8 bytes for header indicating payload size), this will return an `IpcError::PayloadTooLarge` error.
pub fn send(payload: &[u8]) -> Result<(), IpcError> {
    if payload.len() > (PRESET_N - 8) {
        return Err(IpcError::PayloadTooLarge(payload.len(), PRESET_N-8));
    }
    let mut data = [0; PRESET_N];
    data[..8].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    data[8..(payload.len()+8)].copy_from_slice(payload);
    send_page(Blob::<PRESET_N>(data))
}

/// Try to receive a payload of arbitrary size. If there is one, call the provided callback function with that payload. Otherwise do nothing more and return `Ok(None)`.
/// 
/// If a payload is available and the callback is triggered, the return value of the callback is passed back through the option `Ok(Some(R))`.
pub fn recv<F: FnOnce(&[u8]) -> R, R>(f: F) -> Result<Option<R>, IpcError> {
    recv_page(|blob| {
        let payload_len = u64::from_le_bytes(blob.0[..8].try_into().unwrap()) as usize;
        f(&blob.0[8..(payload_len+8)])
    })
}

/// Send a payload of size PRESET_N.
pub fn send_page(payload: BlobSized) -> Result<(), IpcError> {
    IOX2_PUB.with(|cell| {
        if let Some(publisher) = cell.get() {
            let sample = publisher.loan_uninit()?;
            let sample = sample.write_payload(payload);
            sample.send()?;
            Ok(())
        } else {
            Err(IpcError::NotInitialized)
        }
    })
}

/// Try to receive a payload of size PRESET_N. If there is one, call the provided callback function with that payload. Otherwise do nothing more and return `Ok(None)`.
/// 
/// If a payload is available and the callback is triggered, the return value of the callback is passed back through the option `Ok(Some(R))`.
pub fn recv_page<F: FnOnce(&BlobSized) -> R, R>(f: F) -> Result<Option<R>, IpcError> {
    IOX2_SUB.with(|cell| {
        if let Some(subscriber) = cell.get() {
            if let Some(sample) = subscriber.receive()? {
                Ok(Some(f(sample.payload())))
            } else {
                Ok(None)
            }
        } else {
            Err(IpcError::NotInitialized)
        }
    })
}

/// Expose `init` for C consumers.
/// 
/// Initializes SkyCurrent with the given project folder path and the given flags. See `InitFlags` for details on the available flags.
/// 
/// Note that you should initialize once per thread.
/// 
/// Returns:
///  0 on success
/// -1 if path string is invalid UTF-8
/// -2 if path does not exist or is not a folder
/// -3 if path could not be canonicalized
/// -4 if could not create a directory during initialization
/// -16 if could not write IPC config file into tmp/ folder
/// -17 if could not create IPC node
/// -18 if could not create pub/sub client
/// -19 if could not create publisher
/// -20 if could not create subscriber
/// -21 if config file path is invalid
/// -22 if config file is invalid (possibly because it was manually edited since last time it was automatically generated)
#[unsafe(no_mangle)]
pub extern "C" fn sc_init(path: *const c_char, flags: u32) -> c_int {
    let c_str = unsafe { CStr::from_ptr(path) };
    if let Ok(path_string) = c_str.to_str() {
        // Convert flags into `InitFlags`
        let init_flags = InitFlags::from_bits_retain(flags);
        match init(Path::new(path_string), init_flags) {
            Ok(_) => 0,
            Err(e) => match e {
                InitError::NotAFolder(_) => -2,
                InitError::FailedToCanonicalizeProjectPath(_, _) => -3,
                InitError::CreateDirError(_, _) => -4,
                InitError::Iox2WriteIpcConfigError(_, _) => -16,
                InitError::Iox2NodeCreationFailure(_) => -17,
                InitError::Iox2PublishSubscribeOpenOrCreateError(_) => -18,
                InitError::Iox2PublisherCreateError(_) => -19,
                InitError::Iox2SubscriberCreateError(_) => -20,
                InitError::Iox2InvalidConfigFilePathError(_, _) => -21,
                InitError::Iox2ConfigCreationError(_) => -22,
            },
        }
    } else {
        -1
    }
}

fn match_ipc_error(e: &IpcError) -> c_int {
    match e {
        IpcError::NotInitialized => -1,
        IpcError::InterruptSignalReceivedError(interrupt_signals) => match interrupt_signals {
            InterruptSignals::Interrupt => 2,
            InterruptSignals::TerminationRequest => 15,
        },
        IpcError::PayloadTooLarge(_, _) => -2,
        IpcError::Iox2PublisherLoanError(_) => -16,
        IpcError::Iox2PublisherSendError(_) => -17,
        IpcError::Iox2SubscriberReceiveError(_) => -18,
    }
}

/// Expose `wait` for C consumers.
/// 
/// Waits for the specified duration in milliseconds, or until interrupted by a signal.
/// 
/// Returns:
///  0 on success (waited full duration)
///  2 if interrupted by SIGINT
/// 15 if interrupted by SIGTERM
/// -1 if not initialized
#[unsafe(no_mangle)]
pub extern "C" fn sc_wait(ms: u64) -> c_int {
    let dur = std::time::Duration::from_millis(ms);
    match wait(dur) {
        Ok(_) => 0,
        Err(e) => match_ipc_error(&e),
    }
}

/// Expose `send` for C consumers.
/// 
/// Sends a payload of arbitrary size. If the size is greater than PRESET_N-8 (8 bytes for header indicating payload size),
/// this will return error code -2 (PayloadTooLarge).
/// 
/// Returns:
///  0 on success
/// -1 if not initialized
/// -2 if payload too large
/// -16 if publisher loan failed
/// -17 if publisher send failed
/// -32 if data pointer is null
#[unsafe(no_mangle)]
pub extern "C" fn sc_send(payload: *const u8, len: u64) -> c_int {
    if payload.is_null() { return -32; }
    if len > (PRESET_N - 8) as u64 {
        return match_ipc_error(&IpcError::PayloadTooLarge(len as usize, PRESET_N-8));
    }
    let payload = unsafe { std::slice::from_raw_parts(payload, len as usize) };
    let mut data = [0; PRESET_N];
    data[..8].copy_from_slice(&(len as u64).to_le_bytes());
    data[8..(len+8) as usize].copy_from_slice(payload);
    match send_page(Blob::<PRESET_N>(data)) {
        Ok(_) => 0,
        Err(e) => match_ipc_error(&e)
    }
}

/// Expose `recv` for C consumers.
/// 
/// Calls the provided callback with the payload data pointer and length if there is a message available.
/// 
/// The 8-byte length prefix is handled internally - callback receives only the actual payload. This is not the case if you send with `send` and receive with `recv_page` however.
/// 
/// Note that since the provided pointer is managed on the Rust side, once the callback returns to Rust the pointer is not guaranteed to stay valid.
/// 
/// Returns:
///  0 on success (received message and called callback)
///  1 if no new messages available
/// -1 if not initialized
/// -18 if receive failed
/// -32 if callback function pointer is null
type RecvCallback = extern "C" fn(*const u8, u64);
#[unsafe(no_mangle)]
pub extern "C" fn sc_recv(f: Option<RecvCallback>) -> c_int {
    if f.is_none() { return -32; }
    match recv(|payload| {
        let ptr = payload.as_ptr();
        let len = payload.len() as u64;
        if let Some(callback) = f {
            callback(ptr, len);
        }
    }) {
        Ok(Some(_)) => 0,
        Ok(None) => 1,  // No new payloads available, didn't run callback.
        Err(e) => match_ipc_error(&e),
    }
}

/// Expose `send_page` for C consumers.
/// 
/// Sends a raw payload that must be EXACTLY PRESET_N bytes long.
/// 
/// Returns:
///  0 on success
/// -1 if not initialized
/// -16 if publisher loan failed
/// -17 if publisher send failed
/// -32 if data pointer is null
#[unsafe(no_mangle)]
pub extern "C" fn sc_send_page(payload: *const u8) -> c_int {
    if payload.is_null() { return -32; }
    let mut blob = [0u8; PRESET_N];
    unsafe {
        std::ptr::copy_nonoverlapping(payload, blob.as_mut_ptr(), PRESET_N);
    }
    match send_page(Blob::<PRESET_N>(blob)) {
        Ok(_) => 0,
        Err(e) => match_ipc_error(&e),
    }
}

/// Expose `recv_page` for C consumers.
/// 
/// Calls the provided callback with the payload data pointer if there is a message available.
/// 
/// The payload will be exactly PRESET_N bytes long, which is why a length is not provided for the payload.
/// 
/// Note that since the provided pointer is managed on the Rust side, once the callback returns to Rust the pointer is not guaranteed to stay valid.
/// 
/// Returns:
///  0 on success (received message and called callback)
///  1 if no new messages available
/// -1 if not initialized
/// -18 if receive failed
/// -32 if callback function pointer is null
type RecvPageCallback = extern "C" fn(*const u8);
#[unsafe(no_mangle)]
pub extern "C" fn sc_recv_page(f: Option<RecvPageCallback>) -> c_int {
    if f.is_none() { return -32; }
    match recv_page(|blob| {
        let ptr = blob.0.as_ptr();
        if let Some(callback) = f {
            callback(ptr);
        }
    }) {
        Ok(Some(_)) => 0,
        Ok(None) => 1,  // No new payloads available, didn't run callback.
        Err(e) => match_ipc_error(&e),
    }
}

