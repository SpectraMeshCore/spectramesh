//! The daemon: one thread per hue receiving frames, one reading the TUN
//! device if there is one, and the router thread.
//!
//! The reading threads block on their sockets or device and pass what they
//! read over a channel. The router thread owns the [`Router`], waits on that
//! channel until the router's next timer, and sends frames and writes packets
//! itself (it's safe to write from one thread while another reads).

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use log::{debug, error, info, warn};
use spectramesh_core::{Error, HueId, Instant, NodeId, Router};

use crate::icmp;
use crate::link::{BROADCAST_MAC, EthernetSocket, Mac};
use crate::tun::{Prefix, Tun, addresses};

/// A hue ready to run: its router ID, its socket and its device's name.
pub struct Hue {
    pub id: HueId,
    pub device: String,
    pub socket: Arc<EthernetSocket>,
}

enum Event {
    /// A frame received on a hue.
    Frame {
        hue: HueId,
        src_mac: Mac,
        frame: Vec<u8>,
    },
    /// A packet the kernel routed into the TUN device.
    Packet(Vec<u8>),
}

/// Runs until every reading thread has stopped, which only happens on errors.
pub fn run(
    mut router: Router,
    hues: Vec<Hue>,
    tun: Option<(Tun, Prefix)>,
    report_interval: Duration,
) -> io::Error {
    let (inbox, events) = mpsc::channel();
    for hue in &hues {
        let (id, device, socket, inbox) = (
            hue.id,
            hue.device.clone(),
            hue.socket.clone(),
            inbox.clone(),
        );
        thread::spawn(move || receive(id, &device, &socket, &inbox));
    }
    let mut tun = match tun {
        Some((tun, prefix)) => {
            let reader = match tun.try_clone() {
                Ok(reader) => reader,
                Err(err) => return err,
            };
            let inbox = inbox.clone();
            thread::spawn(move || read_tun(reader, &inbox));
            Some((tun, prefix, prefix.address(router.id()).octets()))
        }
        None => None,
    };
    drop(inbox);

    let start = std::time::Instant::now();
    let now = || Instant::from_millis(start.elapsed().as_millis() as u64);
    // Link-layer address of each neighbor, learned from frames the router
    // has authenticated, so frames for one neighbor can be unicast.
    //
    // TODO: forget addresses of neighbors the router has dropped.
    let mut macs: BTreeMap<(HueId, NodeId), Mac> = BTreeMap::new();
    let mut next_report = now() + report_interval;
    let mut last_icmp_error = Instant::default();

    loop {
        let wakeup = router.next_wakeup().min(next_report);
        let timeout = Duration::from_millis(wakeup.as_millis().saturating_sub(now().as_millis()));
        match events.recv_timeout(timeout) {
            Ok(Event::Frame {
                hue,
                src_mac,
                frame,
            }) => match router.handle_frame(hue, &frame, now()) {
                Ok(Some(sender)) => {
                    macs.insert((hue, sender), src_mac);
                }
                Ok(None) => {}
                // Outsiders and damaged frames are worth knowing about.
                Err(err @ (Error::BadTag | Error::Replay)) => {
                    warn!("dropped a frame on hue {}: {err}", hue.0);
                }
                Err(err) => debug!("dropped a frame on hue {}: {err}", hue.0),
            },
            Ok(Event::Packet(packet)) => {
                if let Some((device, prefix, own)) = &mut tun {
                    let error = send_packet(&mut router, *prefix, own, &packet, now());
                    // RFC 4443 asks for error messages to be rate-limited.
                    let allowed = now() >= last_icmp_error + ICMP_ERROR_INTERVAL;
                    if let Some(error) = error.filter(|_| allowed) {
                        last_icmp_error = now();
                        if let Err(err) = device.write_packet(&error) {
                            warn!("writing to {} failed: {err}", device.name);
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return io::Error::other("every reading thread has stopped");
            }
        }

        router.poll(now());
        while let Some(tx) = router.poll_transmit() {
            let Some(hue) = hues.iter().find(|h| h.id == tx.hue) else {
                warn!("no socket for hue {}", tx.hue.0);
                continue;
            };
            let dst = macs
                .get(&(tx.hue, tx.next_hop))
                .copied()
                .unwrap_or(BROADCAST_MAC);
            if let Err(err) = hue.socket.send(dst, &tx.frame) {
                warn!("sending on {} failed: {err}", hue.device);
            }
        }
        while let Some(delivery) = router.poll_delivery() {
            match &mut tun {
                // Only end-to-end data, from the node its source address names.
                Some((device, prefix, own)) if delivery.sender_keys.is_some() => {
                    let genuine = addresses(&delivery.payload).is_some_and(|(src, dst)| {
                        prefix.node(&src) == Some(delivery.src) && &dst == own
                    });
                    if !genuine {
                        debug!(
                            "dropped a packet from {} with the wrong addresses",
                            delivery.src
                        );
                    } else if let Err(err) = device.write_packet(&delivery.payload) {
                        warn!("writing to {} failed: {err}", device.name);
                    }
                }
                // TODO: a local interface for programs to send and receive
                // on channels.
                _ => match delivery.channel {
                    Some(channel) => info!(
                        "channel {channel}: {} bytes from {}",
                        delivery.payload.len(),
                        delivery.src
                    ),
                    None => info!("{} bytes from {}", delivery.payload.len(), delivery.src),
                },
            }
        }

        if now() >= next_report {
            next_report = now() + report_interval;
            report(&router, &hues);
        }
    }
}

/// The shortest gap between ICMPv6 errors: at most 10 a second.
const ICMP_ERROR_INTERVAL: Duration = Duration::from_millis(100);

/// Sends an IPv6 packet from this host to the node its destination names.
/// Returns an ICMPv6 error for the host if the mesh has no route there.
fn send_packet(
    router: &mut Router,
    prefix: Prefix,
    own: &[u8; 16],
    packet: &[u8],
    now: Instant,
) -> Option<Vec<u8>> {
    // Only this node's own mesh address may send into the mesh; the kernel
    // also routes things like link-local multicast here, which stay local.
    let (src, dst) = addresses(packet)?;
    let node = prefix.node(&dst)?;
    if &src != own || node == router.id() {
        return None;
    }
    match router.send(node, packet, now) {
        Ok(()) => None,
        Err(Error::NoRoute(_)) => icmp::no_route(own, packet),
        Err(err) => {
            debug!("dropped a {}-byte packet for {node}: {err}", packet.len());
            None
        }
    }
}

fn read_tun(mut tun: Tun, inbox: &mpsc::Sender<Event>) {
    let mut buf = vec![0; 65_536];
    loop {
        match tun.read_packet(&mut buf) {
            Ok(packet) => {
                if inbox.send(Event::Packet(packet.to_vec())).is_err() {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                error!("reading {} failed: {err}", tun.name);
                return;
            }
        }
    }
}

fn receive(hue: HueId, device: &str, socket: &EthernetSocket, inbox: &mpsc::Sender<Event>) {
    let mut buf = vec![0; 65_536];
    loop {
        match socket.recv(&mut buf) {
            Ok(Some((src_mac, frame))) => {
                let frame = frame.to_vec();
                if inbox
                    .send(Event::Frame {
                        hue,
                        src_mac,
                        frame,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                error!("receiving on {device} failed: {err}");
                return;
            }
        }
    }
}

fn report(router: &Router, hues: &[Hue]) {
    info!(
        "node {}: {} neighbor links, {} routes",
        router.id(),
        router.neighbors().len(),
        router.routes().count()
    );
    for route in router.routes() {
        let device = hues
            .iter()
            .find(|h| h.id == route.hue)
            .map_or("?", |h| h.device.as_str());
        info!(
            "  {} via {} on {device}, cost {} us",
            route.dest, route.next_hop, route.metric
        );
    }
}
