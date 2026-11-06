//! Product connection and first-use provisioning; no application relay or cloud.
#![forbid(unsafe_code)]
pub mod bootstrap;
pub mod bridge;
pub mod computers;
pub mod config;
pub mod diagnosis;
pub mod direct;
pub mod gateway;
mod key_guard;
pub mod pairing;
pub mod provision;
#[cfg(feature = "qr-scan")]
pub mod qr;
pub mod security;
pub mod session;
pub mod ssh;
pub mod usb_rules;
pub mod vault;
