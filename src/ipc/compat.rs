//! Cross-platform IPC socket abstraction.
//!
//! On Unix this re-exports `std::os::unix::net` types directly.
//! On Windows it re-exports `uds_windows` which provides the same API
//! on top of Windows Named Pipes, so the rest of the codebase can
//! use `UnixStream` and `UnixListener` unconditionally.

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};

#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
