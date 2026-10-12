//! The daemon: one thread per hue receiving frames, one reading the TUN
//! device if there is one, threads serving the control socket, and the
//! router thread.
//!
//! The reading threads block on their sockets or device and pass what they
//! read over a channel. The router thread owns the [`Router`], waits on that
//! channel until the router's next timer, and sends frames and writes packets
//! itself (it's safe to write from one thread while another reads).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use log::{debug, error, info, warn};
use spectramesh_core::app::{self, Payload};
use spectramesh_core::{Delivery, Error, HueId, Instant, MAX_PAYLOAD, NodeId, Router};

use crate::control::{self, Command, Request};
use crate::icmp;
use crate::link::{BROADCAST_MAC, EthernetSocket, Mac};
use crate::modem::{Modem, ModemReader};
use crate::tun::{Prefix, Tun, addresses};
use crate::web::{self, Snapshot};

/// A hue ready to run: its router ID, its device's name, and how to send
/// and receive on it.
pub struct Hue {
    pub id: HueId,
    pub device: String,
    pub link: Link,
    pub reader: Reader,
}

/// The sending side of a hue.
pub enum Link {
    Ethernet(Arc<EthernetSocket>),
    Modem(Arc<Modem>),
}

impl Link {
    fn send(&self, dst: Mac, frame: &[u8]) -> io::Result<()> {
        match self {
            Link::Ethernet(socket) => socket.send(dst, frame),
            Link::Modem(modem) => modem.send(dst, frame),
        }
    }
}

/// The receiving side of a hue, which runs on its own thread.
pub enum Reader {
    Ethernet(Arc<EthernetSocket>),
    Modem(ModemReader),
}

/// A hue's sending side, kept by the router thread.
struct Sender {
    id: HueId,
    device: String,
    link: Link,
}

enum Event {
    /// A frame received on a hue.
    Frame {
        hue: HueId,
        src_mac: Mac,
        frame: Vec<u8>,
        /// Signal strength in dBm, if the hue measures it.
        rssi: Option<i8>,
    },
    /// A packet the kernel routed into the TUN device.
    Packet(Vec<u8>),
    /// A command from a program, through the control socket.
    Control(Command),
}

/// Where the control socket goes, and whether the path was chosen on the
/// command line (if so, failing to open it is an error; if not, a warning).
pub struct ControlSocket {
    pub path: PathBuf,
    pub configured: bool,
}

/// Runs until every reading thread has stopped, which only happens on errors.
pub fn run(
    mut router: Router,
    hues: Vec<Hue>,
    tun: Option<(Tun, Prefix)>,
    snapshot: Option<Snapshot>,
    report_interval: Duration,
    control: ControlSocket,
) -> io::Error {
    let (inbox, events) = mpsc::channel();
    let submit = {
        let inbox = inbox.clone();
        move |command| inbox.send(Event::Control(command)).is_ok()
    };
    match control::serve(&control.path, submit) {
        Ok(()) => {}
        Err(err) if control.configured => {
            return io::Error::new(
                err.kind(),
                format!("control socket {}: {err}", control.path.display()),
            );
        }
        Err(err) => warn!(
            "no control socket: {}: {err} (choose another place with --control)",
            control.path.display()
        ),
    }
    // Programs listening for messages, through the control socket.
    let mut listeners: Vec<mpsc::Sender<String>> = Vec::new();
    let mut senders = Vec::new();
    for hue in hues {
        let (id, device, inbox) = (hue.id, hue.device.clone(), inbox.clone());
        match hue.reader {
            Reader::Ethernet(socket) => {
                thread::spawn(move || receive(id, &device, &socket, &inbox));
            }
            Reader::Modem(reader) => {
                thread::spawn(move || receive_modem(id, &device, reader, &inbox));
            }
        }
        senders.push(Sender {
            id: hue.id,
            device: hue.device,
            link: hue.link,
        });
    }
    let hues = senders;
    // Names for the map: device names, and something shorter for modems'
    // serial port paths.
    let hue_names: Vec<(u8, String)> = hues
        .iter()
        .map(|h| {
            let name = match h.link {
                Link::Modem(_) => "ESP-NOW via modem".to_string(),
                Link::Ethernet(_) => h.device.clone(),
            };
            (h.id.0, name)
        })
        .collect();
    let mut next_snapshot = Instant::default();
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
                rssi,
            }) => match router.handle_frame(hue, &frame, now()) {
                Ok(Some(sender)) => {
                    macs.insert((hue, sender), src_mac);
                    if let Some(rssi) = rssi {
                        router.record_signal(sender, hue, rssi);
                    }
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
            Ok(Event::Control(command)) => {
                handle_control(&mut router, command, &mut listeners, now());
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
            if let Err(err) = hue.link.send(dst, &tx.frame) {
                warn!("sending on {} failed: {err}", hue.device);
            }
        }
        while let Some(delivery) = router.poll_delivery() {
            let tun = tun
                .as_mut()
                .map(|(device, prefix, own)| (&mut *device, *prefix, *own));
            deliver(delivery, tun, &mut listeners);
        }

        if let Some(snapshot) = snapshot.as_ref().filter(|_| now() >= next_snapshot) {
            next_snapshot = now() + Duration::from_secs(1);
            let json = web::snapshot_json(&router, &hue_names, now());
            *snapshot.lock().expect("no panics while holding it") = json;
        }

        if now() >= next_report {
            next_report = now() + report_interval;
            report(&router, &hues);
            // Forget the addresses of neighbors the router has dropped.
            macs.retain(|&(hue, node), _| router.neighbors().get(node, hue).is_some());
        }
    }
}

/// Hands data that reached this node to the TUN device or to programs
/// listening on the control socket.
fn deliver(
    delivery: Delivery,
    tun: Option<(&mut Tun, Prefix, [u8; 16])>,
    listeners: &mut Vec<mpsc::Sender<String>>,
) {
    let src = delivery.src;
    let mut notify = |channel, data: &[u8]| {
        let line = control::message_line(src, channel, data);
        listeners.retain(|listener| listener.send(line.clone()).is_ok());
    };
    if let Some(channel) = delivery.channel {
        info!(
            "channel {channel}: {} bytes from {src}",
            delivery.payload.len()
        );
        notify(Some(channel), &delivery.payload);
        return;
    }
    if delivery.sender_keys.is_none() {
        // A broadcast to neighbors, which the mesh key protects but which
        // doesn't prove its sender.
        info!("{} bytes from neighbor {src}", delivery.payload.len());
        return;
    }
    match Payload::classify(&delivery.payload) {
        Payload::Message(data) => {
            info!("message of {} bytes from {src}", data.len());
            notify(None, data);
        }
        Payload::Ipv6(packet) => match tun {
            // Only from the node its source address names.
            Some((device, prefix, own)) => {
                let genuine = addresses(packet)
                    .is_some_and(|(s, d)| prefix.node(&s) == Some(src) && d == own);
                if !genuine {
                    debug!("dropped a packet from {src} with the wrong addresses");
                } else if let Err(err) = device.write_packet(packet) {
                    warn!("writing to {} failed: {err}", device.name);
                }
            }
            None => debug!("dropped an IPv6 packet from {src}: no TUN device"),
        },
        Payload::Unknown => debug!(
            "dropped {} bytes of an unknown type from {src}",
            delivery.payload.len()
        ),
    }
}

/// Carries out a command from the control socket and answers it.
fn handle_control(
    router: &mut Router,
    command: Command,
    listeners: &mut Vec<mpsc::Sender<String>>,
    now: Instant,
) {
    let result = match command.request {
        Request::Send { node, data } => {
            if data.len() >= MAX_PAYLOAD {
                Err(format!("at most {} bytes fit", MAX_PAYLOAD - 1))
            } else {
                router
                    .send(node, &app::message(&data), now)
                    .map(|()| String::new())
                    .map_err(|err| err.to_string())
            }
        }
        Request::Channel { channel, data } => control::find_channel(&channel, router.channels())
            .and_then(|id| {
                router
                    .send_to_channel(id, &data, now)
                    .map_err(|err| err.to_string())
            })
            .map(|()| String::new()),
        Request::Channels => Ok(router
            .channels()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(" ")),
        Request::Listen => {
            listeners.push(command.client.clone());
            Ok(String::new())
        }
    };
    let reply = match result {
        Ok(text) if text.is_empty() => "ok".to_string(),
        Ok(text) => format!("ok {text}"),
        Err(err) => format!("error {err}"),
    };
    let _ = command.client.send(reply);
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

fn receive_modem(hue: HueId, device: &str, mut reader: ModemReader, inbox: &mpsc::Sender<Event>) {
    loop {
        match reader.recv() {
            Ok((src_mac, frame, rssi)) => {
                if inbox
                    .send(Event::Frame {
                        hue,
                        src_mac,
                        frame,
                        rssi: Some(rssi),
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(err) => {
                error!("reading modem {device} failed: {err}");
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
                // Ethernet doesn't measure signal strength.
                if inbox
                    .send(Event::Frame {
                        hue,
                        src_mac,
                        frame,
                        rssi: None,
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

fn report(router: &Router, hues: &[Sender]) {
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
