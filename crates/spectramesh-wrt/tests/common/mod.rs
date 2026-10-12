//! Helpers for tests that run real daemons on this host.

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_spectrameshd");

/// Daemons that are stopped however the test ends.
pub struct Daemons(pub Vec<Child>);

impl Drop for Daemons {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn node_id(dir: &Path, name: &str) -> String {
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

/// Starts a daemon called `name` on the given hues, logging to `name.log`.
pub fn start(dir: &Path, name: &str, hues: &[String]) -> Child {
    let log = fs::File::create(dir.join(format!("{name}.log"))).unwrap();
    Command::new(BIN)
        .args(hues.iter().flat_map(|hue| ["--hue", hue]))
        .args(["--name", name, "--report-interval", "1"])
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

pub fn log(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(format!("{name}.log"))).unwrap_or_default()
}

/// Waits up to 20 seconds for `name`'s log to contain `text`.
pub fn wait_for(dir: &Path, name: &str, text: &str) {
    wait_longer_for(dir, name, text, Duration::from_secs(20));
}

/// Waits up to `limit` for `name`'s log to contain `text`.
pub fn wait_longer_for(dir: &Path, name: &str, text: &str, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !log(dir, name).contains(text) {
        assert!(
            Instant::now() < deadline,
            "{name} never logged {text:?}:\n{}",
            log(dir, name)
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// A fresh directory with a new mesh key in it.
pub fn mesh_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spectramesh-{test}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let key = Command::new(BIN)
        .arg("--generate-mesh-key")
        .output()
        .unwrap();
    fs::write(dir.join("mesh.key"), key.stdout).unwrap();
    fs::set_permissions(dir.join("mesh.key"), fs::Permissions::from_mode(0o600)).unwrap();
    dir
}

/// Removes `dir`, unless KEEP_LOGS is set.
pub fn clean_up(dir: &Path) {
    if std::env::var_os("KEEP_LOGS").is_none() {
        let _ = fs::remove_dir_all(dir);
    }
}

/// UDP ports for a test, unlikely to clash with another test or run.
pub fn port(test: u16, offset: u16) -> String {
    let base = 20_000 + test * 10_000 + (std::process::id() % 2_000) as u16 * 10;
    format!("127.0.0.1:{}", base + offset)
}
