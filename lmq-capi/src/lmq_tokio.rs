use std::{collections::HashMap, ffi::{c_int, c_void}, panic, sync::LazyLock};

use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::{lmq_action_t, lmq_consumer_t, lmq_message_t, lmq_msg_callback_t, UserData};

static TOKIO_RT: LazyLock<Mutex<Option<tokio::runtime::Runtime>>>  = LazyLock::new(|| Mutex::new(None));
static LMQ_WAIT_TASKS: LazyLock<Mutex<HashMap<u64, (JoinHandle<()>, tokio::sync::mpsc::Sender<lmq_resume_mode_t>)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn deregister_handler(callback_id: u64, abort: Option<&tokio::runtime::Handle>, check_shutdown_tokio_runtime: bool) -> bool {
    let mut lock = LMQ_WAIT_TASKS.lock();
    let found_handler = if let Some((old_handle, _old_signal_tx)) = lock.remove(&callback_id) {
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

ffi_fn! {
    fn lmq_register_handler(consumer: *mut lmq_consumer_t, callback: lmq_msg_callback_t, start_paused: bool, user_data: *mut c_void) -> u64 {
        let callback_id = {
            let lock = LMQ_WAIT_TASKS.lock();
            loop {
                let callback_id: u64 = rand::random();
                if callback_id != 0 && !lock.contains_key(&callback_id) {
                    break callback_id;
                }
            }
        };
    
        // Make `user_data` Send by using a wrapper.
        let user_data = UserData(user_data);

        // Make `consumer` Send by using a wrapper.
        let consumer = UserData(consumer);

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
                    lmq_resume_mode_t::LMQ_RESUME_WAIT => Box::into_raw(Box::new(lmq_message_t(unsafe { &mut *consumer.get_mut_ptr() }.0.next().await))),
                    lmq_resume_mode_t::LMQ_RESUME_TRY => match unsafe { &mut *consumer.get_mut_ptr() }.0.try_next() {
                        Some(message) => Box::into_raw(Box::new(lmq_message_t(message))),
                        None => std::ptr::null_mut(),
                    },
                };
                match callback(
                    message,
                    user_data.get_mut_ptr()
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
                        let _ = deregister_handler(callback_id, None, true);
                        break;
                    },
                }
            }
        });

        if let Some((_old_handle, _old_signal_tx)) = LMQ_WAIT_TASKS.lock().insert(callback_id, (join_handle, signal_tx)) {
            panic!("callback id collision! handled at id generation, this should never happen.");
        }

        callback_id
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
    fn lmq_resume_handler(callback_id: u64, no_block: c_int) -> c_int {
        match LMQ_WAIT_TASKS.lock().get(&callback_id) {
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
    fn lmq_deregister_handler(callback_id: u64) -> bool {
        let rt = {
            TOKIO_RT.lock().get_or_insert_with(|| tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to initialize tokio runtime!")).handle().clone()
        };
        deregister_handler(callback_id, Some(&rt), true)
    }
}

