//! Runs real daemons over virtual Ethernet devices with `scripts/veth-demo.sh`.
//!
//! Ignored by default: it takes about 12 seconds and needs unprivileged user
//! namespaces. Run it with `cargo test -p spectramesh-wrt -- --ignored`.

use std::collections::HashMap;
use std::process::Command;

#[test]
#[ignore = "takes 12 s and needs unprivileged user namespaces"]
fn daemons_route_through_each_other_and_keep_outsiders_out() {
    let output = Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/veth-demo.sh"))
        .arg("12")
        .env("SPECTRAMESHD", env!("CARGO_BIN_EXE_spectrameshd"))
        .output()
        .expect("failed to run veth-demo.sh");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "demo failed:\n{log}");

    // Node IDs come from each node's new keys, so read them from the log.
    let ids: HashMap<&str, &str> = log
        .lines()
        .filter_map(|line| {
            let (name, rest) = line
                .strip_prefix('[')?
                .split_once("] info: SpectraMesh node ")?;
            Some((name, rest.split_whitespace().next()?))
        })
        .collect();
    let id = |name: &str| {
        *ids.get(name)
            .unwrap_or_else(|| panic!("no ID for {name}:\n{log}"))
    };
    let has = |line: &str| log.lines().any(|l| l.contains(line));

    let via_2 = |from: &str, to: &str, dev: &str| {
        format!(
            "[{from}] info:   {} via {} on {dev}, cost 2 us",
            id(to),
            id("node 2")
        )
    };
    assert!(has(&via_2("node 1", "node 3", "a0")), "{log}");
    assert!(has(&via_2("node 3", "node 1", "c1")), "{log}");

    // The outsider is dropped by node 2 and learns nothing.
    assert!(
        has("[node 2] warning: dropped a frame on hue 2: frame tag doesn't verify"),
        "{log}"
    );
    assert!(
        has("[node 4] info: node")
            && has(&format!(
                "[node 4] info: node {}: 0 neighbor links, 0 routes",
                id("node 4")
            )),
        "{log}"
    );
    assert!(!has(&format!("{} via", id("node 4"))), "{log}");
}
