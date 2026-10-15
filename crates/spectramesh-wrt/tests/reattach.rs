//! Ethernet-style hues outliving their devices: two daemons joined by a veth
//! pair in a network namespace of their own, which the test deletes and
//! creates again (as `wifi up` does to Wi-Fi interfaces), then takes down
//! and up. Each time, the daemons find the device again and their route
//! comes back.
//!
//! Ignored by default: it takes about 35 seconds and needs unprivileged user
//! namespaces. Run it with `cargo test -p spectramesh-wrt -- --ignored`.

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::*;

/// A user, network and mount namespace with `/sys` showing its devices,
/// held open by a sleeping process.
struct Namespace(Child);

impl Namespace {
    fn new() -> Namespace {
        let mut child = Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--net",
                "--mount",
                "sh",
                "-eu",
                "-c",
            ])
            .arg("mount -t sysfs sysfs /sys; echo ready; exec sleep 600")
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to run unshare");
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "ready\n", "the namespace didn't start");
        Namespace(child)
    }

    /// A command to run inside the namespace.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new("nsenter");
        command
            .args([
                "--preserve-credentials",
                "--user",
                "--net",
                "--mount",
                "--target",
            ])
            .arg(self.0.id().to_string())
            .arg(program);
        command
    }

    fn sh(&self, script: &str) {
        let status = self
            .command("sh")
            .args(["-eu", "-c", script])
            .status()
            .unwrap();
        assert!(status.success(), "{script} failed");
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_in(ns: &Namespace, dir: &std::path::Path, name: &str, device: &str) -> Child {
    let log = fs::File::create(dir.join(format!("{name}.log"))).unwrap();
    ns.command(BIN)
        .args(["--hue", device, "--name", name, "--report-interval", "1"])
        .arg("--mesh-key-file")
        .arg(dir.join("mesh.key"))
        .arg("--identity")
        .arg(dir.join(format!("{name}.id")))
        .arg("--control")
        .arg(dir.join(format!("{name}.sock")))
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap()
}

const CABLE: &str =
    "ip link add sm0 type veth peer name sm1; ip link set sm0 up; ip link set sm1 up";

#[test]
#[ignore = "takes about 35 s and needs unprivileged user namespaces"]
fn hues_find_their_device_again_after_it_is_recreated_or_restarted() {
    let dir = mesh_dir("reattach");
    let (a, b) = (node_id(&dir, "a"), node_id(&dir, "b"));
    let ns = Namespace::new();
    ns.sh(CABLE);
    let _daemons = Daemons(vec![
        start_in(&ns, &dir, "a", "sm0"),
        start_in(&ns, &dir, "b", "sm1"),
    ]);
    let route = format!("{b} via {b} on sm0");
    wait_for(&dir, "a", &route);
    wait_for(&dir, "b", &format!("{a} via {a} on sm1"));

    // A new pair with the same names, as when a Wi-Fi interface is created
    // again.
    ns.sh("ip link del sm0");
    wait_for(&dir, "a", "sm0: Network is down");
    // Long enough for the route to go, so its return is the new device's.
    let lines = log(&dir, "a").lines().count();
    wait_for_after(&dir, "a", lines, " 0 routes");
    ns.sh(CABLE);
    wait_for(&dir, "a", "sm0 is back, as a new device");
    wait_for(&dir, "b", "sm1 is back, as a new device");
    let lines = log(&dir, "a").lines().count();
    wait_for_after(&dir, "a", lines, &route);

    // Down and up: the same device, whose socket works again once it's up.
    let lines = log(&dir, "a").lines().count();
    ns.sh("ip link set sm0 down");
    wait_for_after(&dir, "a", lines, "sm0: Network is down");
    ns.sh("sleep 3; ip link set sm0 up");
    wait_for(&dir, "a", "sm0 is back\n");
    wait_for(&dir, "a", "sending on sm0 works again");
    let lines = log(&dir, "a").lines().count();
    wait_for_after(&dir, "a", lines, &route);

    clean_up(&dir);
}

/// Waits for `text` in `name`'s log after its first `lines` lines.
fn wait_for_after(dir: &std::path::Path, name: &str, lines: usize, text: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    loop {
        let log = log(dir, name);
        if log.lines().skip(lines).any(|l| l.contains(text)) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{name} never logged {text:?} again:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
