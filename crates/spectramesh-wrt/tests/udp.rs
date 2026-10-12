//! Runs real daemons linked by UDP on this host: two spokes that only know
//! a hub's address, and the hub, which knows nobody and learns them. Needs
//! no privileges.

mod common;

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use common::*;

#[test]
fn spokes_reach_each_other_through_a_hub_that_learns_them() {
    let dir = mesh_dir("udp");
    let ids: Vec<String> = ["hub", "a", "b"]
        .iter()
        .map(|name| node_id(&dir, name))
        .collect();
    let (hub, a, b) = (&ids[0], &ids[1], &ids[2]);

    let at = |offset| port(0, offset);
    let _daemons = Daemons(vec![
        start(&dir, "hub", &[format!("{},kind=udp", at(0))]),
        start(&dir, "a", &[format!("{},kind=udp,peer={}", at(1), at(0))]),
        start(&dir, "b", &[format!("{},kind=udp,peer={}", at(2), at(0))]),
    ]);

    // The spokes route to each other through the hub, which learned them.
    wait_for(&dir, "a", &format!("{b} via {hub}"));
    wait_for(&dir, "b", &format!("{a} via {hub}"));
    wait_for(&dir, "hub", &format!("{a} via {a}"));

    // A message from a to b, end to end through the hub.
    let mut listener = Command::new(BIN)
        .arg("--control")
        .arg(dir.join("b.sock"))
        .arg("--listen")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    let sent = Command::new(BIN)
        .arg("--control")
        .arg(dir.join("a.sock"))
        .args(["--send", b, "hello over udp"])
        .output()
        .unwrap();
    assert!(sent.status.success(), "{sent:?}");
    wait_for(&dir, "b", &format!("message of 14 bytes from {a}"));
    listener.kill().unwrap();
    let heard = String::from_utf8(listener.wait_with_output().unwrap().stdout).unwrap();
    assert_eq!(heard, format!("{a} direct: hello over udp\n"));

    clean_up(&dir);
}
