//! Runs real daemons over virtual Ethernet devices with `scripts/veth-demo.sh`.
//!
//! Ignored by default: it takes about 12 seconds and needs unprivileged user
//! namespaces. Run it with `cargo test -p spectramesh-wrt -- --ignored`.

use std::process::Command;

#[test]
#[ignore = "takes 12 s and needs unprivileged user namespaces"]
fn three_daemons_route_through_the_middle_one() {
    let output = Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/veth-demo.sh"))
        .arg("12")
        .env("SPECTRAMESHD", env!("CARGO_BIN_EXE_spectrameshd"))
        .output()
        .expect("failed to run veth-demo.sh");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "demo failed:\n{log}");

    let has = |line: &str| log.lines().any(|l| l.contains(line));
    assert!(
        has("[node 1] info:   !00000003 via !00000002 on a0, cost 2 us"),
        "{log}"
    );
    assert!(
        has("[node 3] info:   !00000001 via !00000002 on c1, cost 2 us"),
        "{log}"
    );
}
