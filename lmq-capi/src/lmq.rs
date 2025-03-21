use std::{ffi::c_void, panic};

use lmq::{LinkMessageQueue, MessageConsumer, MessageRef, NextMessage};

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
    LMQ_ACTION_DEREGISTER = -1,
}

#[cfg(feature = "tokio")]
thread_local! {
    static TOKIO_RT: std::cell::LazyCell<tokio::runtime::Runtime>  = std::cell::LazyCell::new(|| tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to initialize tokio runtime!"));
    static LMQ_WAIT_TASKS: std::cell::RefCell<std::collections::HashMap<(*const lmq_t, lmq_msg_callback_t), tokio::task::JoinHandle<()>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

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
fn abort_handler_backend(rt: &std::cell::LazyCell<tokio::runtime::Runtime>, handle: tokio::task::JoinHandle<()>) {
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
    fn lmq_register_handler(queue: *const lmq_t, callback: lmq_msg_callback_t, user_data: *mut c_void) {
        let queue_rust = unsafe { &*queue };
        let mut consumer = queue_rust.0.create_consumer();
    
        // Make `user_data` Send by using a wrapper.
        let user_data = UserData(user_data);
    
        TOKIO_RT.with(|rt| {
            let join_handle = rt.spawn(async move {
                loop {
                    let message = consumer.next().await;
                    let message = Box::into_raw(Box::new(lmq_message_t(message)));
                    match callback(
                        message,
                        user_data.into()
                    ) {
                        lmq_action_t::LMQ_ACTION_CONTINUE => (),
                        lmq_action_t::LMQ_ACTION_DEREGISTER => {
                            // Note that we don't remove ourselves from `LMQ_WAIT_TASKS` since it's not really necessary; awaiting our join handle will simply return immediately as we are exiting naturally, so if the same callback is ever re-registered, our handle will be cleaned up then.
                            break;
                        },
                    }
                }
            });
    
            LMQ_WAIT_TASKS.with_borrow_mut(|cell| {
                if let Some(old_handle) = cell.insert((queue, callback), join_handle) {
                    abort_handler_backend(rt, old_handle);
                }
            });
        });
    }
}

#[cfg(feature = "tokio")]
ffi_fn! {
    fn lmq_deregister_handler(queue: *const lmq_t, callback: lmq_msg_callback_t) -> bool {
        TOKIO_RT.with(|rt| {
            LMQ_WAIT_TASKS.with_borrow_mut(|cell| {
                if let Some(old_handle) = cell.remove(&(queue, callback)) {
                    abort_handler_backend(rt, old_handle);
                    true
                } else {
                    false
                }
            })
        })
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

