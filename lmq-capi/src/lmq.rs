use std::{ffi::{c_int, c_void}, panic};

use lmq_rs::{LinkMessageQueue, MessageConsumer, MessageRef, NextMessage};

#[allow(non_camel_case_types)]
pub struct lmq_t(pub(crate) LinkMessageQueue);
#[allow(non_camel_case_types)]
pub struct lmq_consumer_t(MessageConsumer);
#[allow(non_camel_case_types)]
pub struct lmq_message_t(pub(crate) NextMessage);
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

#[allow(non_camel_case_types)]
pub type lmq_msg_callback_t = extern "C" fn(message: *mut lmq_message_t, user_data: *mut c_void) -> lmq_action_t;

#[derive(Clone, Copy)]
pub(crate) struct UserData(pub(crate) *mut c_void);
unsafe impl Send for UserData {  }
impl From<UserData> for *mut c_void {
    fn from(value: UserData) -> Self {
        value.0
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

