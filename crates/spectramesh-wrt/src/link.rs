//! Raw Ethernet frames on a Linux network device.
//!
//! SpectraMesh frames travel directly inside Ethernet frames with their own
//! EtherType, with no IP. That works on anything Linux presents as an Ethernet
//! device: Ethernet ports, fiber SFP ports, Wi-Fi interfaces in 802.11s mesh
//! mode (including HaLow), and virtual devices such as veth pairs.
//!
//! Ethernet pads short frames to its 60-byte minimum, so each SpectraMesh
//! frame is preceded by its length as two big-endian bytes, and receivers
//! drop the padding.
//!
//! Opening a socket needs root or `CAP_NET_RAW`.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, RwLock};

/// IEEE 802 "Local Experimental Ethertype 1", reserved for experiments like this one.
//
// TODO: register an EtherType before any public release.
pub const ETHERTYPE: u16 = 0x88b5;

/// Bytes the length prefix adds to every frame.
pub const LENGTH_PREFIX_LEN: usize = 2;

pub type Mac = [u8; 6];

pub const BROADCAST_MAC: Mac = [0xff; 6];

/// A neighbor's address on a hue: a MAC address on Ethernet and radio
/// modems, or an IP address and port on UDP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkAddr {
    Mac(Mac),
    Udp(std::net::SocketAddr),
}

/// A network device, as described in `/sys/class/net`.
#[derive(Clone, Debug)]
pub struct Device {
    pub name: String,
    pub index: i32,
    pub mtu: usize,
    /// Whether it's administratively up (`ip link set up`).
    pub up: bool,
}

impl Device {
    pub fn open(name: &str) -> io::Result<Device> {
        let read = |file: &str| -> io::Result<String> {
            std::fs::read_to_string(format!("/sys/class/net/{name}/{file}"))
                .map(|s| s.trim().to_owned())
                .map_err(|err| io::Error::new(err.kind(), format!("network device {name}: {err}")))
        };
        let invalid =
            |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{name}: bad {what}"));
        Ok(Device {
            name: name.into(),
            index: read("ifindex")?.parse().map_err(|_| invalid("ifindex"))?,
            mtu: read("mtu")?.parse().map_err(|_| invalid("MTU"))?,
            up: read("flags")
                .ok()
                .and_then(|flags| u32::from_str_radix(flags.trim_start_matches("0x"), 16).ok())
                .is_some_and(|flags| flags & libc::IFF_UP as u32 != 0),
        })
    }
}

/// Adds the length prefix.
pub fn wrap(frame: &[u8]) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(LENGTH_PREFIX_LEN + frame.len());
    wrapped.extend_from_slice(&(frame.len() as u16).to_be_bytes());
    wrapped.extend_from_slice(frame);
    wrapped
}

/// Removes the length prefix and any padding. `None` if the frame is cut short.
pub fn unwrap(wrapped: &[u8]) -> Option<&[u8]> {
    let (len, rest) = wrapped.split_first_chunk::<LENGTH_PREFIX_LEN>()?;
    rest.get(..usize::from(u16::from_be_bytes(*len)))
}

/// A socket sending and receiving SpectraMesh frames on one device, by name:
/// when the device is deleted and created again, as `wifi up` does to Wi-Fi
/// interfaces, [`reattach`](Self::reattach) binds a new socket to it.
pub struct EthernetSocket {
    name: String,
    bound: RwLock<Bound>,
}

/// A socket bound to one device's index.
#[derive(Clone)]
struct Bound {
    fd: Arc<OwnedFd>,
    index: i32,
}

impl EthernetSocket {
    pub fn open(device: &Device) -> io::Result<Self> {
        Ok(EthernetSocket {
            name: device.name.clone(),
            bound: RwLock::new(Bound {
                fd: Arc::new(bind(device.index)?),
                index: device.index,
            }),
        })
    }

    /// Looks the device up again. If it's a new device with the same name,
    /// moves to it and returns true. A device taken down and up again keeps
    /// its index and needs nothing: its socket works again once it's up.
    pub fn reattach(&self) -> io::Result<bool> {
        let device = Device::open(&self.name)?;
        if device.index == self.bound().index {
            return Ok(false);
        }
        let fd = Arc::new(bind(device.index)?);
        *self.bound.write().expect("no panics while holding it") = Bound {
            fd,
            index: device.index,
        };
        Ok(true)
    }

    fn bound(&self) -> Bound {
        self.bound
            .read()
            .expect("no panics while holding it")
            .clone()
    }

    /// Waits for a frame. Returns the sender's MAC address and the
    /// SpectraMesh frame, or `None` for frames this host sent itself or that
    /// are cut short.
    pub fn recv<'b>(&self, buf: &'b mut [u8]) -> io::Result<Option<(Mac, &'b [u8])>> {
        // SAFETY: sockaddr_ll is plain data, valid when zeroed.
        let mut addr: libc::sockaddr_ll = unsafe { mem::zeroed() };
        let mut addr_len = mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
        // Hold this socket, not the lock, while waiting: a reattach can
        // replace it meanwhile.
        let fd = self.bound().fd;
        // SAFETY: `buf` and `addr` are valid for writes of the lengths given.
        let len = unsafe {
            libc::recvfrom(
                fd.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
                (&raw mut addr).cast(),
                &mut addr_len,
            )
        };
        if len < 0 {
            return Err(io::Error::last_os_error());
        }
        // Packet sockets also see this host's outgoing frames.
        if addr.sll_pkttype == libc::PACKET_OUTGOING {
            return Ok(None);
        }
        let mut src = [0; 6];
        src.copy_from_slice(&addr.sll_addr[..6]);
        Ok(unwrap(&buf[..len as usize]).map(|frame| (src, frame)))
    }

    /// Sends a SpectraMesh frame to `dst`, which may be [`BROADCAST_MAC`].
    pub fn send(&self, dst: Mac, frame: &[u8]) -> io::Result<()> {
        let wrapped = wrap(frame);
        let bound = self.bound();
        let addr = link_addr(bound.index, dst);
        // SAFETY: `wrapped` and `addr` are valid for reads of the lengths given.
        let sent = unsafe {
            libc::sendto(
                bound.fd.as_raw_fd(),
                wrapped.as_ptr().cast(),
                wrapped.len(),
                0,
                (&raw const addr).cast(),
                mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Opens a packet socket for SpectraMesh frames on the device with `index`.
fn bind(index: i32) -> io::Result<OwnedFd> {
    // SOCK_DGRAM: the kernel adds and strips the Ethernet header.
    // SAFETY: plain syscall; the result is checked before use.
    let fd = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            i32::from(ETHERTYPE.to_be()),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a valid descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };

    let addr = link_addr(index, BROADCAST_MAC);
    // SAFETY: `addr` is a valid sockaddr_ll and the length matches it.
    let result = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const addr).cast(),
            mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

fn link_addr(device_index: i32, mac: Mac) -> libc::sockaddr_ll {
    // SAFETY: sockaddr_ll is plain data, valid when zeroed.
    let mut addr: libc::sockaddr_ll = unsafe { mem::zeroed() };
    addr.sll_family = libc::AF_PACKET as u16;
    addr.sll_protocol = ETHERTYPE.to_be();
    addr.sll_ifindex = device_index;
    addr.sll_halen = 6;
    addr.sll_addr[..6].copy_from_slice(&mac);
    addr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefix_strips_padding() {
        let mut padded = wrap(b"hello");
        assert_eq!(padded.len(), 7);
        padded.resize(46, 0);
        assert_eq!(unwrap(&padded), Some(&b"hello"[..]));
        assert_eq!(unwrap(&padded[..5]), None);
        assert_eq!(unwrap(&[0]), None);
    }
}
