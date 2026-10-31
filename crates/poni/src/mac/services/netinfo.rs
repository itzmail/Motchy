//! The Mac's name and network addresses, for the System page.

use objc2_core_foundation::{CFRetained, CFString};
use std::ffi::{c_char, c_void, CStr};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

#[link(name = "SystemConfiguration", kind = "framework")]
extern "C" {
    fn SCDynamicStoreCopyComputerName(store: *const c_void, encoding: *mut u32) -> *const CFString;
}

/// The Mac's name in System Settings > General > About.
pub(crate) fn computer_name() -> Option<String> {
    let raw = unsafe { SCDynamicStoreCopyComputerName(std::ptr::null(), std::ptr::null_mut()) };
    std::ptr::NonNull::new(raw as *mut CFString).map(|p| unsafe { CFRetained::from_raw(p) }.to_string()).filter(|n| !n.trim().is_empty())
}

// ---------------------------------------------------------------- the address

#[repr(C)]
struct IfAddrs {
    ifa_next: *mut IfAddrs,
    ifa_name: *mut c_char,
    ifa_flags: u32,
    ifa_addr: *mut SockAddrIn,
    ifa_netmask: *mut c_void,
    ifa_dstaddr: *mut c_void,
    ifa_data: *mut c_void,
}

#[repr(C)]
struct SockAddrIn {
    sin_len: u8,
    sin_family: u8,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

extern "C" {
    fn getifaddrs(out: *mut *mut IfAddrs) -> i32;
    fn freeifaddrs(ifa: *mut IfAddrs);
}

const AF_INET: u8 = 2;
const IFF_UP: u32 = 0x1;
const IFF_LOOPBACK: u32 = 0x8;
const IFF_RUNNING: u32 = 0x40;

/// Each connected interface's IPv4 address, by name ("en0"); the System
/// page reads them too.
pub(super) fn interface_ipv4s() -> Vec<(String, Ipv4Addr)> {
    let mut out = Vec::new();
    let mut head: *mut IfAddrs = std::ptr::null_mut();
    if unsafe { getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut p = head;
    while !p.is_null() {
        let a = unsafe { &*p };
        let up = a.ifa_flags & (IFF_UP | IFF_RUNNING) == (IFF_UP | IFF_RUNNING) && a.ifa_flags & IFF_LOOPBACK == 0;
        if up && !a.ifa_addr.is_null() && !a.ifa_name.is_null() {
            let sa = unsafe { &*a.ifa_addr };
            if sa.sin_family == AF_INET {
                let name = unsafe { CStr::from_ptr(a.ifa_name) }.to_string_lossy().into_owned();
                out.push((name, Ipv4Addr::from(u32::from_be(sa.sin_addr))));
            }
        }
        p = a.ifa_next;
    }
    unsafe { freeifaddrs(head) };
    out
}

/// The address macOS would send internet traffic from (no packet is sent:
/// connecting a UDP socket only picks the route).
pub(super) fn route_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}
