use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct LinkMessageQueue {
    inner: crate::LinkMessageQueue,
}

#[wasm_bindgen]
impl LinkMessageQueue {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: crate::LinkMessageQueue::new(),
        }
    }

    pub fn push(&mut self, payload: Vec<u8>) {
        self.inner.push(payload);
    }

    pub fn create_consumer(&self) -> MessageConsumer {
        MessageConsumer {
            inner: self.inner.create_consumer(),
        }
    }
}

#[wasm_bindgen]
pub struct MessageConsumer {
    inner: crate::MessageConsumer,
}

#[wasm_bindgen]
impl MessageConsumer {
    #[wasm_bindgen]
    pub async fn next(&mut self) -> NextMessage {
        NextMessage { inner: self.inner.next().await }
    }

    #[wasm_bindgen]
    pub fn try_next(&mut self) -> Option<NextMessage> {
        self.inner.try_next().map(|msg| NextMessage { inner: msg })
    }

    #[wasm_bindgen]
    pub fn blocking_next(&mut self) -> NextMessage {
        NextMessage { inner: self.inner.blocking_next() }
    }
}

#[wasm_bindgen]
pub struct NextMessage {
    inner: crate::NextMessage,
}

#[wasm_bindgen]
impl NextMessage {
    #[wasm_bindgen]
    pub fn read(&self) -> Option<Vec<u8>> {
        self.inner.read().map(|msg_ref| msg_ref.to_vec())
    }

    #[wasm_bindgen]
    pub fn claim(&mut self) -> Option<Vec<u8>> {
        self.inner.claim()
    }
}

