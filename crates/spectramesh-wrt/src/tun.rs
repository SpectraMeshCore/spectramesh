//! IPv6 over the mesh, through a TUN device.
//!
//! Every node gets the address `<mesh prefix>:<node ID>`: a `/64` unique
//! local prefix shared by the mesh, followed by the 8-byte node ID as the
//! interface identifier. So the destination address of any packet names the
//! node it's for, with no address assignment or lookup table.
//!
//! Packets the kernel routes into the device are sent to that node, encrypted
//! end to end. Packets that arrive from the mesh are written to the device
//! only if their source address belongs to the node that sent them, as proven
//! by the session, so nodes can't spoof each other's addresses.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem;
use std::net::Ipv6Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;

use spectramesh_core::NodeId;

/// IPv6 requires every link to carry packets of at least this size.
pub const IPV6_MIN_MTU: usize = 1280;

/// The mesh's `/64` prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefix(pub [u8; 8]);

impl Prefix {
    pub fn address(self, node: NodeId) -> Ipv6Addr {
        let mut octets = [0u8; 16];
        octets[..8].copy_from_slice(&self.0);
        octets[8..].copy_from_slice(&node.0);
        Ipv6Addr::from(octets)
    }

    /// The node an address belongs to, if it's in this prefix.
    pub fn node(self, address: &[u8; 16]) -> Option<NodeId> {
        (address[..8] == self.0).then(|| NodeId(address[8..].try_into().expect("8 bytes")))
    }
}

/// The source and destination addresses of an IPv6 packet, or `None` if it
/// isn't one.
pub fn addresses(packet: &[u8]) -> Option<([u8; 16], [u8; 16])> {
    if packet.len() < 40 || packet[0] >> 4 != 6 {
        return None;
    }
    Some((
        packet[8..24].try_into().expect("16 bytes"),
        packet[24..40].try_into().expect("16 bytes"),
    ))
}

pub struct Tun {
    file: File,
    pub name: String,
}

impl Tun {
    /// Creates TUN device `name` with `address` in a `/64` and the given MTU,
    /// and brings it up. Needs root or `CAP_NET_ADMIN`.
    pub fn create(name: &str, address: Ipv6Addr, mtu: usize) -> io::Result<Tun> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open("/dev/net/tun")?;

        let mut req = ifreq(name)?;
        req.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
        // SAFETY: TUNSETIFF reads and writes an ifreq, which `req` is.
        if unsafe { libc::ioctl(file.as_raw_fd(), libc::TUNSETIFF as _, &mut req) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let name = interface_name(&req);

        // Address, MTU and flags are set through an ordinary IPv6 socket.
        // SAFETY: plain syscall; the result is checked.
        let sock =
            unsafe { libc::socket(libc::AF_INET6, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if sock < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `sock` is a valid descriptor nothing else owns.
        let sock = unsafe { OwnedFd::from_raw_fd(sock) };
        let ioctl = |request, arg: *mut libc::c_void| {
            // SAFETY: each call below passes the struct its request expects.
            if unsafe { libc::ioctl(sock.as_raw_fd(), request as _, arg) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        };

        let mut req = ifreq(&name)?;
        req.ifr_ifru.ifru_mtu = mtu as libc::c_int;
        ioctl(libc::SIOCSIFMTU, (&raw mut req).cast())?;

        let c_name = CString::new(name.as_str()).expect("no NUL in name");
        // SAFETY: `c_name` is a valid C string.
        let index = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
        if index == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: in6_ifreq is plain data, valid when zeroed.
        let mut addr: libc::in6_ifreq = unsafe { mem::zeroed() };
        addr.ifr6_addr.s6_addr = address.octets();
        addr.ifr6_prefixlen = 64;
        addr.ifr6_ifindex = index as libc::c_int;
        ioctl(libc::SIOCSIFADDR, (&raw mut addr).cast())?;

        let mut req = ifreq(&name)?;
        ioctl(libc::SIOCGIFFLAGS, (&raw mut req).cast())?;
        // SAFETY: SIOCGIFFLAGS filled in the flags member.
        let flags = unsafe { req.ifr_ifru.ifru_flags };
        req.ifr_ifru.ifru_flags = flags | (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
        ioctl(libc::SIOCSIFFLAGS, (&raw mut req).cast())?;

        Ok(Tun { file, name })
    }

    /// A second handle on the device, for a thread that only reads.
    pub fn try_clone(&self) -> io::Result<Tun> {
        Ok(Tun {
            file: self.file.try_clone()?,
            name: self.name.clone(),
        })
    }

    /// Waits for the next packet the kernel routes into the device.
    pub fn read_packet<'b>(&mut self, buf: &'b mut [u8]) -> io::Result<&'b [u8]> {
        let len = self.file.read(buf)?;
        Ok(&buf[..len])
    }

    pub fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        self.file.write_all(packet)
    }
}

fn ifreq(name: &str) -> io::Result<libc::ifreq> {
    if name.len() >= libc::IFNAMSIZ || name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad interface name {name:?}"),
        ));
    }
    // SAFETY: ifreq is plain data, valid when zeroed.
    let mut req: libc::ifreq = unsafe { mem::zeroed() };
    for (dst, src) in req.ifr_name.iter_mut().zip(name.bytes()) {
        *dst = src as libc::c_char;
    }
    Ok(req)
}

fn interface_name(req: &libc::ifreq) -> String {
    req.ifr_name
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8 as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_name_their_node() {
        let prefix = Prefix([0xfd, 1, 2, 3, 4, 5, 0, 0]);
        let node = NodeId::from_u64(0x1122_3344_5566_7788);
        let address = prefix.address(node);
        assert_eq!(address.to_string(), "fd01:203:405:0:1122:3344:5566:7788");
        assert_eq!(prefix.node(&address.octets()), Some(node));
        assert_eq!(prefix.node(&Ipv6Addr::LOCALHOST.octets()), None);
    }

    #[test]
    fn reads_ipv6_addresses() {
        let mut packet = [0u8; 40];
        packet[0] = 0x60;
        packet[8..24].copy_from_slice(&[1; 16]);
        packet[24..40].copy_from_slice(&[2; 16]);
        assert_eq!(addresses(&packet), Some(([1; 16], [2; 16])));
        packet[0] = 0x45;
        assert_eq!(addresses(&packet), None);
        assert_eq!(addresses(&packet[..39]), None);
    }
}
