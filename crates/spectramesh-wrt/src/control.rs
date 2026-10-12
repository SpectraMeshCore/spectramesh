//! The control socket: how programs on this host use the mesh.
//!
//! The daemon listens on a Unix socket (`--control`). Programs send it one
//! command per line, and it answers each with `ok` or `error REASON`:
//!
//! | Command              | Effect                                                     |
//! |----------------------|------------------------------------------------------------|
//! | `send NODE HEX`      | Sends bytes to one node, encrypted end to end              |
//! | `channel CHANNEL HEX`| Sends bytes to everyone on a channel this node has joined; CHANNEL may be the start of its ID |
//! | `channels`           | Lists the joined channels: `ok ID ID ...`                  |
//! | `listen`             | From now on, writes a line for each message that arrives   |
//!
//! Arriving messages are written as `message FROM CHANNEL HEX`, where CHANNEL
//! is a channel ID, or `direct` for messages sent to this node alone. A
//! direct message's sender is proven by the end-to-end handshake; a channel
//! message's isn't, as any member of a channel can claim to be another.
//!
//! Anyone who can connect can send as this node and read its messages, so
//! the socket is only open to its owner (mode 0600). `socat` makes a quick
//! client: `socat - UNIX-CONNECT:/var/run/spectramesh.sock`.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use log::{debug, info, warn};
use spectramesh_core::{ChannelId, MAX_PAYLOAD, NodeId};

/// Where the socket is unless `--control` says otherwise.
pub const DEFAULT_PATH: &str = "/var/run/spectramesh.sock";

/// The longest command line accepted: a full payload in hex, and the rest.
const MAX_LINE: u64 = 2 * MAX_PAYLOAD as u64 + 128;

/// A command from a program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Send {
        node: NodeId,
        data: Vec<u8>,
    },
    /// `channel` is the start of a channel's ID, in hex.
    Channel {
        channel: String,
        data: Vec<u8>,
    },
    Channels,
    Listen,
}

/// A request and where to write lines for its client.
pub struct Command {
    pub request: Request,
    pub client: mpsc::Sender<String>,
}

/// Parses one command line.
pub fn parse(line: &str) -> Result<Request, String> {
    let mut words = line.split_whitespace();
    let request = match words.next() {
        Some("send") => {
            let node = words.next().ok_or("send needs a node ID")?;
            let node = node
                .parse()
                .map_err(|_| format!("{node:?} isn't a node ID like !0123456789abcdef"))?;
            let data = unhex(words.next().unwrap_or(""))?;
            Request::Send { node, data }
        }
        Some("channel") => {
            let channel = words.next().ok_or("channel needs a channel ID")?;
            if channel.is_empty()
                || channel.len() > 16
                || !channel.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(format!("{channel:?} isn't a channel ID"));
            }
            let data = unhex(words.next().unwrap_or(""))?;
            Request::Channel {
                channel: channel.to_ascii_lowercase(),
                data,
            }
        }
        Some("channels") => Request::Channels,
        Some("listen") => Request::Listen,
        Some(other) => return Err(format!("unknown command {other:?}")),
        None => return Err("empty command".into()),
    };
    match words.next() {
        Some(extra) => Err(format!("unexpected {extra:?} at the end")),
        None => Ok(request),
    }
}

/// The joined channel whose ID starts with `prefix`, if exactly one does.
pub fn find_channel(
    prefix: &str,
    joined: impl Iterator<Item = ChannelId>,
) -> Result<ChannelId, String> {
    let matches: Vec<ChannelId> = joined
        .filter(|id| id.to_string().starts_with(prefix))
        .collect();
    match matches[..] {
        [id] => Ok(id),
        [] => Err(format!("not a member of a channel {prefix}")),
        _ => Err(format!("{prefix} matches more than one channel")),
    }
}

/// A line telling listeners about an arriving message.
pub fn message_line(from: NodeId, channel: Option<ChannelId>, data: &[u8]) -> String {
    let channel = channel.map_or("direct".to_string(), |id| id.to_string());
    format!("message {from} {channel} {}", hex(data))
}

pub fn hex(data: &[u8]) -> String {
    let mut text = String::with_capacity(2 * data.len());
    for byte in data {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

pub fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err("hex data needs an even number of digits".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| format!("{text:?} isn't hex"))
        })
        .collect()
}

/// Opens the socket at `path` and serves it on its own thread, passing each
/// command to `submit`.
pub fn serve(
    path: &Path,
    submit: impl Fn(Command) -> bool + Send + Clone + 'static,
) -> io::Result<()> {
    remove_stale(path)?;
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    info!("control socket at {}", path.display());
    let path = path.to_path_buf();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let submit = submit.clone();
                    thread::spawn(move || {
                        if let Err(err) = answer(stream, submit) {
                            debug!("control client: {err}");
                        }
                    });
                }
                Err(err) => {
                    warn!("control socket {}: {err}", path.display());
                    return;
                }
            }
        }
    });
    Ok(())
}

/// Removes a socket file left behind by a daemon that has stopped. Refuses
/// if a daemon is still listening on it.
fn remove_stale(path: &Path) -> io::Result<()> {
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("another daemon is listening on {}", path.display()),
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => match fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
    }
}

fn answer(stream: UnixStream, submit: impl Fn(Command) -> bool) -> io::Result<()> {
    // Everything for the client, replies and messages alike, goes through one
    // channel to a writing thread, so lines never interleave.
    let (client, lines) = mpsc::channel::<String>();
    let mut writer = stream.try_clone()?;
    thread::spawn(move || {
        for mut line in lines {
            line.push('\n');
            if writer.write_all(line.as_bytes()).is_err() {
                return; // The client left; dropping `lines` tells the daemon.
            }
        }
    });

    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let len = reader.by_ref().take(MAX_LINE).read_line(&mut line)?;
        if len == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') && len as u64 == MAX_LINE {
            let _ = client.send("error line too long".into());
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match parse(line) {
            Ok(request) => {
                let command = Command {
                    request,
                    client: client.clone(),
                };
                if !submit(command) {
                    return Ok(()); // The daemon is stopping.
                }
            }
            Err(err) => {
                let _ = client.send(format!("error {err}"));
            }
        }
    }
}

/// Connects to the daemon's control socket, for the client commands.
pub fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "{}: {err}; is spectrameshd running with --control {}?",
                path.display(),
                path.display()
            ),
        )
    })
}

/// Sends one command and returns the daemon's answer, without the `ok`.
pub fn request(path: &Path, command: &str) -> io::Result<String> {
    let stream = connect(path)?;
    (&stream).write_all(format!("{command}\n").as_bytes())?;
    let mut reader = BufReader::new(&stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::other("the daemon closed the connection"));
        }
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("ok") {
            return Ok(rest.trim_start().to_string());
        }
        if let Some(reason) = line.strip_prefix("error ") {
            return Err(io::Error::other(reason.to_string()));
        }
    }
}

/// Prints each message as it arrives, as `FROM CHANNEL: TEXT`, until the
/// daemon stops.
pub fn listen(path: &Path, mut out: impl Write) -> io::Result<()> {
    let stream = connect(path)?;
    (&stream).write_all(b"listen\n")?;
    for line in BufReader::new(&stream).lines() {
        let line = line?;
        let mut words = line.split(' ');
        if words.next() != Some("message") {
            continue;
        }
        let (Some(from), Some(channel), data) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let data = unhex(data.unwrap_or("")).unwrap_or_default();
        writeln!(out, "{from} {channel}: {}", printable(&data))?;
        out.flush()?;
    }
    Ok(())
}

/// Message bytes as text, with anything that could upset a terminal escaped.
pub fn printable(data: &[u8]) -> String {
    String::from_utf8_lossy(data)
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// The control socket's path: `--control`, or the default.
pub fn path(configured: Option<&Path>) -> PathBuf {
    configured.map_or_else(|| PathBuf::from(DEFAULT_PATH), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let node: NodeId = "!023e13d36796696d".parse().unwrap();
        assert_eq!(
            parse("send !023e13d36796696d 6869"),
            Ok(Request::Send {
                node,
                data: b"hi".to_vec()
            })
        );
        assert_eq!(
            parse("channel 3FA4 00ff"),
            Ok(Request::Channel {
                channel: "3fa4".into(),
                data: vec![0, 255]
            })
        );
        assert_eq!(parse(" listen "), Ok(Request::Listen));
        assert_eq!(parse("channels"), Ok(Request::Channels));
        for bad in [
            "",
            "send",
            "send !12 00",
            "send !023e13d36796696d 0",
            "send !023e13d36796696d zz",
            "channel xyz 00",
            "channel 00112233445566778 00",
            "listen now",
            "frobnicate",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn finds_channels_by_prefix() {
        let a: ChannelId = "3fa4000000000001".parse().unwrap();
        let b: ChannelId = "3fa5000000000002".parse().unwrap();
        let joined = || [a, b].into_iter();
        assert_eq!(find_channel("3fa4", joined()), Ok(a));
        assert!(
            find_channel("3fa", joined())
                .unwrap_err()
                .contains("more than one")
        );
        assert!(
            find_channel("77", joined())
                .unwrap_err()
                .contains("not a member")
        );
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(unhex(&hex(&[0, 1, 0xab, 0xff])), Ok(vec![0, 1, 0xab, 0xff]));
        assert_eq!(unhex(""), Ok(vec![]));
        assert!(unhex("é0").is_err());
    }

    #[test]
    fn printable_escapes_terminal_controls() {
        assert_eq!(printable(b"hi\x1b[2J\n"), "hi\\u{1b}[2J\\n");
    }

    #[test]
    fn serves_requests_over_the_socket() {
        let dir = std::env::temp_dir().join(format!("spectramesh-control-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.sock");
        // A stand-in for the router thread: answers `channels`, and sends
        // listeners a message.
        serve(&path, |command: Command| {
            let reply = match command.request {
                Request::Channels => "ok 3fa4000000000001".to_string(),
                Request::Listen => {
                    let _ = command.client.send("ok".into());
                    let from: NodeId = "!0000000000000007".parse().unwrap();
                    message_line(from, None, b"hi\x07")
                }
                _ => "error unexpected".to_string(),
            };
            command.client.send(reply).is_ok()
        })
        .unwrap();
        assert_eq!(request(&path, "channels").unwrap(), "3fa4000000000001");
        assert_eq!(
            request(&path, "bogus").unwrap_err().to_string(),
            "unknown command \"bogus\""
        );

        let reader = thread::spawn({
            let path = path.clone();
            move || {
                let mut out = Vec::new();
                // Returns once the stream ends, which it doesn't here; read
                // what arrives within the test's time instead.
                let stream = connect(&path).unwrap();
                (&stream).write_all(b"listen\n").unwrap();
                let mut reader = BufReader::new(&stream);
                for _ in 0..2 {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    out.push(line);
                }
                out
            }
        });
        assert_eq!(
            reader.join().unwrap(),
            ["ok\n", "message !0000000000000007 direct 686907\n"]
        );
        // A second daemon can't take over the socket.
        assert_eq!(
            serve(&path, |_| true).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }
}
