//! The PC's network address, for the System page.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// The local address of a route to the internet (no packet is sent).
pub(crate) fn route_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}
