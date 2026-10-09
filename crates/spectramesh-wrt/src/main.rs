//! `spectrameshd`: the SpectraMesh routing daemon for OpenWRT and other Linux
//! systems. See `config.rs` for its options.

mod config;
mod daemon;
mod keys;
mod link;
mod logger;
mod tun;

use std::io;
use std::process::ExitCode;
use std::sync::Arc;

use log::{error, info};
use std::path::Path;

use spectramesh_core::packet::DATA_OVERHEAD;
use spectramesh_core::{
    Config as RouterConfig, HueId, HueInfo, MeshKey, Router, TRANSPORT_OVERHEAD,
};

use crate::config::{Command, Config, USAGE};
use crate::link::{Device, EthernetSocket, LENGTH_PREFIX_LEN};
use crate::tun::{IPV6_MIN_MTU, Prefix, Tun};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let config = match config::parse(args) {
        Ok(Command::Run(config)) => config,
        Ok(Command::GenerateMeshKey) => return generate_mesh_key(),
        Ok(Command::NodeInfo {
            identity_file,
            mesh_key_file,
            ipv6_prefix,
        }) => return node_info(&identity_file, mesh_key_file.as_deref(), ipv6_prefix),
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

/// Prints the node's ID and identity hash, and its IPv6 address if the
/// prefix is known. Creates the identity if it doesn't exist yet.
fn node_info(
    identity_file: &Path,
    mesh_key_file: Option<&Path>,
    prefix: Option<[u8; 8]>,
) -> ExitCode {
    let result = (|| -> io::Result<()> {
        let identity = keys::load_or_create_identity(identity_file)?;
        println!("node ID:       {}", identity.node_id());
        let hash: String = identity
            .public()
            .hash()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("identity hash: {hash}");
        let prefix = match (prefix, mesh_key_file) {
            (Some(prefix), _) => Some(prefix),
            (None, Some(file)) => Some(keys::load_mesh_keys(file)?.1),
            (None, None) => None,
        };
        if let Some(prefix) = prefix {
            println!(
                "IPv6 address:  {}",
                Prefix(prefix).address(identity.node_id())
            );
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("spectrameshd: {err}");
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
    let (mesh_keys, key_prefix) = match keys::load_mesh_keys(&config.mesh_key_file) {
        Ok(keys) => keys,
        Err(err) => return err,
    };
    let prefix = Prefix(config.ipv6_prefix.unwrap_or(key_prefix));
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

    // IPv6 packets must fit the smallest hue once wrapped for the mesh.
    let smallest_mtu = infos.iter().map(|h| usize::from(h.mtu)).min().unwrap_or(0);
    let ip_mtu = smallest_mtu.saturating_sub(DATA_OVERHEAD + TRANSPORT_OVERHEAD);

    let node_id = identity.node_id();
    let mut router = Router::new(identity, mesh_keys, RouterConfig::default(), seed);
    for info in infos {
        router.add_hue(info);
    }
    let devices: Vec<&str> = hues.iter().map(|h| h.device.as_str()).collect();
    info!(
        "SpectraMesh node {node_id} running on {}",
        devices.join(", ")
    );

    let tun = match &config.tun {
        None => None,
        Some(_) if ip_mtu < IPV6_MIN_MTU => {
            return io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "IPv6 needs {IPV6_MIN_MTU}-byte packets, but the smallest hue only fits {ip_mtu}"
                ),
            );
        }
        Some(name) => match Tun::create(name, prefix.address(node_id), ip_mtu) {
            Ok(tun) => {
                info!(
                    "IPv6 address {} on {} (MTU {ip_mtu})",
                    prefix.address(node_id),
                    tun.name
                );
                Some((tun, prefix))
            }
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                return io::Error::new(
                    err.kind(),
                    format!("creating TUN device {name} needs root or CAP_NET_ADMIN"),
                );
            }
            Err(err) => return io::Error::new(err.kind(), format!("TUN device {name}: {err}")),
        },
    };

    daemon::run(router, hues, tun, config.report_interval)
}
