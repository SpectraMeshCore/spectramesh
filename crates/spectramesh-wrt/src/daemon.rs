//! The daemon: one thread per hue receiving frames, and the router thread.
//!
//! Receive threads block on their sockets and pass frames over a channel. The
//! router thread owns the [`Router`], waits on that channel until the
//! router's next timer, and sends frames itself (sockets are safe to send on
//! from one thread while another receives).

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use log::{debug, error, info, warn};
use spectramesh_core::packet::control_sender;
use spectramesh_core::{HueId, Instant, NodeId, Router};

use crate::link::{BROADCAST_MAC, EthernetSocket, Mac};

/// A hue ready to run: its router ID, its socket and its device's name.
pub struct Hue {
    pub id: HueId,
    pub device: String,
    pub socket: Arc<EthernetSocket>,
}

struct Received {
    hue: HueId,
    src_mac: Mac,
    frame: Vec<u8>,
}

/// Runs until every receive thread has stopped, which only happens on errors.
pub fn run(mut router: Router, hues: Vec<Hue>, report_interval: Duration) -> io::Error {
    let (inbox, received) = mpsc::channel();
    for hue in &hues {
        let (id, device, socket, inbox) = (
            hue.id,
            hue.device.clone(),
            hue.socket.clone(),
            inbox.clone(),
        );
        thread::spawn(move || receive(id, &device, &socket, &inbox));
    }
    drop(inbox);

    let start = std::time::Instant::now();
    let now = || Instant::from_millis(start.elapsed().as_millis() as u64);
    // Link-layer address of each neighbor, learned from its control frames,
    // so frames for one neighbor can be unicast instead of broadcast.
    //
    // TODO: forget addresses of neighbors the router has dropped.
    let mut macs: BTreeMap<(HueId, NodeId), Mac> = BTreeMap::new();
    let mut next_report = now() + report_interval;

    loop {
        let wakeup = router.next_wakeup().min(next_report);
        let timeout = Duration::from_millis(wakeup.as_millis().saturating_sub(now().as_millis()));
        match received.recv_timeout(timeout) {
            Ok(frame) => {
                if let Some(sender) = control_sender(&frame.frame) {
                    macs.insert((frame.hue, sender), frame.src_mac);
                }
                if let Err(err) = router.handle_frame(frame.hue, &frame.frame, now()) {
                    debug!("dropped a frame on hue {}: {err}", frame.hue.0);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return io::Error::other("every receive thread has stopped");
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
        // TODO: hand deliveries to applications (for example, IP traffic
        // through a TUN device) instead of logging them.
        while let Some(delivery) = router.poll_delivery() {
            info!("{} bytes from {}", delivery.payload.len(), delivery.src);
        }

        if now() >= next_report {
            next_report = now() + report_interval;
            report(&router, &hues);
        }
    }
}

fn receive(hue: HueId, device: &str, socket: &EthernetSocket, inbox: &mpsc::Sender<Received>) {
    let mut buf = vec![0; 65_536];
    loop {
        match socket.recv(&mut buf) {
            Ok(Some((src_mac, frame))) => {
                let frame = frame.to_vec();
                if inbox
                    .send(Received {
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
