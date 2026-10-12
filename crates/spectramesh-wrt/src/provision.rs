//! `--provision`: gives an ESP32 node its keys and name over USB.
//!
//! The `spectramesh-esp` firmware keeps its settings in flash and takes
//! commands on its USB serial console (see its `console.rs`). This sends it
//! the mesh keys, name and channel keys from this host's files, then
//! restarts it to apply them, so keys are never typed or built into
//! firmware.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use spectramesh_core::{ChannelKey, MeshKey};

use crate::keys::read_key_file;
use crate::modem;

/// What to give the node. `None` leaves that setting as it is.
pub struct Provision<'a> {
    pub mesh_key_file: Option<&'a Path>,
    pub name: Option<&'a str>,
    pub channel_key_file: Option<&'a Path>,
}

/// How long to wait for each answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(port: &str, provision: &Provision<'_>) -> io::Result<()> {
    let mut commands = Vec::new();
    if let Some(file) = provision.mesh_key_file {
        let keys = read_key_file(file, |text| MeshKey::from_text(text).map(|k| k.to_text()))?;
        commands.push(format!("mesh-keys {}", keys.join(" ")));
    }
    if let Some(name) = provision.name {
        commands.push(format!("name {name}"));
    }
    if let Some(file) = provision.channel_key_file {
        let keys = read_key_file(file, |text| {
            ChannelKey::from_text(text).map(|k| k.to_text())
        })?;
        commands.push(format!("channel-keys {}", keys.join(" ")));
    }
    if commands.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nothing to provision: give --mesh-key-file, --name or --channel-key-file",
        ));
    }

    let mut console = Console {
        port: modem::open_serial(port)
            .map_err(|err| io::Error::new(err.kind(), format!("{port}: {err}")))?,
        pending: Vec::new(),
    };
    // End anything half typed, then ask who it is.
    console.port.write_all(b"\n")?;
    let info = console.command("info").map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("{port}: {err}; is the spectramesh-esp firmware running on it?"),
        )
    })?;
    let node = info.split_whitespace().nth(1).unwrap_or("?").to_string();
    println!("{port}: node {node}");
    for command in &commands {
        console.command(command)?;
        // Say what was set, but never the keys.
        let what = command.split_whitespace().next().unwrap_or_default();
        println!("  set {what}");
    }
    console.command("restart")?;
    println!("  restarting; it rejoins the mesh in a few seconds");
    Ok(())
}

struct Console {
    port: File,
    /// Bytes read past the last complete line.
    pending: Vec<u8>,
}

impl Console {
    /// Sends a command and waits for its answer, skipping log lines.
    /// Returns the answer after `ok`.
    fn command(&mut self, command: &str) -> io::Result<String> {
        self.port.write_all(format!("{command}\n").as_bytes())?;
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        while Instant::now() < deadline {
            let Some(line) = self.read_line()? else {
                continue;
            };
            if line == "ok" || line.starts_with("ok ") {
                return Ok(line[2..].trim_start().to_string());
            }
            if let Some(reason) = line.strip_prefix("error ") {
                let what = command.split_whitespace().next().unwrap_or_default();
                return Err(io::Error::other(format!("{what}: {reason}")));
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "no answer"))
    }

    /// A complete line, or `None` if none arrived within a tenth of a second.
    fn read_line(&mut self) -> io::Result<Option<String>> {
        if let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            return Ok(Some(String::from_utf8_lossy(&line).trim().to_string()));
        }
        let mut buf = [0u8; 512];
        match self.port.read(&mut buf) {
            Ok(len) => self.pending.extend_from_slice(&buf[..len]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
        Ok(None)
    }
}
