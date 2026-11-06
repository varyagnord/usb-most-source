//! USB Most native implementation. Derived from the project's GPLv3 C engine.
#![feature(allocator_ext)] // fallible Box allocation for bounded USB requests
#![deny(unsafe_op_in_unsafe_fn)]
mod engine;
mod interfaces;
mod jni_api;
mod linux;
mod registry;

#[cfg(feature = "diagnostics")]
mod diagnostics;

mod connection_jni;
mod managed_jni;
mod policy;
mod policy_jni;

pub mod agent;
mod wan;
pub use usb_most_protocol::wire;

pub mod connector;
