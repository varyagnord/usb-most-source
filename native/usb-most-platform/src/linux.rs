//! Linux removal uses UDisks2 in the host's mount namespace. No shell, mounts,
//! device-node writes or force removal. Only an owned VHCI import is accepted.
mod eject;
mod owned_usb;
mod udisks;

pub use owned_usb::OwnedUsb;

use usb_most_protocol::eject::Outcome;

/// The coordinator must retain the import and operation ledger across timeout,
/// disconnect and a lost D-Bus reply. Do not cancel/drop its owner as a deadline
/// shortcut. This adapter does not detach USB, retry a method, or authorize a UI
/// command; it reports only the result of the host OS operation.
pub async fn eject(device: &OwnedUsb) -> Outcome {
    udisks::eject(device).await
}
