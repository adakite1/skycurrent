use std::{thread, sync::LazyLock, time::Duration};
use parking_lot::RwLock;
use lmq::{LinkMessageQueue, MessageConsumer};

use crate::backing::combined::{init as init_backing, send_stream as send_stream_backing, InitFlags, IpcError, recv_stream};

static GLOBAL_LINK_MESSAGE_QUEUE: LazyLock<RwLock<LinkMessageQueue>> = LazyLock::new(|| RwLock::new(LinkMessageQueue::new()));

pub use crate::backing::combined::{set_global_project_dir, set_global_should_collect, close};

macro_rules! retry_after_timeout {
    ($error:ident) => {{
        eprintln!("{:?}", $error);
        eprintln!("Retrying in 3 seconds...");
        tokio::time::sleep(Duration::from_millis(3000)).await;
    }};
}

macro_rules! retry_after_small_timeout {
    ($error:ident) => {{
        eprintln!("{:?}", $error);
        eprintln!("Retrying in 100ms...");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }};
}

macro_rules! blocking_retry_after_small_timeout {
    ($error:ident) => {{
        eprintln!("{:?}", $error);
        eprintln!("Retrying in 100ms...");
        thread::sleep(Duration::from_millis(100));
    }};
}

pub async fn init() {
    // Initialize.
    while let Err(e) = init_backing(InitFlags::INIT_IOX2_RECV | InitFlags::INIT_IOX2_SEND | InitFlags::INIT_WS_GATEWAY).await {
        match e {
            crate::backing::combined::InitError::MissingGlobalProjectDirectory(e) => panic!("{:?}", e),
            crate::backing::combined::InitError::CommonInitError(e) => match e {
                crate::backing::common::InitError::NotAFolder(_) => panic!("{:?}", e),
                crate::backing::common::InitError::FailedToCanonicalizeProjectPath(_, _) => panic!("{:?}", e),
                crate::backing::common::InitError::CreateDirError(_, _) => panic!("{:?}", e),
                crate::backing::common::InitError::TcpListenerBindError(_) => retry_after_timeout!(e),
            },
            crate::backing::combined::InitError::BackingCrashed(_) => retry_after_timeout!(e),
            #[cfg(feature = "backing-iox2")]
            crate::backing::combined::InitError::Iox2InitError(e) => match e {
                crate::backing::iox2::InitError::CommonInitError(e) => match e {
                    crate::backing::common::InitError::NotAFolder(_) => panic!("{:?}", e),
                    crate::backing::common::InitError::FailedToCanonicalizeProjectPath(_, _) => panic!("{:?}", e),
                    crate::backing::common::InitError::CreateDirError(_, _) => panic!("{:?}", e),
                    crate::backing::common::InitError::TcpListenerBindError(_) => retry_after_timeout!(e),
                },
                crate::backing::iox2::InitError::CreateIox2RootError(_, _) => panic!("{:?}", e),
                crate::backing::iox2::InitError::Iox2WriteIpcConfigError(_, _) => panic!("{:?}", e),
                crate::backing::iox2::InitError::Iox2NodeCreationFailure(_) => panic!("{:?}", e),
                crate::backing::iox2::InitError::Iox2EventOpenOrCreateError(_) => retry_after_timeout!(e),
                crate::backing::iox2::InitError::Iox2NotifierCreateError(_) => retry_after_timeout!(e),
                crate::backing::iox2::InitError::Iox2ListenerCreateError(_) => retry_after_timeout!(e),
                crate::backing::iox2::InitError::Iox2PublishSubscribeOpenOrCreateError(_) => retry_after_timeout!(e),
                crate::backing::iox2::InitError::Iox2PublisherCreateError(_) => retry_after_timeout!(e),
                crate::backing::iox2::InitError::Iox2SubscriberCreateError(_) => retry_after_timeout!(e),
                crate::backing::iox2::InitError::Iox2InvalidConfigFilePathError(_, _) => panic!("{:?}", e),
                crate::backing::iox2::InitError::Iox2ConfigCreationError(_) => panic!("{:?}", e),
            },
            #[cfg(feature = "backing-ws-gateway")]
            crate::backing::combined::InitError::WsGatewayInitError(e) => match e {
                crate::backing::ws_gateway::InitError::CommonInitError(e) => match e {
                    crate::backing::common::InitError::NotAFolder(_) => panic!("{:?}", e),
                    crate::backing::common::InitError::FailedToCanonicalizeProjectPath(_, _) => panic!("{:?}", e),
                    crate::backing::common::InitError::CreateDirError(_, _) => panic!("{:?}", e),
                    crate::backing::common::InitError::TcpListenerBindError(_) => retry_after_timeout!(e),
                },
            },
        }
    };

    // Receiver task to fill the queue as messages arrive.
    tokio::spawn(async move {
        loop {
            match recv_stream().await {
                Ok(payload) => GLOBAL_LINK_MESSAGE_QUEUE.write().push(payload),
                Err(e) => {
                    match e {
                        #[cfg(feature = "backing-iox2")]
                        IpcError::Iox2IpcError(e) => match e {
                            crate::backing::iox2::IpcError::Iox2ListenerWaitError(_) => retry_after_small_timeout!(e),
                            crate::backing::iox2::IpcError::Iox2SubscriberReceiveError(_) => retry_after_small_timeout!(e),
                            _ => panic!("{:?}", e),
                        },
                        _ => panic!("{:?}", e),
                    }
                },
            }
        }
    });
}

/// Send an arbitrarily-sized payload, expecting responses to that payload afterwards.
/// 
/// If a message is sent with the expectation of a reply and the `autodrop` feature is enabled, [`dlg_stream`] must be used over [`send_stream`] for that purpose.
/// 
/// The returned [`MessageConsumer`] can be used to receive any subsequent messages that could potentially be replies to this message.
pub fn dlg_stream(payload: &[u8], header_size: usize) -> MessageConsumer {
    let consumer = iter_stream();
    send_stream(payload, header_size);
    consumer
}

/// Send an arbitrarily-sized payload.
/// 
/// Since this might require reassembly of data on the receiver-side on certain backings, all payloads should have a small header section so that receivers can decide if they want to reconstruct the message or pass on it, saving memory and execution time.
/// 
/// The `header_size` specifies how many bytes from the head of the payload corresponds to the header; every fragment sent will include the header but have different sections of the body.
pub fn send_stream(payload: &[u8], header_size: usize) {
    loop {
        match send_stream_backing(payload, header_size) {
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
}

/// Get an iterator over the message stream.
/// 
/// This returns a [`MessageConsumer`] that can be used to iterate through and process unclaimed incoming messages.
pub fn iter_stream() -> MessageConsumer {
    GLOBAL_LINK_MESSAGE_QUEUE.read().create_consumer()
}

