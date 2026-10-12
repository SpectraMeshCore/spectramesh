//! Runs real daemons linked by UDP on this host: two spokes that only know
//! a hub's address, and the hub, which knows nobody and learns them. Needs
//! no privileges.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_spectrameshd");

/// Daemons that are stopped however the test ends.
struct Daemons(Vec<Child>);

impl Drop for Daemons {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn node_id(dir: &Path, name: &str) -> String {
    let output = Command::new(BIN)
        .args(["--node-info", "--identity"])
        .arg(dir.join(format!("{name}.id")))
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix("node ID:"))
        .unwrap()
        .trim()
        .to_string()
}

fn start(dir: &Path, name: &str, hue: &str) -> Child {
    let log = fs::File::create(dir.join(format!("{name}.log"))).unwrap();
    Command::new(BIN)
        .args(["--hue", hue, "--name", name, "--report-interval", "1"])
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

fn log(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(format!("{name}.log"))).unwrap_or_default()
}

/// Waits up to 20 seconds for `name`'s log to contain `text`.
fn wait_for(dir: &Path, name: &str, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !log(dir, name).contains(text) {
        assert!(
            Instant::now() < deadline,
            "{name} never logged {text:?}:\n{}",
            log(dir, name)
        );
        thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn spokes_reach_each_other_through_a_hub_that_learns_them() {
    let dir = std::env::temp_dir().join(format!("spectramesh-udp-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let key = Command::new(BIN)
        .arg("--generate-mesh-key")
        .output()
        .unwrap();
    fs::write(dir.join("mesh.key"), key.stdout).unwrap();
    fs::set_permissions(dir.join("mesh.key"), fs::Permissions::from_mode(0o600)).unwrap();
    let ids: Vec<String> = ["hub", "a", "b"]
        .iter()
        .map(|name| node_id(&dir, name))
        .collect();
    let (hub, a, b) = (&ids[0], &ids[1], &ids[2]);

    // Ports unlikely to clash with another run of this test.
    let base = 20_000 + (std::process::id() % 20_000) as u16;
    let at = |offset: u16| format!("127.0.0.1:{}", base + offset);
    let _daemons = Daemons(vec![
        start(&dir, "hub", &format!("{},kind=udp", at(0))),
        start(&dir, "a", &format!("{},kind=udp,peer={}", at(1), at(0))),
        start(&dir, "b", &format!("{},kind=udp,peer={}", at(2), at(0))),
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

    if std::env::var_os("KEEP_LOGS").is_none() {
        let _ = fs::remove_dir_all(PathBuf::from(&dir));
    }
}
