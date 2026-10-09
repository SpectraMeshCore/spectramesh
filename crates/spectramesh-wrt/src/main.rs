//! `spectrameshd`: the SpectraMesh routing daemon for OpenWRT and other Linux
//! systems. See `config.rs` for its options.

mod config;
mod daemon;
mod keys;
mod link;
mod logger;

use std::io;
use std::process::ExitCode;
use std::sync::Arc;

use log::{error, info};
use spectramesh_core::{Config as RouterConfig, HueId, HueInfo, MeshKey, Router};

use crate::config::{Command, Config, USAGE};
use crate::link::{Device, EthernetSocket, LENGTH_PREFIX_LEN};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let config = match config::parse(args) {
        Ok(Command::Run(config)) => config,
        Ok(Command::GenerateMeshKey) => return generate_mesh_key(),
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

fn generate_mesh_key() -> ExitCode {
    match keys::random_bytes() {
        Ok(bytes) => {
            println!("{}", MeshKey::from_bytes(bytes).to_text());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("spectrameshd: no random numbers: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Opens every hue and runs the daemon. Only returns on failure.
fn run(config: Config) -> io::Error {
    let identity = match keys::load_or_create_identity(&config.identity_file) {
        Ok(identity) => identity,
        Err(err) => return err,
    };
    let mesh_keys = match keys::load_mesh_keys(&config.mesh_key_file) {
        Ok(keys) => keys,
        Err(err) => return err,
    };
    // Fresh for every run; see `Router::new`.
    let seed = match keys::random_bytes() {
        Ok(seed) => seed,
        Err(err) => return err,
    };

    let mut hues = Vec::new();
    let mut infos = Vec::new();
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
        hues.push(daemon::Hue {
            id,
            device: device.name,
            socket: Arc::new(socket),
        });
    }

    let node_id = identity.node_id();
    let mut router = Router::new(node_id, mesh_keys, RouterConfig::default(), seed);
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
