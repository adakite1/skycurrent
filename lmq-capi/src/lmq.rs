use std::{ffi::{c_int, c_void}, panic};

use lmq_rs::{LinkMessageQueue, MessageConsumer, MessageRef, NextMessage};

#[cfg(feature = "tokio")]
use std::{collections::HashMap, sync::LazyLock};
#[cfg(feature = "tokio")]
use parking_lot::Mutex;
#[cfg(feature = "tokio")]
use tokio::task::JoinHandle;

#[allow(non_camel_case_types)]
pub struct lmq_t(LinkMessageQueue);
#[allow(non_camel_case_types)]
pub struct lmq_consumer_t(MessageConsumer);
#[allow(non_camel_case_types)]
pub struct lmq_message_t(NextMessage);
#[allow(non_camel_case_types, dead_code)]
pub struct lmq_message_ref_t(MessageRef<'static>);

#[allow(non_camel_case_types, dead_code)]
pub struct lmq_vec_t(Vec<u8>);

impl From<LinkMessageQueue> for lmq_t {
    fn from(value: LinkMessageQueue) -> Self { Self(value) }
}
impl From<MessageConsumer> for lmq_consumer_t {
    fn from(value: MessageConsumer) -> Self { Self(value) }
}
impl From<NextMessage> for lmq_message_t {
    fn from(value: NextMessage) -> Self { Self(value) }
}
impl From<MessageRef<'static>> for lmq_message_ref_t {
    fn from(value: MessageRef<'static>) -> Self { Self(value) }
}
impl From<Vec<u8>> for lmq_vec_t {
    fn from(value: Vec<u8>) -> Self { Self(value) }
}

ffi_fn! {
    fn lmq_new() -> *mut lmq_t {
        Box::into_raw(Box::new(lmq_t(LinkMessageQueue::new())))
    }
}

ffi_fn! {
    fn lmq_destroy(queue: *mut lmq_t) {
        drop(unsafe { Box::from_raw(queue) });
    }
}

ffi_fn! {
    fn lmq_push(queue: *mut lmq_t, data: *const u8, len: usize) {
        let queue = unsafe { &mut *queue };
        let payload = unsafe { std::slice::from_raw_parts(data, len) };
        queue.0.push(payload.to_vec());
    }
}

ffi_fn! {
    fn lmq_consumer_new(queue: *const lmq_t) -> *mut lmq_consumer_t {
        let queue = unsafe { &*queue };
        Box::into_raw(Box::new(lmq_consumer_t(queue.0.create_consumer())))
    }
}

ffi_fn! {
    fn lmq_consumer_destroy(consumer: *mut lmq_consumer_t) {
        drop(unsafe { Box::from_raw(consumer) });
    }
}

ffi_fn! {
    fn lmq_consumer_try_next(consumer: *mut lmq_consumer_t) -> *mut lmq_message_t {
        let consumer = unsafe { &mut *consumer };
        match consumer.0.try_next() {
            Some(message) => Box::into_raw(Box::new(lmq_message_t(message))),
            None => std::ptr::null_mut(),
        }
    }
}

ffi_fn! {
    fn lmq_consumer_wait(consumer: *mut lmq_consumer_t) -> *mut lmq_message_t {
        let consumer = unsafe { &mut *consumer };
        Box::into_raw(Box::new(lmq_message_t(consumer.0.blocking_next())))
    }
}

#[allow(non_camel_case_types)]
#[repr(C)]
pub enum lmq_action_t {
    LMQ_ACTION_CONTINUE = 0,
    LMQ_ACTION_PAUSE = 1,
    LMQ_ACTION_DEREGISTER = -1,
}

#[cfg(feature = "tokio")]
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
struct CallbackId(*const lmq_t, lmq_msg_callback_t);
#[cfg(feature = "tokio")]
unsafe impl Send for CallbackId {  }
#[cfg(feature = "tokio")]
unsafe impl Sync for CallbackId {  }
#[cfg(feature = "tokio")]
static TOKIO_RT: LazyLock<Mutex<Option<tokio::runtime::Runtime>>>  = LazyLock::new(|| Mutex::new(None));
#[cfg(feature = "tokio")]
static LMQ_WAIT_TASKS: LazyLock<Mutex<HashMap<CallbackId, (JoinHandle<()>, tokio::sync::mpsc::Sender<lmq_resume_mode_t>)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[allow(non_camel_case_types)]
pub type lmq_msg_callback_t = extern "C" fn(message: *mut lmq_message_t, user_data: *mut c_void) -> lmq_action_t;

#[derive(Clone, Copy)]
struct UserData(*mut c_void);
unsafe impl Send for UserData {  }
impl From<UserData> for *mut c_void {
    fn from(value: UserData) -> Self {
        value.0
    }
}

#[cfg(feature = "tokio")]
fn deregister_handler(callback_id: &CallbackId, abort: Option<&tokio::runtime::Handle>, check_shutdown_tokio_runtime: bool) -> bool {
    let mut lock = LMQ_WAIT_TASKS.lock();
    let found_handler = if let Some((old_handle, _old_signal_tx)) = lock.remove(callback_id) {
        if let Some(rt) = abort {
            abort_handler_backend(rt, old_handle);
        }
        true
    } else {
        false
    };
    if check_shutdown_tokio_runtime && lock.is_empty() {
        drop(TOKIO_RT.lock().take());
    }
    found_handler
}

#[cfg(feature = "tokio")]
fn abort_handler_backend(rt: &tokio::runtime::Handle, handle: tokio::task::JoinHandle<()>) {
    handle.abort();
    match rt.block_on(handle) {
        Ok(_) => (),
        Err(e) => if e.is_panic() {
            // Resume the panic on the main thread.
            panic::resume_unwind(e.into_panic());
        },
    }
}

#[cfg(feature = "tokio")]
ffi_fn! {
    fn lmq_register_handler(queue: *const lmq_t, callback: lmq_msg_callback_t, start_paused: bool, user_data: *mut c_void) {
        let queue_rust = unsafe { &*queue };
        let mut consumer = queue_rust.0.create_consumer();

        let callback_id = CallbackId(queue, callback);
    
        // Make `user_data` Send by using a wrapper.
        let user_data = UserData(user_data);

        let rt = {
            TOKIO_RT.lock().get_or_insert_with(|| tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to initialize tokio runtime!")).handle().clone()
        };

        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel(1);
        
        let join_handle = rt.spawn(async move {
            let mut resume_mode = lmq_resume_mode_t::LMQ_RESUME_WAIT;
            // If starting paused, wait for a resume signal before entering the main loop.
            if start_paused {
                match signal_rx.recv().await {
                    Some(new_resume_mode) => {
                        resume_mode = new_resume_mode;
                    },
                    None => {
                        // If the sender is gone, we have been deregistered.
                        return;
                    },
                }
            }
            loop {
                let message = match resume_mode {
                    lmq_resume_mode_t::LMQ_RESUME_WAIT => Box::into_raw(Box::new(lmq_message_t(consumer.next().await))),
                    lmq_resume_mode_t::LMQ_RESUME_TRY => match consumer.try_next() {
                        Some(message) => Box::into_raw(Box::new(lmq_message_t(message))),
                        None => std::ptr::null_mut(),
                    },
                };
                match callback(
                    message,
                    user_data.into()
                ) {
                    lmq_action_t::LMQ_ACTION_CONTINUE => (),
                    lmq_action_t::LMQ_ACTION_PAUSE => {
                        match signal_rx.recv().await {
                            Some(new_resume_mode) => {
                                resume_mode = new_resume_mode;
                            },
                            None => {
                                // If the sender is gone, we have been deregistered.
                                break;
                            },
                        }
                    },
                    lmq_action_t::LMQ_ACTION_DEREGISTER => {
                        let _ = deregister_handler(&callback_id, None, true);
                        break;
                    },
                }
            }
        });

        if let Some((old_handle, _old_signal_tx)) = LMQ_WAIT_TASKS.lock().insert(callback_id, (join_handle, signal_tx)) {
            abort_handler_backend(&rt, old_handle);
        }
    }
}

#[allow(non_camel_case_types)]
#[repr(C)]
pub enum lmq_resume_mode_t {
    LMQ_RESUME_WAIT = 0,
    LMQ_RESUME_TRY = 1,
}
impl From<c_int> for lmq_resume_mode_t {
    fn from(value: c_int) -> Self {
        match value {
            0 => Self::LMQ_RESUME_WAIT,
            1 => Self::LMQ_RESUME_TRY,
            _ => panic!("invalid resume mode. this should never happen."),
        }
    }
}

#[cfg(feature = "tokio")]
ffi_fn! {
    fn lmq_resume_handler(queue: *const lmq_t, callback: lmq_msg_callback_t, no_block: c_int) -> c_int {
        match LMQ_WAIT_TASKS.lock().get(&CallbackId(queue, callback)) {
            Some((_, signal_tx)) => match signal_tx.blocking_send(no_block.into()) {
                Ok(_) => 0,  // handler resume signal sent.
                Err(_) => -1,  // handler is no longer running and cannot be resumed.
            },
            None => -1,  // handler is no longer running and cannot be resumed.
        }
    }
}

#[cfg(feature = "tokio")]
ffi_fn! {
    fn lmq_deregister_handler(queue: *const lmq_t, callback: lmq_msg_callback_t) -> bool {
        let rt = {
            TOKIO_RT.lock().get_or_insert_with(|| tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to initialize tokio runtime!")).handle().clone()
        };
        deregister_handler(&CallbackId(queue, callback), Some(&rt), true)
    }
}

ffi_fn! {
    fn lmq_message_destroy(message: *mut lmq_message_t) {
        drop(unsafe { Box::from_raw(message) });
    }
}

ffi_fn! {
    fn lmq_message_peek(message: *const lmq_message_t, p: *mut *const u8, len: *mut usize) -> *mut lmq_message_ref_t {
        let message = unsafe { &*message };
        match message.0.read() {
            Some(message_ref) => unsafe {
                *p = message_ref.as_ptr();
                *len = message_ref.len();
                Box::into_raw(Box::new(lmq_message_ref_t(message_ref)))
            },
            None => unsafe {
                *p = std::ptr::null();
                *len = 0;
                std::ptr::null_mut()
            },
        }
    }
}

ffi_fn! {
    fn lmq_message_peek_release(message_ref: *mut lmq_message_ref_t) {
        drop(unsafe { Box::from_raw(message_ref) });
    }
}

ffi_fn! {
    fn lmq_message_claim(message: *mut lmq_message_t, p: *mut *mut u8, len: *mut usize) -> *mut lmq_vec_t {
        let message = unsafe { &mut *message };
        match message.0.claim() {
            Some(mut payload) => unsafe {
                *p = payload.as_mut_ptr();
                *len = payload.len();
                Box::into_raw(Box::new(lmq_vec_t(payload)))
            },
            None => unsafe {
                *p = std::ptr::null_mut();
                *len = 0;
                std::ptr::null_mut()
            },
        }
    }
}

ffi_fn! {
    fn lmq_vec_destroy(data: *mut lmq_vec_t) {
        drop(unsafe { Box::from_raw(data) });
    }
}

