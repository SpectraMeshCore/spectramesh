//! Routing across several hues, with real daemons linked by UDP on this
//! host: no privileges needed.
//!
//! ```text
//!        fast hue 1          fast hue 2
//!   a ---------------- hub ---------------- c
//!    \__________________________________________/
//!              slow hue 3 (100 kbit/s)
//! ```
//!
//! The hub has two hues, one to each side. a and c each have one of them,
//! and share a third, slow hue. Two fast hops cost less than the slow one,
//! so a reaches c through the hub, changing hue there; when the hub stops,
//! a falls back to the slow hue.

mod common;

use std::time::Duration;

use common::*;

#[test]
#[ignore = "takes about 20 s"]
fn routes_change_hue_at_each_hop_and_fall_back_to_another_hue() {
    let dir = mesh_dir("multi-hue");
    let (hub, a, c) = (node_id(&dir, "hub"), node_id(&dir, "a"), node_id(&dir, "c"));
    let at = |offset| port(1, offset);
    let udp =
        |own: u16, peer: u16, extra: &str| format!("{},kind=udp,peer={}{extra}", at(own), at(peer));

    let mut daemons = Daemons(vec![
        start(
            &dir,
            "hub",
            &[format!("{},kind=udp", at(0)), format!("{},kind=udp", at(1))],
        ),
        start(&dir, "a", &[udp(2, 0, ""), udp(4, 5, ",bitrate=100k")]),
        start(&dir, "c", &[udp(3, 1, ""), udp(5, 4, ",bitrate=100k")]),
    ]);

    // a to c: through the hub, from a's fast hue; the hub forwards on its
    // other hue. 80 µs per fast hop, against 8,000 for the slow hue.
    wait_for(
        &dir,
        "a",
        &format!("{c} via {hub} on {}, cost 160 us", at(2)),
    );
    wait_for(&dir, "hub", &format!("{c} via {c} on {}", at(1)));
    wait_for(&dir, "hub", &format!("{a} via {a} on {}", at(0)));
    wait_for(
        &dir,
        "c",
        &format!("{a} via {hub} on {}, cost 160 us", at(3)),
    );

    // Without the hub, the slow hue takes over.
    daemons.0[0].kill().unwrap();
    daemons.0[0].wait().unwrap();
    let slow = format!("{c} via {c} on {}, cost 8000 us", at(4));
    wait_longer_for(&dir, "a", &slow, Duration::from_secs(60));

    clean_up(&dir);
}
