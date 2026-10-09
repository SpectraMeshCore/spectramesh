//! `spectrameshd`: the SpectraMesh routing daemon for OpenWRT and other Linux
//! systems. See `config.rs` for its options.

mod config;
mod daemon;
mod link;
mod logger;

use std::io;
use std::process::ExitCode;
use std::sync::Arc;

use log::{error, info};
use spectramesh_core::{Config as RouterConfig, HueId, HueInfo, NodeId, Router};

use crate::config::{Config, USAGE};
use crate::link::{Device, EthernetSocket, LENGTH_PREFIX_LEN};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let config = match config::parse(args) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("spectrameshd: {err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    logger::init(config.verbose);

    let err = run(config);
    error!("{err}");
    ExitCode::FAILURE
}

/// Opens every hue and runs the daemon. Only returns on failure.
fn run(config: Config) -> io::Error {
    let mut hues = Vec::new();
    let mut infos = Vec::new();
    let mut first_mac = None;
    for (index, spec) in config.hues.iter().enumerate() {
        let device = match Device::open(&spec.device) {
            Ok(device) => device,
            Err(err) => return err,
        };
        let socket = match EthernetSocket::open(&device) {
            Ok(socket) => socket,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                return io::Error::new(
                    err.kind(),
                    format!("opening {} needs root or CAP_NET_RAW", device.name),
                );
            }
            Err(err) => return io::Error::new(err.kind(), format!("{}: {err}", device.name)),
        };
        let id = HueId(index as u8);
        let mtu = spec.mtu.unwrap_or_else(|| {
            u16::try_from(device.mtu.saturating_sub(LENGTH_PREFIX_LEN)).unwrap_or(u16::MAX)
        });
        infos.push(HueInfo::new(id, spec.kind, mtu, spec.bitrate_bps));
        first_mac.get_or_insert(device.mac);
        hues.push(daemon::Hue {
            id,
            device: device.name,
            socket: Arc::new(socket),
        });
    }

    // Like the ESP32 firmware: the last four bytes of the first device's MAC.
    let node_id = config.node_id.unwrap_or_else(|| {
        let mac = first_mac.expect("config has at least one hue");
        NodeId([mac[2], mac[3], mac[4], mac[5]])
    });
    let mut router = Router::new(node_id, RouterConfig::default());
    for info in infos {
        router.add_hue(info);
    }
    let devices: Vec<&str> = hues.iter().map(|h| h.device.as_str()).collect();
    info!(
        "SpectraMesh node {node_id} running on {}",
        devices.join(", ")
    );

    daemon::run(router, hues, config.report_interval)
}
