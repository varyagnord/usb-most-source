//! Small, audited OS boundary. Portable connection/protocol code stays safe Rust.
#![deny(unsafe_code)]
#[cfg(any(windows, test))]
mod import_identity;
#[cfg(all(target_os = "linux", feature = "linux-eject"))]
pub mod linux;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;
