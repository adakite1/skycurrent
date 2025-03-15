use std::path::{Path, PathBuf};

use thiserror::Error;

/// Macro to join an actor thread.
/// 
/// Symbol `IpcError::NotInitialized` must be within scope.
macro_rules! actor_join {
    ($join_handle:ident) => {{
        if let Some(join_handle) = $join_handle.lock().take() {
            if let Err(e) = join_handle.join() {
                eprintln!("{}: backing exited with panic: {:?}", stringify!($join_handle), e);
            }
        }
    }};
}
pub(crate) use actor_join;

/// Macro to send a message to an actor thread and optionally wait for a response.
/// 
/// Symbol `IpcError::NotInitialized` must be within scope.
/// 
/// # Parameters
/// - `$sender_tx`: The thread-local static sender variable to use.
/// - `$message_expr`: The expression that constructs the message to send.
/// - `$recv`: The `mpsc` channel to wait for a response on.
/// 
/// # Returns
/// - [`Result<(), E>`] where E is an [`IpcError`].
/// - [`Result<T, E>`] where T is the response type from the actor and E is an [`IpcError`] if `$recv` is provided.
macro_rules! actor_call {
    ($sender_tx:ident, $message_expr:expr) => {{
        if let Some(sender_tx) = $sender_tx.read().as_ref() {
            if let Err(_) = sender_tx.send($message_expr) {
                Err(IpcError::BackingCrashedOrNotStarted(stringify!($sender_tx)))
            } else {
                Ok(())
            }
        } else {
            Err(IpcError::NotInitialized)
        }
    }};
    ($sender_tx:ident, $message_expr:expr, $recv:ident) => {{
        if let Some(sender_tx) = $sender_tx.read().as_ref() {
            if let Err(_) = sender_tx.send($message_expr) {
                return Err(IpcError::BackingCrashedOrNotStarted(stringify!($sender_tx)));
            }
            if let Ok(result) = $recv.recv() {
                Ok(result?)
            } else {
                Err(IpcError::BackingCrashedOrNotStarted(stringify!($sender_tx)))
            }
        } else {
            Err(IpcError::NotInitialized)
        }
    }};
}
pub(crate) use actor_call;

/// Macro to join an actor thread with a thread-local join handle.
/// 
/// Symbol `IpcError::NotInitialized` must be within scope.
macro_rules! actor_join_tl {
    ($join_handle:ident) => {
        $join_handle.with_borrow_mut(|cell| {
            if let Some(join_handle) = cell.take() {
                if let Err(e) = join_handle.join() {
                    eprintln!("{}: backing exited with panic: {:?}", stringify!($join_handle), e);
                }
            }
        })
    };
}
pub(crate) use actor_join_tl;

/// Macro to send a message to an actor thread with a thread-local Sender and optionally wait for a response.
/// 
/// Symbol `IpcError::NotInitialized` must be within scope.
/// 
/// # Parameters
/// - `$sender_tx`: The thread-local static sender variable to use.
/// - `$message_expr`: The expression that constructs the message to send.
/// - `$recv`: The `mpsc` channel to wait for a response on.
/// 
/// # Returns
/// - [`Result<(), E>`] where E is an [`IpcError`].
/// - [`Result<T, E>`] where T is the response type from the actor and E is an [`IpcError`] if `$recv` is provided.
macro_rules! actor_call_tl {
    ($sender_tx:ident, $message_expr:expr) => {
        $sender_tx.with(|cell| {
            if let Some(sender_tx) = cell.get() {
                if let Err(_) = sender_tx.send($message_expr) {
                    Err(IpcError::BackingCrashedOrNotStarted(stringify!($sender_tx)))
                } else {
                    Ok(())
                }
            } else {
                Err(IpcError::NotInitialized)
            }
        })
    };
    ($sender_tx:ident, $message_expr:expr, $recv:ident) => {
        $sender_tx.with(|cell| {
            if let Some(sender_tx) = cell.get() {
                if let Err(_) = sender_tx.send($message_expr) {
                    return Err(IpcError::BackingCrashedOrNotStarted(stringify!($sender_tx)));
                }
                if let Ok(result) = $recv.recv() {
                    Ok(result?)
                } else {
                    Err(IpcError::BackingCrashedOrNotStarted(stringify!($sender_tx)))
                }
            } else {
                Err(IpcError::NotInitialized)
            }
        })
    };
}
pub(crate) use actor_call_tl;

/// Possible errors during initialization.
#[derive(Error, Debug, Clone)]
pub enum InitError {
    /// Expected a project folder, but instead was provided with a path that does not exist or is not a folder.
    #[error("the provided path '{0}' for initializing SkyCurrent does not exist or is not a folder!")]
    NotAFolder(PathBuf),
    /// Could not canonicalize the provided project path.
    #[error("failed to canonicalize the provided project root path '{0}': {1}")]
    FailedToCanonicalizeProjectPath(PathBuf, std::io::ErrorKind),
    /// Could not create a folder.
    #[error("failed to create directory at '{0}': {1}")]
    CreateDirError(PathBuf, std::io::ErrorKind),
    /// Failed to bind to address.
    #[cfg(feature = "tokio")]
    #[error("failed to bind to address")]
    TcpListenerBindError(tokio::io::ErrorKind),
}

pub(crate) struct ProjectDirectoryPaths {
    pub root: PathBuf,
    pub res: PathBuf,
    pub tmp: PathBuf,
}
pub(crate) fn build_project_dir_structure(path: &Path) -> Result<ProjectDirectoryPaths, InitError> {
    // We are expecting a project folder. If the project folder does not exist or is not a directory, we cannot continue.
    if !path.is_dir() {
        return Err(InitError::NotAFolder(path.to_path_buf()));
    }
    // Canonicalize the project folder path.
    let path = dunce::canonicalize(path).map_err(|e| InitError::FailedToCanonicalizeProjectPath(path.to_path_buf(), e.kind()))?;

    // Set up common temporary directories.
    //  The res/ directory is for things that should not be checked into git but still makes sense to be kept around for a long time.
    //  The tmp/ directory is for things that should not be checked into git as they are temporary files.
    let res = path.join("res");
    let tmp = path.join("tmp");
    std::fs::create_dir_all(&res).map_err(|e| InitError::CreateDirError(res.to_path_buf(), e.kind()))?;
    std::fs::create_dir_all(&tmp).map_err(|e| InitError::CreateDirError(tmp.to_path_buf(), e.kind()))?;

    Ok(ProjectDirectoryPaths {
        root: path,
        res,
        tmp,
    })
}

#[cfg(feature = "tokio")]
pub(crate) async fn bind_tcp_listener<A: tokio::net::ToSocketAddrs>(addr: A) -> Result<tokio::net::TcpListener, InitError> {
    tokio::net::TcpListener::bind(addr).await.map_err(|e| InitError::TcpListenerBindError(e.kind()))
}

/// Interruption signals that might possibly be received.
#[derive(Debug, Clone, Copy)]
pub enum InterruptSignals {
    /// A `SIGINT` signal was received.
    Interrupt,
    /// A `SIGTERM` signal was received.
    TerminationRequest,
    /// A generic interrupt signal was received.
    GenericInterruptSignal,
}

/// Return value on `try_recv_stream` success.
pub enum TryRecvStreamResult {
    /// A new merge has been completed.
    NewCompleted(Vec<u8>),
    /// More pages might reside in the buffer, waiting to be merged. Will be returned whenever a page was obtained in that run of `try_recv_stream`.
    PotentiallyAvailable,
    /// Out of any accessible pages to merge for now. Will be returned whenever a page was unable to be obtained in that run of `try_recv_stream`.
    OutOfAccessible
}

