//! Runs the router and connects it to the hues.
//!
//! Each hue has a receive task, which passes frames to the router through
//! [`INBOX`], and a send task, which reads frames from its own [`Outbox`].
//! The router task owns the [`Router`] and is the only code that touches it.

use alloc::boxed::Box;
use alloc::vec::Vec;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use log::{info, warn};
use spectramesh_core::{HueId, Instant, Router, Transmit};

/// A frame a hue received.
pub struct Received {
    pub hue: HueId,
    pub frame: Vec<u8>,
}

const QUEUE_LEN: usize = 16;

/// How often to log the neighbor and route tables.
const REPORT_INTERVAL: Duration = Duration::from_secs(30);

pub type Inbox = Channel<CriticalSectionRawMutex, Received, QUEUE_LEN>;
pub type Outbox = Channel<CriticalSectionRawMutex, Transmit, QUEUE_LEN>;

/// Frames received on any hue, waiting for the router.
pub static INBOX: Inbox = Channel::new();

/// The current time as the router sees it: milliseconds since boot.
pub fn now() -> Instant {
    Instant::from_millis(embassy_time::Instant::now().as_millis())
}

/// Runs the router forever. `outboxes` names the channel each hue's send task reads.
pub async fn run(mut router: Box<Router>, outboxes: &'static [(HueId, &'static Outbox)]) -> ! {
    let mut next_report = embassy_time::Instant::now() + REPORT_INTERVAL;
    loop {
        // Wake for the router's timers, a received frame, or the next report.
        let wakeup = to_embassy(router.next_wakeup()).min(next_report);
        if let Either::First(received) = select(INBOX.receive(), Timer::at(wakeup)).await
            && let Err(err) = router.handle_frame(received.hue, &received.frame, now())
        {
            warn!("dropped a frame on hue {}: {err}", received.hue.0);
        }

        router.poll(now());
        while let Some(tx) = router.poll_transmit() {
            let Some((_, outbox)) = outboxes.iter().find(|(hue, _)| *hue == tx.hue) else {
                warn!("no send task for hue {}", tx.hue.0);
                continue;
            };
            // Never block the router on a busy radio; routing recovers from a lost frame.
            if outbox.try_send(tx).is_err() {
                warn!("send queue full; dropped a frame");
            }
        }
        // TODO: hand deliveries to an application task instead of logging them.
        while let Some(delivery) = router.poll_delivery() {
            info!("{} bytes from {}", delivery.payload.len(), delivery.src);
        }

        if embassy_time::Instant::now() >= next_report {
            next_report += REPORT_INTERVAL;
            report(&router);
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
