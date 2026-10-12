//! `spectrameshd`: the SpectraMesh routing daemon for OpenWRT and other Linux
//! systems. See `config.rs` for its options.

use std::io;
use std::process::ExitCode;
use std::sync::Arc;

use log::{error, info};
use std::path::Path;

use spectramesh_core::packet::DATA_OVERHEAD;
use spectramesh_core::{
    ChannelKey, Config as RouterConfig, HueId, HueInfo, MAX_PAYLOAD, MeshKey, Router,
    TRANSPORT_OVERHEAD,
};

use spectramesh_wrt::config::{self, Command, Config, Destination, USAGE};
use spectramesh_wrt::link::{Device, EthernetSocket, LENGTH_PREFIX_LEN};
use spectramesh_wrt::modem::Modem;
use spectramesh_wrt::tun::{IPV6_MIN_MTU, Prefix, Tun};
use spectramesh_wrt::{control, daemon, keys, logger, provision, web};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let config = match config::parse(args) {
        Ok(Command::Run(config)) => config,
        Ok(Command::GenerateMeshKey) => {
            return generate(|bytes| MeshKey::from_bytes(bytes).to_text());
        }
        Ok(Command::GenerateChannelKey) => {
            return generate(|bytes| ChannelKey::from_bytes(bytes).to_text());
        }
        Ok(Command::NodeInfo {
            identity_file,
            mesh_key_file,
            ipv6_prefix,
        }) => return node_info(&identity_file, mesh_key_file.as_deref(), ipv6_prefix),
        Ok(Command::Send {
            control,
            to,
            message,
        }) => return send(&control::path(control.as_deref()), to, &message),
        Ok(Command::Provision {
            port,
            mesh_key_file,
            name,
            channel_key_file,
        }) => {
            let settings = provision::Provision {
                mesh_key_file: mesh_key_file.as_deref(),
                name: name.as_deref(),
                channel_key_file: channel_key_file.as_deref(),
            };
            return client_result(provision::run(&port, &settings));
        }
        Ok(Command::Channels { control }) => {
            let ids = control::request(&control::path(control.as_deref()), "channels");
            return client_result(ids.map(|ids| {
                for id in ids.split_whitespace() {
                    println!("{id}");
                }
            }));
        }
        Ok(Command::Listen { control }) => {
            return client_result(control::listen(
                &control::path(control.as_deref()),
                io::stdout().lock(),
            ));
        }
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

/// Prints a new random key, as text from `to_text`.
fn generate(to_text: impl FnOnce([u8; 32]) -> String) -> ExitCode {
    match keys::random_bytes() {
        Ok(bytes) => {
            println!("{}", to_text(bytes));
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("spectrameshd: no random numbers: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Asks the running daemon to send `message`.
fn send(control: &Path, to: Destination, message: &str) -> ExitCode {
    let hex = control::hex(message.as_bytes());
    let command = match to {
        Destination::Node(node) => format!("send {node} {hex}"),
        Destination::Channel(channel) => format!("channel {channel} {hex}"),
    };
    client_result(control::request(control, &command).map(drop))
}

fn client_result(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("spectrameshd: {err}");
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
    let ids: Vec<String> = mesh_keys.ids().map(|id| id.to_string()).collect();
    let channels = match &config.channel_key_file {
        Some(file) => match keys::load_channel_keys(file) {
            Ok(channels) => channels,
            Err(err) => return err,
        },
        None => Vec::new(),
    };
    // Fresh for every run; see `Router::new`.
    let seed = match keys::random_bytes() {
        Ok(seed) => seed,
        Err(err) => return err,
    };

    let mut hues = Vec::new();
    let mut infos = Vec::new();
    for (index, spec) in config.hues.iter().enumerate() {
        let id = HueId(index as u8);
        if spec.modem {
            let (modem, reader) = match Modem::open(&spec.device) {
                Ok(modem) => modem,
                Err(err) => return io::Error::new(err.kind(), format!("{}: {err}", spec.device)),
            };
            let info = modem.info;
            info!(
                "modem {} on channel {}, {} bytes per frame",
                spec.device, info.channel, info.mtu
            );
            let bitrate = spec.bitrate_bps.unwrap_or(info.bitrate_bps.into());
            infos.push(HueInfo::new(
                id,
                spec.kind,
                spec.mtu.unwrap_or(info.mtu),
                bitrate,
            ));
            hues.push(daemon::Hue {
                id,
                device: spec.device.clone(),
                link: daemon::Link::Modem(Arc::new(modem)),
                reader: daemon::Reader::Modem(reader),
            });
            continue;
        }
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
        let mtu = spec.mtu.unwrap_or_else(|| {
            u16::try_from(device.mtu.saturating_sub(LENGTH_PREFIX_LEN)).unwrap_or(u16::MAX)
        });
        let bitrate = spec.bitrate_bps.expect("every non-modem hue has a bitrate");
        infos.push(HueInfo::new(id, spec.kind, mtu, bitrate));
        let socket = Arc::new(socket);
        hues.push(daemon::Hue {
            id,
            device: device.name,
            link: daemon::Link::Ethernet(socket.clone()),
            reader: daemon::Reader::Ethernet(socket),
        });
    }

    // Packets that fit every hue whole avoid fragmentation. Smaller hues
    // fragment, which still carries IPv6's minimum.
    let smallest_mtu = infos.iter().map(|h| usize::from(h.mtu)).min().unwrap_or(0);
    let ip_mtu = smallest_mtu
        .saturating_sub(DATA_OVERHEAD + TRANSPORT_OVERHEAD)
        .clamp(IPV6_MIN_MTU, MAX_PAYLOAD);

    let node_id = identity.node_id();
    let mut router = Router::new(identity, mesh_keys, RouterConfig::default(), seed);
    for info in infos {
        router.add_hue(info);
    }
    // Key IDs help when changing keys: every node should list the same ones.
    info!(
        "mesh key IDs: sending with {}, accepting {}",
        ids[0],
        ids.join(", ")
    );
    for channel in channels {
        info!("joined channel {}", channel.id());
        router.join_channel(channel);
    }
    if let Some(name) = &config.name {
        router.set_name(name);
    }
    // Counted beside the identity file (node.key: node.starts), as both
    // belong to this node.
    let starts = keys::count_start(&config.identity_file.with_extension("starts"));
    router.set_boot_info(starts, spectramesh_core::ResetCause::Unknown);
    let snapshot = match config.web {
        Some(address) => {
            let snapshot = web::Snapshot::default();
            if let Err(err) = web::serve(address, snapshot.clone()) {
                return io::Error::new(err.kind(), format!("serving the map on {address}: {err}"));
            }
            Some(snapshot)
        }
        None => None,
    };
    let devices: Vec<&str> = hues.iter().map(|h| h.device.as_str()).collect();
    info!(
        "SpectraMesh node {node_id} running on {}",
        devices.join(", ")
    );

    let tun = match &config.tun {
        None => None,
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

    let control = daemon::ControlSocket {
        path: control::path(config.control.as_deref()),
        configured: config.control.is_some(),
    };
    daemon::run(router, hues, tun, snapshot, config.report_interval, control)
}
