//! UDP as a hue: SpectraMesh frames in UDP datagrams, for linking nodes
//! across networks SpectraMesh doesn't control, such as the internet or a
//! VPN.
//!
//! A UDP hue has no broadcast, so "broadcast" frames (hellos, updates,
//! floods) go to every configured peer and to every neighbor the daemon has
//! learned on the hue. A node with no configured peers, such as a hub on a
//! public address, learns its neighbors from the frames they send it. Frames
//! are protected by the mesh key as on any hue, so the socket can face the
//! internet; outsiders' frames are dropped.
//!
//! Peers are given as `host:port`. Names are looked up again every few
//! minutes, so peers on dynamic addresses are found again when they move.

use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use log::{info, warn};

/// How often to look peers' names up again.
const RESOLVE_INTERVAL: Duration = Duration::from_secs(300);
/// How soon to try again when a name didn't resolve.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// The sending side of a UDP hue.
pub struct UdpLink {
    socket: UdpSocket,
    /// The configured peers' current addresses.
    peers: Arc<Mutex<Vec<SocketAddr>>>,
}

/// The receiving side, for the hue's reading thread.
pub struct UdpReader {
    socket: UdpSocket,
}

impl UdpLink {
    /// Binds `address` and looks up `peers` (as `host:port`), now and from
    /// then on in the background.
    pub fn open(address: SocketAddr, peers: Vec<String>) -> io::Result<(UdpLink, UdpReader)> {
        let socket = UdpSocket::bind(address)?;
        let reader = UdpReader {
            socket: socket.try_clone()?,
        };
        let ipv6 = socket.local_addr()?.is_ipv6();
        let resolved = Arc::new(Mutex::new(resolve(&peers, ipv6).0));
        if !peers.is_empty() {
            let resolved = resolved.clone();
            thread::spawn(move || {
                loop {
                    let (addresses, complete) = resolve(&peers, ipv6);
                    thread::sleep(if complete {
                        RESOLVE_INTERVAL
                    } else {
                        RETRY_INTERVAL
                    });
                    *resolved.lock().expect("no panics while holding it") = addresses;
                }
            });
        }
        Ok((
            UdpLink {
                socket,
                peers: resolved,
            },
            reader,
        ))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// The configured peers' addresses, as last looked up.
    pub fn peers(&self) -> Vec<SocketAddr> {
        self.peers
            .lock()
            .expect("no panics while holding it")
            .clone()
    }

    pub fn send_to(&self, address: SocketAddr, frame: &[u8]) -> io::Result<()> {
        self.socket.send_to(frame, address).map(drop)
    }
}

impl UdpReader {
    /// Waits for a datagram. Returns its sender and contents.
    pub fn recv<'b>(&self, buf: &'b mut [u8]) -> io::Result<(SocketAddr, &'b [u8])> {
        let (len, from) = self.socket.recv_from(buf)?;
        Ok((from, &buf[..len]))
    }
}

/// Looks up each `host:port`, keeping the addresses a socket of the given
/// family can reach. Also says whether every name resolved.
fn resolve(peers: &[String], ipv6: bool) -> (Vec<SocketAddr>, bool) {
    let mut addresses = Vec::new();
    let mut complete = true;
    for peer in peers {
        match peer.to_socket_addrs() {
            Ok(found) => {
                let usable: Vec<SocketAddr> = found.filter_map(|a| for_socket(a, ipv6)).collect();
                if usable.is_empty() {
                    warn!("UDP peer {peer} has no address this socket can reach");
                    complete = false;
                }
                // One address per peer: the first, as the system prefers it.
                addresses.extend(usable.first());
            }
            Err(err) => {
                warn!("UDP peer {peer}: {err}");
                complete = false;
            }
        }
    }
    addresses.sort();
    addresses.dedup();
    if !peers.is_empty() && complete {
        info!(
            "UDP peers: {}",
            addresses
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    (addresses, complete)
}

/// `address` as a socket of the given family sends to it: IPv4 addresses
/// become IPv4-mapped IPv6 ones on an IPv6 socket (the form in which such a
/// socket reports IPv4 senders, so each peer has one address), and IPv6
/// ones are unreachable from an IPv4 socket.
fn for_socket(address: SocketAddr, ipv6: bool) -> Option<SocketAddr> {
    match (address.ip(), ipv6) {
        (IpAddr::V4(v4), true) => Some(SocketAddr::new(v4.to_ipv6_mapped().into(), address.port())),
        (IpAddr::V6(_), false) => None,
        _ => Some(address),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_are_looked_up_for_the_socket_family() {
        let (v6, complete) = resolve(&["127.0.0.1:4790".into(), "[::1]:4791".into()], true);
        assert!(complete);
        assert_eq!(
            v6,
            [
                "[::1]:4791".parse().unwrap(),
                "[::ffff:127.0.0.1]:4790".parse().unwrap()
            ]
        );
        let (v4, complete) = resolve(&["127.0.0.1:4790".into(), "[::1]:4791".into()], false);
        assert!(!complete);
        assert_eq!(v4, ["127.0.0.1:4790".parse().unwrap()]);
        let (none, complete) = resolve(&["no-such-host.invalid:4790".into()], true);
        assert!(none.is_empty() && !complete);
    }

    #[test]
    fn frames_cross_between_sockets() {
        let (a, _a_reader) = UdpLink::open("127.0.0.1:0".parse().unwrap(), vec![]).unwrap();
        let (_b, b_reader) = UdpLink::open("127.0.0.1:0".parse().unwrap(), vec![]).unwrap();
        let b_address = b_reader.socket.local_addr().unwrap();
        a.send_to(b_address, b"frame").unwrap();
        let mut buf = [0u8; 64];
        let (from, frame) = b_reader.recv(&mut buf).unwrap();
        assert_eq!(frame, b"frame");
        assert_eq!(from, a.local_addr().unwrap());
    }
}
