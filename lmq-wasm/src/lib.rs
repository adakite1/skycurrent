use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::js_sys::Uint8Array;

use lmq_rs::MessageRef;

/// Linked message queue.
/// 
/// Must be freed with the `free` method after use.
#[wasm_bindgen]
pub struct LinkMessageQueue {
    inner: lmq_rs::LinkMessageQueue,
}

#[wasm_bindgen]
impl LinkMessageQueue {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: lmq_rs::LinkMessageQueue::new(),
        }
    }
    /// Push a message into the end of the queue.
    #[wasm_bindgen]
    pub fn push(&mut self, payload: Vec<u8>) {
        self.inner.push(payload);
    }
    /// Create a new message consumer which consumes from the queue.
    #[wasm_bindgen]
    pub fn create_consumer(&self) -> MessageConsumer {
        MessageConsumer {
            inner: self.inner.create_consumer(),
        }
    }
}

/// Message consumer.
/// 
/// Must be freed with the `free` method after use.
#[wasm_bindgen]
pub struct MessageConsumer {
    inner: lmq_rs::MessageConsumer,
}

#[wasm_bindgen]
impl MessageConsumer {
    /// Asynchronously wait for the next message to arrive.
    #[wasm_bindgen]
    pub async fn next(&mut self) -> NextMessage {
        NextMessage { inner: self.inner.next().await, read_ref: None }
    }
    /// Attempt to return the next unclaimed message without blocking.
    #[wasm_bindgen]
    pub fn try_next(&mut self) -> Option<NextMessage> {
        self.inner.try_next().map(|msg| NextMessage { inner: msg, read_ref: None })
    }
    /// Block until the next unclaimed message arrives.
    #[wasm_bindgen]
    pub fn blocking_next(&mut self) -> NextMessage {
        NextMessage { inner: self.inner.blocking_next(), read_ref: None }
    }
}

/// Message.
/// 
/// Must be freed with the `free` method after use. Freeing the message will also invalidate any read views obtained. It will not invalidate claimed data.
#[wasm_bindgen]
pub struct NextMessage {
    inner: lmq_rs::NextMessage,
    read_ref: Option<MessageRef<'static>>,
}

#[wasm_bindgen]
impl NextMessage {
    /// Read the message without claiming it. Zero-copy.
    /// 
    /// If called multiple times, only the view returned from the latest call is guaranteed to be valid.
    /// 
    /// The view will remain valid until either the `NextMessage` is dropped or the data is claimed.
    /// 
    /// Returns `undefined` if the message has since been claimed.
    #[wasm_bindgen]
    pub fn read(&mut self) -> Option<Uint8Array> {
        drop(self.read_ref.take());
        self.read_ref = unsafe {
            std::mem::transmute(self.inner.read())
        };
        match self.read_ref.as_ref() {
            Some(msg_ref) => {
                // Create a Uint8Array view of the data without copying.
                Some(
                    unsafe { Uint8Array::view(&msg_ref) }
                )
            },
            None => None,
        }
    }
    /// Claim the message and return the payload.
    /// 
    /// Returns `undefined` if the message has since been claimed.
    #[wasm_bindgen]
    pub fn claim(&mut self) -> Option<Vec<u8>> {
        drop(self.read_ref.take());
        self.inner.claim()
    }
}

