use std::{ffi::{c_char, c_int, c_void, CStr}, sync::LazyLock};

use lmq::lmq_consumer_t;
use parking_lot::Mutex;
use skycurrent_rs::{close, init, iter_stream, send_stream, set_global_project_dir, set_global_should_collect};

ffi_fn! {
    fn sc_set_global_project_dir(project_dir: *const c_char) -> c_int {
        let c_str = unsafe { CStr::from_ptr(project_dir) };
        match c_str.to_str() {
            Ok(path_str) => {
                set_global_project_dir(path_str);
                0
            },
            Err(_) => {
                1
            },
        }
    }
}

#[allow(non_camel_case_types)]
pub type skycurrent_should_collect_callback_t = extern "C" fn(p: *const u8, len: usize, user_data: *mut c_void) -> bool;

#[derive(Clone, Copy)]
struct UserData(*mut c_void);
unsafe impl Send for UserData {  }
unsafe impl Sync for UserData {  }
impl From<UserData> for *mut c_void {
    fn from(value: UserData) -> Self {
        value.0
    }
}

ffi_fn! {
    fn sc_set_global_should_collect(should_collect: skycurrent_should_collect_callback_t, user_data: *mut c_void) {
        let user_data: UserData = UserData(user_data);
        let callback = move |data: &[u8]| -> bool {
            should_collect(data.as_ptr(), data.len(), user_data.into())
        };
        set_global_should_collect(callback)
    }
}

static TOKIO_RT: LazyLock<Mutex<Option<tokio::runtime::Runtime>>>  = LazyLock::new(|| Mutex::new(None));

ffi_fn! {
    fn sc_close() {
        close();
        drop(TOKIO_RT.lock().take());
    }
}

ffi_fn! {
    fn sc_init() {
        let rt = {
            TOKIO_RT.lock().get_or_insert_with(|| tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to initialize tokio runtime!")).handle().clone()
        };
        rt.block_on(init())
    }
}

ffi_fn! {
    fn sc_send_stream(data: *const u8, len: usize, header_size: usize) {
        let payload = unsafe { std::slice::from_raw_parts(data, len) };
        send_stream(payload, header_size)
    }
}

ffi_fn! {
    fn sc_iter_stream() -> *mut lmq_consumer_t {
        Box::into_raw(Box::new(iter_stream().into()))
    }
}

