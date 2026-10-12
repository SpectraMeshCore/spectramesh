//! The serial console: settings and messages over USB.
//!
//! The console reads one command per line from the USB serial port and
//! answers each with a line starting `ok` or `error`. Log messages appear on
//! the same port, between answers. `spectrameshd --provision PORT` uses it to
//! give a board its keys, so they never need to be typed or built into the
//! firmware.
//!
//! | Command                   | Effect                                         |
//! |---------------------------|------------------------------------------------|
//! | `help`                    | Lists the commands                             |
//! | `info`                    | Node ID, name, uptime, neighbors and routes    |
//! | `mesh-keys KEY...`        | Saves the mesh keys; the first is sent with, the others are also accepted |
//! | `name [NAME]`             | Saves this node's name for the map, or clears it |
//! | `channel-keys [KEY...]`   | Saves the channels to join, or none            |
//! | `erase-settings`          | Forgets the keys, name and channels; keeps the identity |
//! | `restart`                 | Restarts, applying saved settings              |
//! | `send NODE TEXT`          | Sends TEXT to one node, encrypted end to end   |
//! | `channel CHANNEL TEXT`    | Sends TEXT on a joined channel; CHANNEL may be the start of its ID |
//!
//! Saved settings apply at the next restart. Keys are never echoed or printed.

use alloc::string::String;
use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embedded_io_async::Read;
use esp_hal::Async;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx;
use esp_println::println;
use spectramesh_core::settings::{MAX_CHANNEL_KEYS, MAX_MESH_KEYS, Settings};
use spectramesh_core::{ChannelKey, MeshKey, NodeId};

use crate::reset;
use crate::storage::Store;

/// The longest command line: a few keys of 60 characters, or a message.
const MAX_LINE: usize = 512;

const HELP: &str = "\
commands: help, info, mesh-keys KEY..., name [NAME], channel-keys [KEY...],
          erase-settings, restart, send NODE TEXT, channel CHANNEL TEXT";

/// Commands only the router can carry out.
pub enum Request {
    Info,
    Send { node: NodeId, text: Vec<u8> },
    Channel { prefix: String, text: Vec<u8> },
}

/// Requests for the router task, which answers them on the console.
pub static REQUESTS: Channel<CriticalSectionRawMutex, Request, 4> = Channel::new();

/// What the console needs to know besides the store.
pub struct Context {
    pub node: NodeId,
    /// Whether the router is running; it isn't without a mesh key.
    pub routing: bool,
}

/// Reads and carries out commands forever.
pub async fn run(mut rx: UsbSerialJtagRx<'static, Async>, mut store: Store, context: Context) -> ! {
    let mut line = Vec::with_capacity(MAX_LINE);
    let mut too_long = false;
    let mut buf = [0u8; 64];
    loop {
        // Reading from the USB serial port can't fail.
        let Ok(len) = rx.read(&mut buf).await;
        for &byte in &buf[..len] {
            match byte {
                b'\r' | b'\n' => {
                    if too_long {
                        println!("error line too long");
                    } else if let Ok(text) = core::str::from_utf8(&line) {
                        let text = text.trim();
                        if !text.is_empty() {
                            handle(text, &mut store, &context).await;
                        }
                    } else {
                        println!("error not UTF-8");
                    }
                    // The line may have held keys.
                    line.fill(0);
                    line.clear();
                    too_long = false;
                }
                _ if line.len() >= MAX_LINE => too_long = true,
                byte => line.push(byte),
            }
        }
    }
}

async fn handle(line: &str, store: &mut Store, context: &Context) {
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    match command {
        "help" => println!("ok\n{HELP}"),
        "info" if context.routing => REQUESTS.send(Request::Info).await,
        "info" => println!(
            "ok node {} has no mesh key: set one with mesh-keys, then restart",
            context.node
        ),
        "mesh-keys" => {
            let keys: Result<Vec<MeshKey>, _> =
                rest.split_whitespace().map(MeshKey::from_text).collect();
            match keys {
                Ok(keys) if keys.is_empty() => println!("error give at least one mesh key"),
                Ok(keys) if keys.len() > MAX_MESH_KEYS => {
                    println!("error at most {MAX_MESH_KEYS} mesh keys")
                }
                Ok(keys) => update(store, |settings| settings.mesh_keys = keys),
                Err(err) => println!("error {err}"),
            }
        }
        "name" => update(store, |settings| settings.name = rest.into()),
        "channel-keys" => {
            let keys: Result<Vec<ChannelKey>, _> =
                rest.split_whitespace().map(ChannelKey::from_text).collect();
            match keys {
                Ok(keys) if keys.len() > MAX_CHANNEL_KEYS => {
                    println!("error at most {MAX_CHANNEL_KEYS} channel keys")
                }
                Ok(keys) => update(store, |settings| settings.channel_keys = keys),
                Err(err) => println!("error {err}"),
            }
        }
        "erase-settings" => match store.erase_settings() {
            Ok(()) => println!("ok erased; restart to apply"),
            Err(err) => println!("error erasing flash: {err:?}"),
        },
        "restart" => {
            println!("ok restarting");
            embassy_time::Timer::after_millis(100).await;
            reset::restart();
        }
        "send" | "channel" if !context.routing => println!("error no mesh key yet"),
        "send" => {
            let (node, text) = rest.split_once(' ').unwrap_or((rest, ""));
            match node.parse() {
                Ok(node) => {
                    let text = text.as_bytes().to_vec();
                    REQUESTS.send(Request::Send { node, text }).await;
                }
                Err(_) => println!("error {node:?} isn't a node ID like !0123456789abcdef"),
            }
        }
        "channel" => {
            let (prefix, text) = rest.split_once(' ').unwrap_or((rest, ""));
            if prefix.is_empty()
                || prefix.len() > 16
                || !prefix.bytes().all(|b| b.is_ascii_hexdigit())
            {
                println!("error {prefix:?} isn't a channel ID");
            } else {
                let prefix = prefix.to_ascii_lowercase();
                let text = text.as_bytes().to_vec();
                REQUESTS.send(Request::Channel { prefix, text }).await;
            }
        }
        _ => println!("error unknown command {command:?}; try help"),
    }
}

/// Changes the stored settings with `change`, keeping the rest.
fn update(store: &mut Store, change: impl FnOnce(&mut Settings)) {
    let mut settings = store.load_settings();
    change(&mut settings);
    match store.save_settings(&settings) {
        Ok(()) => println!("ok saved; restart to apply"),
        Err(err) => println!("error writing flash: {err:?}"),
    }
}
