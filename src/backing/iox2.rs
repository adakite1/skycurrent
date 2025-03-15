use std::{cell::{OnceCell, RefCell}, collections::HashMap, path::{Path, PathBuf}, time::Duration};
use bitvec::prelude as bv;
use iceoryx2::{node::{Node, NodeBuilder, NodeWaitFailure}, service::ipc};
use iceoryx2_bb_container::semantic_string::SemanticString;
use iceoryx2_cal::event::TriggerId;
use rand::Rng;
use thiserror::Error;
use bitflags::bitflags;

use crate::hasher::IdentityHash;

use super::common::ProjectDirectoryPaths;

/// Maximum message size in bytes.
const PRESET_N: usize = 1024;
#[unsafe(no_mangle)]
static SC_PRESET_N: usize = PRESET_N;

/// A simple binary blob sized exactly to the IPC-expected length.
type BlobSized = Blob<PRESET_N>;

// The library does not require its users to create any structures. Instead, service is 
// thread-bound, meaning each thread will have its own set of local IPC configurations.
thread_local! {
    static PROJECT_ROOT_PATH: OnceCell<PathBuf> = OnceCell::new();
    static IOX2_CUSTOM_CONFIG: OnceCell<iceoryx2::config::Config> = OnceCell::new();
    static IOX2_NODE: OnceCell<Node<ipc::Service>> = OnceCell::new();
    static IOX2_SERVICE_EVENTS: OnceCell<iceoryx2::service::port_factory::event::PortFactory<ipc::Service>> = OnceCell::new();
    static IOX2_NOTIF: OnceCell<iceoryx2::port::notifier::Notifier<ipc::Service>> = OnceCell::new();
    static IOX2_LISTE: OnceCell<iceoryx2::port::listener::Listener<ipc::Service>> = OnceCell::new();
    static IOX2_SERVICE_PUB_SUB: OnceCell<iceoryx2::service::port_factory::publish_subscribe::PortFactory<ipc::Service, Blob<PRESET_N>, ()>> = OnceCell::new();
    static IOX2_PUB: OnceCell<iceoryx2::port::publisher::Publisher<ipc::Service, Blob<PRESET_N>, ()>> = OnceCell::new();
    static IOX2_SUB: OnceCell<iceoryx2::port::subscriber::Subscriber<ipc::Service, Blob<PRESET_N>, ()>> = OnceCell::new();
    static MERGE_ALLOCS: RefCell<HashMap<u64, (bv::BitVec, usize, Vec<u8>), IdentityHash>> = RefCell::new(HashMap::with_hasher(IdentityHash::default()));
}

/// A simple binary blob with N bytes in it.
/// 
/// The default maximum message size is 2 KB (2048 Bytes), and messages must be padded to that length before being sent and received.
#[repr(C)]
#[derive(Debug)]
struct Blob<const N: usize>(pub [u8; N]);
impl<const N: usize> Blob<N> {
    /// Resizes the blob.
    pub fn resize<const T: usize>(self) -> Blob<T> {
        let mut new_data = [0u8; T];
        let copy_size = std::cmp::min(N, T);
        new_data[..copy_size].copy_from_slice(&self.0[..copy_size]);
        Blob(new_data)
    }
}

bitflags! {
    /// Flags used during the initialization of SkyCurrent.
    /// 
    /// The following flags are currently supported:
    /// 
    /// - `CREAT_SUBSCRIBER` (0b00000001): Create a subscriber.
    /// - `CREAT_PUBLISHER` (0b00000010): Create a publisher.
    /// - `CREAT_LISTENER` (0b00000100): Create a listener.
    /// - `CREAT_NOTIFIER` (0b00001000): Create a notifier.
    pub struct InitFlags: u32 {
        /// Create a subscriber.
        const IOX2_CREAT_SUBSCRIBER = 0b00000001;

        /// Create a publisher.
        const IOX2_CREAT_PUBLISHER = 0b00000010;

        /// Create a listener.
        const IOX2_CREAT_LISTENER = 0b00000100;

        /// Create a notifier.
        const IOX2_CREAT_NOTIFIER = 0b00001000;
    }
}

/// Possible errors during initialization.
#[derive(Error, Debug, Clone)]
pub enum InitError {
    /// Encountered an initialization error in the common code.
    #[error("encountered an initialization error in the common code")]
    CommonInitError(#[from] super::common::InitError),
    /// Could not create a folder.
    #[error("failed to create iceoryx2 root directory at '{0}': {1}")]
    CreateIox2RootError(PathBuf, std::io::ErrorKind),
    /// Failed to write the IPC config file.
    #[error("failed to write the IPC config file '{0}': {1}")]
    Iox2WriteIpcConfigError(PathBuf, std::io::ErrorKind),
    /// Failed to create iceoryx2 node for IPC.
    #[error("failed to create iceoryx2 node")]
    Iox2NodeCreationFailure(#[from] iceoryx2::node::NodeCreationFailure),
    /// Failed to open or create the iceoryx2 events client.
    #[error("failed to open or create the iceoryx2 events client")]
    Iox2EventOpenOrCreateError(#[from] iceoryx2::service::builder::event::EventOpenOrCreateError),
    /// Failed to create notifier.
    #[error("failed to create notifier")]
    Iox2NotifierCreateError(#[from] iceoryx2::port::notifier::NotifierCreateError),
    /// Failed to create listener.
    #[error("failed to create listener")]
    Iox2ListenerCreateError(#[from] iceoryx2::port::listener::ListenerCreateError),
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
    let ProjectDirectoryPaths { root: path, res: _, tmp } = super::common::build_project_dir_structure(path)?;

    // IPC setup
    // Set up a root directory for iceoryx2.
    let root = tmp.join("iox2");
    std::fs::create_dir_all(&root).map_err(|e| InitError::CreateIox2RootError(root.to_path_buf(), e.kind()))?;

    // Set up the global configuration file for iceoryx2 if it does not already exist.
    let config_path = tmp.join("iox2.toml");
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
subscriber-max-buffer-size                  = 10
subscriber-max-borrowed-samples             = 10
publisher-max-loaned-samples                = 10
enable-safe-overflow                        = false
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
        std::fs::write(&config_path, config).map_err(|e| InitError::Iox2WriteIpcConfigError(config_path.to_path_buf(), e.kind()))?;
    }

    // Start ipc.
    let custom_config = iceoryx2::config::Config::from_file(
        &iceoryx2_bb_system_types::file_path::FilePath::new(config_path.to_string_lossy().to_string().as_bytes()).map_err(|e| InitError::Iox2InvalidConfigFilePathError(config_path.to_path_buf(), e))?
    )?;
    let node = NodeBuilder::new()
        .config(&custom_config)
        .create::<ipc::Service>()?;

    let service_events = node.service_builder(&"SkyCurrent/Events".try_into().unwrap()) // Unwrap is fine here because the service name is static.
        .event()
        .open_or_create()?;
    if flags.intersects(InitFlags::IOX2_CREAT_NOTIFIER) {
        let notifier = service_events.notifier_builder().create()?;
        IOX2_NOTIF.with(|cell| {
            cell.get_or_init(|| notifier);
        });
    }
    if flags.intersects(InitFlags::IOX2_CREAT_LISTENER) {
        let listener = service_events.listener_builder().create()?;
        IOX2_LISTE.with(|cell| {
            cell.get_or_init(|| listener);
        });
    }

    let service_pub_sub = node.service_builder(&"SkyCurrent/PubSub".try_into().unwrap()) // Unwrap is fine here because the service name is static.
        .publish_subscribe::<BlobSized>()
        .open_or_create()?;
    if flags.intersects(InitFlags::IOX2_CREAT_PUBLISHER) {
        let publisher = service_pub_sub.publisher_builder().create()?;
        IOX2_PUB.with(|cell| {
            cell.get_or_init(|| publisher);
        });
    }
    if flags.intersects(InitFlags::IOX2_CREAT_SUBSCRIBER) {
        let subscriber = service_pub_sub.subscriber_builder().create()?;
        IOX2_SUB.with(|cell| {
            cell.get_or_init(|| subscriber);
        });
    }

    IOX2_SERVICE_PUB_SUB.with(|cell| {
        cell.get_or_init(|| service_pub_sub);
    });
    IOX2_SERVICE_EVENTS.with(|cell| {
        cell.get_or_init(|| service_events);
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

/// Possible errors during normal operations.
#[derive(Error, Debug, Clone)]
pub enum IpcError {
    /// IPC not initialized on this thread.
    #[error("SkyCurrent is not initialized on this thread - call `init` first")]
    NotInitialized,
    /// An interrupt signal was received while waiting.
    #[error("an interrupt signal was received while waiting: {0:?}")]
    InterruptSignalReceivedError(super::common::InterruptSignals),
    /// Invalid header size provided, header size exceeds payload length.
    #[error("invalid header size, header size is {0} but payload itself is only {1}")]
    PayloadHeaderSizeExceedsPayloadLength(usize, usize),
    /// Failed to send a payload because it was too large.
    #[error("payload was too large! size was {0} but maximum is {1}")]
    Iox2PayloadTooLarge(usize, usize),
    /// Failed to send a payload because its header was too large.
    #[error("payload header was too large! size was {0} but maximum is {1}")]
    Iox2PayloadHeaderTooLarge(usize, usize),
    /// Failed to send notification.
    #[error("failed to send notification")]
    Iox2NotifierNotifyError(#[from] iceoryx2::port::notifier::NotifierNotifyError),
    /// Error while waiting for signal.
    #[error("error happened while waiting for signal")]
    Iox2ListenerWaitError(#[from] iceoryx2_cal::event::ListenerWaitError),
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
fn wait(cycle_time: Duration) -> Result<(), IpcError> {
    IOX2_NODE.with(|cell| {
        if let Some(node) = cell.get() {
            node.wait(cycle_time).map_err(|e| match e {
                NodeWaitFailure::Interrupt => IpcError::InterruptSignalReceivedError(super::common::InterruptSignals::Interrupt),
                NodeWaitFailure::TerminationRequest => IpcError::InterruptSignalReceivedError(super::common::InterruptSignals::TerminationRequest)
            })
        } else {
            Err(IpcError::NotInitialized)
        }
    })
}

/// Wait for any event, returning an error and exiting early if an interrupt signal is received.
/// 
/// Once an event arrives, grab any other events that might have come simultaneously and call the callback function for every event ID received.
fn wait_for_events<F: FnMut(usize)>(mut f: F) -> Result<(), IpcError> {
    IOX2_LISTE.with(|cell| {
        if let Some(listener) = cell.get() {
            listener.blocking_wait_all(|event_id| f(event_id.as_value())).map_err(|e| match e {
                iceoryx2_cal::event::ListenerWaitError::InterruptSignal => IpcError::InterruptSignalReceivedError(super::common::InterruptSignals::GenericInterruptSignal),
                _ => IpcError::Iox2ListenerWaitError(e)
            })
        } else {
            Err(IpcError::NotInitialized)
        }
    })
}

/// Wait for any event with a timeout, returning an error and exiting early if we reach the timeout or an interrupt signal is received.
/// 
/// Once an event arrives, grab any other events that might have come simultaneously and call the callback function for every event ID received.
fn wait_for_events_with_timeout<F: FnMut(usize)>(mut f: F, timeout: Duration) -> Result<(), IpcError> {
    IOX2_LISTE.with(|cell| {
        if let Some(listener) = cell.get() {
            listener.timed_wait_all(|event_id| f(event_id.as_value()), timeout).map_err(|e| match e {
                iceoryx2_cal::event::ListenerWaitError::InterruptSignal => IpcError::InterruptSignalReceivedError(super::common::InterruptSignals::GenericInterruptSignal),
                _ => IpcError::Iox2ListenerWaitError(e)
            })
        } else {
            Err(IpcError::NotInitialized)
        }
    })
}

/// Notify all listeners of the event id. Return the number of listeners notified on success.
fn notify(event_id: usize) -> Result<usize, IpcError> {
    IOX2_NOTIF.with(|cell| {
        if let Some(notifier) = cell.get() {
            notifier.notify_with_custom_event_id(TriggerId::new(event_id)).map_err(|e| IpcError::Iox2NotifierNotifyError(e))
        } else {
            Err(IpcError::NotInitialized)
        }
    })
}

/// Try to receive a payload of arbitrary size by fragmenting.
/// 
/// Note that since this requires assembly of data on the receiver-side, the `should_collect` callback is supplied with the header of each incoming chunk so that it may indicate whether it wants to collect said chunk and start a local merge.
/// 
/// This function never blocks, instead attempting to get a single accessible page and merging it if told to do so, then returning immediately.
pub fn try_recv_stream<I: FnMut(&[u8]) -> bool>(mut should_collect: I) -> Result<super::common::TryRecvStreamResult, IpcError> {
    loop {
        match recv_page(|blob| {
            let payload_body_len_remainder = u64::from_le_bytes(blob.0[..8].try_into().unwrap()) as usize;
            let chunk_i = u32::from_le_bytes(blob.0[8..12].try_into().unwrap()) as usize;
            let num_chunks = u32::from_le_bytes(blob.0[12..16].try_into().unwrap()) as usize;
            let identification = u64::from_le_bytes(blob.0[16..24].try_into().unwrap());
            if num_chunks == 0 {  // We are not fragmenting.
                let payload_len = payload_body_len_remainder;
                let header_size = identification as usize;
                if should_collect(&blob.0[24..header_size+24]) {
                    Some(blob.0[24..(payload_len+24)].to_vec())
                } else {
                    None
                }
            } else {  // We are fragmenting.
                // Read the rest of the metadata.
                let header_size = u64::from_le_bytes(blob.0[24..32].try_into().unwrap()) as usize;
                // Check if the user is interested in the fragment.
                if should_collect(&blob.0[32..(header_size+32)]) {
                    // The user has requested that we collect the fragment and begin merging the full data.
                    // Calculate how much space to allocate to contain any future fragments.
                    let chunk_size = PRESET_N-8-8-8-8-header_size;
                    let total_len = header_size + (num_chunks - 1) * chunk_size + if payload_body_len_remainder == 0 { chunk_size } else { payload_body_len_remainder };
                    MERGE_ALLOCS.with(|cell| {
                        let current_count;
                        {
                            let mut allocations = cell.borrow_mut();
    
                            // Allocate if we haven't done so yet, but otherwise get the existing allocation.
                            let (merged, count, allocation) = allocations.entry(identification).or_insert_with(|| {
                                // Allocate.
                                let merged = bv::bitvec![0; num_chunks];
                                let mut allocation = vec![0u8; total_len];
    
                                // Copy the header in right after so that we only write the header once.
                                allocation[..header_size].copy_from_slice(&blob.0[32..(header_size+32)]);
    
                                (merged, 0, allocation)
                            });
    
                            // Check if this chunk has already been merged in or not.
                            if merged[chunk_i] {
                                // If it has already, then we don't need it and can ignore it.
                                return None;
                            }
                            
                            // Calculate the offset of this chunk.
                            let chunk_start_offset = header_size + chunk_i * chunk_size;
                            let actual_chunk_len;
                            if num_chunks == (chunk_i+1) {
                                // This is the last chunk, so use the remainder length instead of the full chunk length to calculate the end offset if it is not zero.
                                actual_chunk_len = if payload_body_len_remainder == 0 { chunk_size } else { payload_body_len_remainder };
                            } else {
                                actual_chunk_len = chunk_size;
                            }
                            let chunk_end_offset = chunk_start_offset + actual_chunk_len;
    
                            // Merge.
                            allocation[chunk_start_offset..chunk_end_offset].copy_from_slice(&blob.0[(header_size+32)..(actual_chunk_len+header_size+32)]);
    
                            // Update statistics.
                            merged.set(chunk_i, true);
                            *count += 1;
                            
                            current_count = *count;
                        }
    
                        // Check if this payload has been fully merged or not.
                        if num_chunks == current_count {
                            let mut allocations = cell.borrow_mut();
    
                            if let Some((_, _, allocation)) = allocations.remove(&identification) {
                                return Some(allocation);
                            }
                        }
    
                        None
                    })
                } else {
                    None
                }
            }
        }) {
            Ok(did_receive_new_page) => {
                match did_receive_new_page {
                    Some(callback_return_value) => {
                        match callback_return_value {
                            Some(merged_message) => return Ok(super::common::TryRecvStreamResult::NewCompleted(merged_message)),
                            None => return Ok(super::common::TryRecvStreamResult::PotentiallyAvailable),
                        }
                    },
                    None => {
                        return Ok(super::common::TryRecvStreamResult::OutOfAccessible);
                    },
                }
            },
            Err(e) => return Err(e),
        }
    }
}

/// Wait until new item is available on stream.
/// 
/// This will return early if the shutdown signal is received, but only if that signal is sent from a different sending thread.
pub fn wait_stream() -> Result<(), IpcError> {
    wait_for_events(|_| {})
}

/// Sends a shutdown signal.
/// 
/// Note that this won't actually shut anything down! Do that by calling [`close`].
/// 
/// This simply causes certain blocking functions to return early. Most notably, `wait_stream` should return early when this signal is sent.
pub fn shutdown_signal() -> Result<(), IpcError> {
    notify(0).map(|_| ())
}

/// Close SkyCurrent.
/// 
/// On some backings, this function is a no-op, but it should always be called before the thread using the library exits to give SkyCurrent a chance to clean up.
pub fn close() -> Result<(), IpcError> { Ok(()) }

/// Receive a payload of arbitrary size by fragmenting.
/// 
/// Since this requires assembly of data on the receiver-side, the `should_collect` callback is supplied with the header of each incoming chunk so that it may indicate whether it wants to collect said chunk and start a local merge.
/// 
/// This function blocks until it successfully finishes merging a message, returning that merged message as the result.
pub fn recv_stream<I: FnMut(&[u8]) -> bool>(mut should_collect: I) -> Result<Vec<u8>, IpcError> {
    loop {
        match try_recv_stream(&mut should_collect) {
            Ok(did_finish_new_merged_message_before_ran_out_of_accessible_pages) => match did_finish_new_merged_message_before_ran_out_of_accessible_pages {
                super::common::TryRecvStreamResult::NewCompleted(merged_message) => return Ok(merged_message),
                super::common::TryRecvStreamResult::PotentiallyAvailable => {
                    continue;
                },
                super::common::TryRecvStreamResult::OutOfAccessible => {
                    wait_stream()?;
                }
            },
            Err(e) => return Err(e),
        }
    }
}

/// Send an arbitrarily-sized payload by fragmenting.
/// 
/// Note that since this requires assembly of data on the receiver-side, all payloads should have a small header section so that receivers can decide if they want to reconstruct the message or pass on it, saving memory and execution time.
/// 
/// The `header_size` specifies how many bytes from the head of the payload corresponds to the header; every fragment sent will include the header but have different sections of the body.
pub fn send_stream(payload: &[u8], header_size: usize) -> Result<(), IpcError> {
    if header_size > payload.len() {
        return Err(IpcError::PayloadHeaderSizeExceedsPayloadLength(header_size, payload.len()));
    }
    // Check if the entire payload actually fits in one page. If so, don't fragment.
    if payload.len() <= (PRESET_N-8-8-8) {
        let mut data = [0; PRESET_N];
        data[..8].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        //page[8..16] is already zero.
        data[16..24].copy_from_slice(&header_size.to_le_bytes());
        data[24..(payload.len()+24)].copy_from_slice(payload);
        let result = send_page(Blob::<PRESET_N>(data));
        notify(0)?;
        return result;
    }
    // If the combined length of everything except the fragment data body is greater than or equal to the preset page size, then there's no way we can ever send the body data. Return an `IpcError::PayloadHeaderTooLarge` error.
    if (8+8+8+8+header_size) >= PRESET_N {
        // After subtracting the automatic header part, the header can fill the remaining space, but has to at least give us 1 byte to work with for fragmentation, hence the minus 1.
        return Err(IpcError::Iox2PayloadHeaderTooLarge(header_size, PRESET_N-8-8-8-8-1));
    }
    // Otherwise, we'll need to fragment.
    let chunk_size = PRESET_N-8-8-8-8-header_size;  // Thanks to the previous check, this will at minimum be 1.
    let payload_header = &payload[..header_size];
    let payload_body_chunks = payload[header_size..].chunks(chunk_size);
    let num_chunks = payload_body_chunks.len();
    let mut rng = rand::rng();
    let mut identification = rng.random::<u64>();
    while identification == 0 {
        identification = rng.random::<u64>();
    }
    // Here there is the possibility that payload_body_chunks is empty, right? However, that situation only arises when the header does not fit into the page (since the body is empty only the header needs to fit) and if it doesn't fit there it won't fit here and it will have been rejected in the first few lines.
    // Calculate some more constants.
    let payload_body_len_remainder = (payload.len() - header_size) % chunk_size;
    // Loop through each chunk and send it.
    for (i, chunk) in payload_body_chunks.enumerate() {
        let mut page = [0; PRESET_N];
        page[..8].copy_from_slice(&payload_body_len_remainder.to_le_bytes());  // Instead of setting this field to the payload size like `send_page_sized`, we set it to the size of the last body chunk. This, combined with the next two fields and the PRESET_N allows us to calculate the total payload size quite easily.
        page[8..12].copy_from_slice(&(i as u32).to_le_bytes());
        page[12..16].copy_from_slice(&(num_chunks as u32).to_le_bytes());
        page[16..24].copy_from_slice(&identification.to_le_bytes());
        page[24..32].copy_from_slice(&(header_size as u64).to_le_bytes());
        page[32..(header_size+32)].copy_from_slice(payload_header);
        let actual_chunk_len = chunk.len();  // Final chunk will not be full.
        page[(header_size+32)..(actual_chunk_len+header_size+32)].copy_from_slice(chunk);
        let mut backoff = 100;
        while let Err(_) = send_page(Blob::<PRESET_N>(page)) {
            // Keep trying with exponential backoff.
            if let Err(wait_error) = wait(Duration::from_millis(backoff)) {
                return Err(wait_error);
            }
            backoff = 8000.min(backoff*2);
        }
        // Notify.
        notify(0)?;
    }
    Ok(())
}

/// Send a payload of arbitrary size. If the size is greater than PRESET_N-8 (8 bytes for header indicating payload size), this will return an `IpcError::PayloadTooLarge` error.
fn send_page_sized(payload: &[u8]) -> Result<(), IpcError> {
    if payload.len() > (PRESET_N - 8) {
        return Err(IpcError::Iox2PayloadTooLarge(payload.len(), PRESET_N-8));
    }
    let mut data = [0; PRESET_N];
    data[..8].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    data[8..(payload.len()+8)].copy_from_slice(payload);
    send_page(Blob::<PRESET_N>(data))
}

/// Try to receive a payload of arbitrary size. If there is one, call the provided callback function with that payload. Otherwise do nothing more and return `Ok(None)`.
/// 
/// If a payload is available and the callback is triggered, the return value of the callback is passed back through the option `Ok(Some(R))`.
fn recv_page_sized<F: FnOnce(&[u8]) -> R, R>(f: F) -> Result<Option<R>, IpcError> {
    recv_page(|blob| {
        let payload_len = u64::from_le_bytes(blob.0[..8].try_into().unwrap()) as usize;
        f(&blob.0[8..(payload_len+8)])
    })
}

/// Send a payload of size PRESET_N.
fn send_page(payload: BlobSized) -> Result<(), IpcError> {
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
fn recv_page<F: FnOnce(&BlobSized) -> R, R>(f: F) -> Result<Option<R>, IpcError> {
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

