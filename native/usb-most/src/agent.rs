//! Linux service adapter. Portable protocol has no dependency on this layer.
pub use usb_most_protocol::agent::*;

pub fn listen_auto(
    address: std::net::SocketAddr,
    remote: std::net::SocketAddr,
    auto: bool,
) -> std::io::Result<()> {
    usb_most_protocol::agent::listen_with_ready(address, remote, move || {
        if auto {
            std::thread::spawn(move || crate::connector::run(address));
        }
    })
}
