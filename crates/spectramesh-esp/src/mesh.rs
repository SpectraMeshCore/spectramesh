//! Runs the router and connects it to the hues.
//!
//! Each hue has a receive task, which passes frames to the router through
//! [`INBOX`], and a send task, which reads frames from its own [`Outbox`].
//! The router task owns the [`Router`] and is the only code that touches it.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use esp_println::println;
use log::{debug, info, warn};
use spectramesh_core::app::{self, Payload};
use spectramesh_core::{Delivery, Error, HueId, Instant, NodeId, Router};

use crate::console::{self, Request};

/// A frame a hue received.
pub struct Received {
    pub hue: HueId,
    pub frame: Vec<u8>,
    /// The sender's link-layer address, on hues that have them.
    pub src: Option<[u8; 6]>,
    /// Signal strength in dBm, if the radio measures it.
    pub rssi: Option<i8>,
}

const QUEUE_LEN: usize = 16;

/// How often to log the neighbor and route tables.
const REPORT_INTERVAL: Duration = Duration::from_secs(30);

pub type Inbox = Channel<CriticalSectionRawMutex, Received, QUEUE_LEN>;
pub type Outbox = Channel<CriticalSectionRawMutex, Outgoing, QUEUE_LEN>;

/// A frame for a hue to send.
pub struct Outgoing {
    pub frame: Vec<u8>,
    /// The next hop's link-layer address, if the hue has them and it's
    /// known; otherwise the frame is broadcast.
    pub dst: Option<[u8; 6]>,
}

/// Frames received on any hue, waiting for the router.
pub static INBOX: Inbox = Channel::new();

/// The current time as the router sees it: milliseconds since boot.
pub fn now() -> Instant {
    Instant::from_millis(embassy_time::Instant::now().as_millis())
}

/// Runs the router forever. `outboxes` names the channel each hue's send task reads.
pub async fn run(mut router: Box<Router>, outboxes: &'static [(HueId, &'static Outbox)]) -> ! {
    let mut next_report = embassy_time::Instant::now() + REPORT_INTERVAL;
    // Link-layer address of each neighbor, learned from frames the router
    // accepted, so frames for one neighbor can be unicast.
    let mut addresses: BTreeMap<(HueId, NodeId), [u8; 6]> = BTreeMap::new();
    loop {
        // Wake for the router's timers, a received frame, a console
        // command, or the next report.
        let wakeup = to_embassy(router.next_wakeup()).min(next_report);
        let event = select3(
            INBOX.receive(),
            console::REQUESTS.receive(),
            Timer::at(wakeup),
        )
        .await;
        let received = match event {
            Either3::First(received) => Some(received),
            Either3::Second(request) => {
                answer(&mut router, request);
                None
            }
            Either3::Third(()) => None,
        };
        if let Some(received) = received {
            match router.handle_frame(received.hue, &received.frame, now()) {
                Ok(Some(sender)) => {
                    if let Some(src) = received.src {
                        addresses.insert((received.hue, sender), src);
                    }
                    // Only frames the router accepted count towards signal strength.
                    if let Some(rssi) = received.rssi {
                        router.record_signal(sender, received.hue, rssi);
                    }
                }
                Ok(None) => {}
                // Outsiders and damaged frames are worth knowing about; the
                // rest, such as challenges to new neighbors, are routine.
                Err(err @ (Error::BadTag | Error::Replay)) => {
                    warn!("dropped a frame on hue {}: {err}", received.hue.0)
                }
                Err(err) => debug!("dropped a frame on hue {}: {err}", received.hue.0),
            }
        }

        router.poll(now());
        while let Some(tx) = router.poll_transmit() {
            let Some((_, outbox)) = outboxes.iter().find(|(hue, _)| *hue == tx.hue) else {
                warn!("no send task for hue {}", tx.hue.0);
                continue;
            };
            let dst = addresses.get(&(tx.hue, tx.next_hop)).copied();
            let outgoing = Outgoing {
                frame: tx.frame,
                dst,
            };
            // Never block the router on a busy radio; routing recovers from a lost frame.
            if outbox.try_send(outgoing).is_err() {
                warn!("send queue full; dropped a frame");
            }
        }
        while let Some(delivery) = router.poll_delivery() {
            deliver(&delivery);
        }

        if embassy_time::Instant::now() >= next_report {
            next_report += REPORT_INTERVAL;
            report(&router);
            // Forget the addresses of neighbors the router has dropped.
            addresses.retain(|&(hue, node), _| router.neighbors().get(node, hue).is_some());
        }
    }
}

/// Prints messages for programs on the console, as `spectrameshd --listen`
/// does: `message FROM CHANNEL: TEXT`, with CHANNEL `direct` for messages to
/// this node alone.
//
// TODO: hand them to an application task too.
fn deliver(delivery: &Delivery) {
    let (channel, data) = match (delivery.channel, delivery.sender_keys.is_some()) {
        (Some(channel), _) => (alloc::format!("{channel}"), &delivery.payload[..]),
        (None, true) => match Payload::classify(&delivery.payload) {
            Payload::Message(data) => ("direct".into(), data),
            _ => {
                debug!(
                    "dropped {} bytes from {}",
                    delivery.payload.len(),
                    delivery.src
                );
                return;
            }
        },
        (None, false) => {
            info!(
                "{} bytes from neighbor {}",
                delivery.payload.len(),
                delivery.src
            );
            return;
        }
    };
    let text: alloc::string::String = alloc::string::String::from_utf8_lossy(data)
        .chars()
        .flat_map(char::escape_debug)
        .collect();
    println!("message {} {channel}: {text}", delivery.src);
}

/// Carries out a console command that needs the router.
fn answer(router: &mut Router, request: Request) {
    match request {
        Request::Info => {
            let status = router.status(now());
            println!(
                "ok node {} {:?}, up {} s, boot {}, last start {:?}, {} neighbor links, {} routes, \
                 heap {} of {} bytes free",
                router.id(),
                router.name(),
                status.uptime.as_secs(),
                status.boots.unwrap_or(0),
                status.reset,
                router.neighbors().len(),
                router.routes().count(),
                esp_alloc::HEAP.free(),
                esp_alloc::HEAP.free() + esp_alloc::HEAP.used(),
            );
            for hue in (0..=u8::MAX).filter_map(|id| router.hue(HueId(id))) {
                println!(
                    "  hue {}: {:?}, {} bytes per frame",
                    hue.id.0, hue.kind, hue.mtu
                );
            }
            for id in router.channels() {
                println!("  channel {id}");
            }
            for route in router.routes() {
                println!(
                    "  route to {} via {} on hue {}, cost {} us",
                    route.dest, route.next_hop, route.hue.0, route.metric
                );
            }
        }
        Request::Send { node, text } => match router.send(node, &app::message(&text), now()) {
            Ok(()) => println!("ok"),
            Err(err) => println!("error {err}"),
        },
        Request::Channel { prefix, text } => {
            let matches: alloc::vec::Vec<_> = router
                .channels()
                .filter(|id| alloc::format!("{id}").starts_with(&prefix))
                .collect();
            match matches[..] {
                [id] => match router.send_to_channel(id, &text, now()) {
                    Ok(()) => println!("ok"),
                    Err(err) => println!("error {err}"),
                },
                [] => println!("error not a member of a channel {prefix}"),
                _ => println!("error {prefix} matches more than one channel"),
            }
        }
    }
}

fn report(router: &Router) {
    info!(
        "node {}: {} neighbor links, {} routes",
        router.id(),
        router.neighbors().len(),
        router.routes().count()
    );
    for route in router.routes() {
        info!(
            "  {} via {} on hue {}, cost {} us",
            route.dest, route.next_hop, route.hue.0, route.metric
        );
    }
}

/// Converts a router time to an embassy one, capped an hour ahead so that
/// "no timers" doesn't overflow.
fn to_embassy(at: Instant) -> embassy_time::Instant {
    let cap = embassy_time::Instant::now().as_millis() + 3_600_000;
    embassy_time::Instant::from_millis(at.as_millis().min(cap))
}
