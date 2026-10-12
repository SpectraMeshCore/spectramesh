//! Command-line configuration.
//!
//! Each `--hue` names a Linux network device and describes the link on it,
//! as the device name followed by comma-separated settings:
//!
//! | Setting   | Values                                                   | Default |
//! |-----------|----------------------------------------------------------|---------|
//! | `kind`    | `ethernet` (including fiber), `wifi`, `halow`, `modem` for a radio modem on a serial port, or `udp` | `ethernet` |
//! | `bitrate` | Bits per second, optionally with a `k`, `M` or `G` suffix | `1G` for Ethernet, the modem's own for modems, `10M` for UDP; required for radios |
//! | `freq`    | Channel frequency in MHz                                 | 2437 for `wifi`, 915 for `halow` |
//! | `mtu`     | Largest SpectraMesh frame, in bytes                      | The device's MTU less 2; 1200 for UDP |
//! | `peer`    | For `udp`: a peer's `host:port`. Repeatable              | None: learn peers from the frames they send |
//!
//! For a `udp` hue, the device is the address and port to listen on.
//!
//! For example, `--hue eth1`, `--hue phy1-mesh0,kind=wifi,freq=5805,bitrate=100M`,
//! `--hue /dev/ttyACM0,kind=modem` or `--hue [::]:4790,kind=udp,peer=hub.example.org:4790`.

use spectramesh_core::HueKind;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
Usage: spectrameshd [OPTIONS] --mesh-key-file FILE --hue DEVICE[,SETTING=VALUE...]...
       spectrameshd --generate-mesh-key
       spectrameshd --generate-channel-key
       spectrameshd --node-info [--identity FILE] [--mesh-key-file FILE]
       spectrameshd [--control PATH] --send NODE MESSAGE
       spectrameshd [--control PATH] --send-channel CHANNEL MESSAGE
       spectrameshd [--control PATH] --listen
       spectrameshd [--control PATH] --channels
       spectrameshd --provision PORT [--mesh-key-file FILE] [--name NAME]
                    [--channel-key-file FILE]

Routes SpectraMesh traffic across the given network devices.

Options:
  --hue SPEC              A network device to route over (repeatable). Settings:
                            kind=ethernet|wifi|halow|modem|udp (default
                                    ethernet; modem: DEVICE is a serial port
                                    with a spectramesh-modem radio on it;
                                    udp: DEVICE is an address to listen on,
                                    such as [::]:4790)
                            bitrate=BITS[k|M|G]        (default 1G for ethernet,
                                                        the modem's own for
                                                        modems, 10M for udp)
                            freq=MHZ                   (radio channel frequency)
                            mtu=BYTES                  (default: device MTU less
                                                        2; 1200 for udp)
                            peer=HOST:PORT             (udp: a peer to send to;
                                                        repeatable)
  --mesh-key-file FILE    The mesh keys, one per line. The first is sent with;
                          any others are also accepted, while keys are changed.
  --identity FILE         This node's secret key, created on first run
                          (default /etc/spectramesh/node.key)
  --channel-key-file FILE Channel keys to join, one per line
  --name NAME             This node's name on the mesh map (up to 32 bytes)
  --web ADDRESS           Serve a live map of the mesh, such as
                          127.0.0.1:8470 (shows the mesh's layout, so keep
                          it local unless you mean to share it)
  --tun NAME              Carry IPv6 over the mesh through TUN device NAME,
                          with this node's address on it
  --ipv6-prefix PREFIX    The mesh's /64, such as fd12:3456:789a:: (default:
                          derived from the first mesh key)
  --report-interval SECS  How often to log neighbors and routes (default 30)
  --control PATH          The control socket, through which programs on this
                          host send and receive messages
                          (default /var/run/spectramesh.sock)
  -v, --verbose           Log debug messages
  --generate-mesh-key     Print a new random mesh key and exit
  --generate-channel-key  Print a new random channel key and exit
  --node-info             Print this node's ID and IPv6 address and exit
  --send NODE MESSAGE     Ask the running daemon to send MESSAGE to NODE,
                          encrypted end to end, and exit
  --send-channel CHANNEL MESSAGE
                          Ask the running daemon to send MESSAGE on a joined
                          channel (CHANNEL may be the start of its ID) and exit
  --listen                Print messages the running daemon receives
  --channels              Print the IDs of the channels the running daemon
                          has joined
  --provision PORT        Give the ESP32 node on serial port PORT the mesh
                          keys, name and channel keys given with the other
                          options, then restart it, and exit
  -h, --help              Show this help

Example:
  spectrameshd --generate-mesh-key > /etc/spectramesh/mesh.key
  spectrameshd --mesh-key-file /etc/spectramesh/mesh.key --hue eth1
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Run(Config),
    GenerateMeshKey,
    GenerateChannelKey,
    NodeInfo {
        identity_file: PathBuf,
        mesh_key_file: Option<PathBuf>,
        ipv6_prefix: Option<[u8; 8]>,
    },
    /// Asks the running daemon to send a message.
    Send {
        control: Option<PathBuf>,
        to: Destination,
        message: String,
    },
    /// Prints messages the running daemon receives.
    Listen {
        control: Option<PathBuf>,
    },
    /// Gives an ESP32 node its settings over USB.
    Provision {
        port: String,
        mesh_key_file: Option<PathBuf>,
        name: Option<String>,
        channel_key_file: Option<PathBuf>,
    },
    /// Prints the channels the running daemon has joined.
    Channels {
        control: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    Node(spectramesh_core::NodeId),
    /// The start of a channel's ID.
    Channel(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub identity_file: PathBuf,
    pub mesh_key_file: PathBuf,
    pub channel_key_file: Option<PathBuf>,
    pub name: Option<String>,
    pub web: Option<std::net::SocketAddr>,
    pub hues: Vec<HueSpec>,
    pub tun: Option<String>,
    pub ipv6_prefix: Option<[u8; 8]>,
    pub report_interval: Duration,
    /// `None` for the default.
    pub control: Option<PathBuf>,
    pub verbose: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HueSpec {
    pub device: String,
    pub kind: HueKind,
    pub link: LinkType,
    /// `None` for a modem, to use its own.
    pub bitrate_bps: Option<u64>,
    /// `None` to use the device's MTU.
    pub mtu: Option<u16>,
}

/// What a hue's device is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkType {
    /// A Linux network device that carries Ethernet frames.
    Ethernet,
    /// A serial port with a `spectramesh-modem` radio on it.
    Modem,
    /// A UDP socket bound to the device, an address and port, which sends
    /// to `peers` (each `host:port`) and to the neighbors it learns.
    Udp { peers: Vec<String> },
}

/// Parses command-line arguments, not including the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut identity_file = PathBuf::from("/etc/spectramesh/node.key");
    let mut mesh_key_file = None;
    let mut hues = Vec::new();
    let mut report_interval = Duration::from_secs(30);
    let mut verbose = false;
    let mut generate = false;
    let mut generate_channel = false;
    let mut channel_key_file = None;
    let mut name = None;
    let mut web = None;
    let mut node_info = false;
    let mut tun = None;
    let mut ipv6_prefix = None;
    let mut control = None;
    let mut send = None;
    let mut listen = false;
    let mut list_channels = false;
    let mut provision = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--hue" => hues.push(parse_hue(&value("--hue")?)?),
            "--mesh-key-file" => mesh_key_file = Some(value("--mesh-key-file")?.into()),
            "--identity" => identity_file = value("--identity")?.into(),
            "--generate-mesh-key" => generate = true,
            "--generate-channel-key" => generate_channel = true,
            "--channel-key-file" => channel_key_file = Some(value("--channel-key-file")?.into()),
            "--name" => name = Some(value("--name")?),
            "--web" => {
                let address = value("--web")?;
                web = Some(address.parse().map_err(|_| {
                    format!("invalid --web address {address:?}; try 127.0.0.1:8470")
                })?);
            }
            "--node-info" => node_info = true,
            "--tun" => tun = Some(value("--tun")?),
            "--ipv6-prefix" => ipv6_prefix = Some(parse_prefix(&value("--ipv6-prefix")?)?),
            "--report-interval" => {
                let secs = value("--report-interval")?;
                let secs: u64 = secs
                    .parse()
                    .map_err(|_| format!("invalid report interval {secs:?}"))?;
                report_interval = Duration::from_secs(secs.max(1));
            }
            "--control" => control = Some(value("--control")?.into()),
            "--send" => {
                let node = value("--send")?;
                let node = node
                    .parse()
                    .map_err(|_| format!("{node:?} isn't a node ID like !0123456789abcdef"))?;
                send = Some((Destination::Node(node), value("--send")?));
            }
            "--send-channel" => {
                let channel = value("--send-channel")?.to_ascii_lowercase();
                if channel.is_empty()
                    || channel.len() > 16
                    || !channel.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(format!("{channel:?} isn't a channel ID"));
                }
                send = Some((Destination::Channel(channel), value("--send-channel")?));
            }
            "--listen" => listen = true,
            "--channels" => list_channels = true,
            "--provision" => provision = Some(value("--provision")?),
            "-v" | "--verbose" => verbose = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if generate {
        return Ok(Command::GenerateMeshKey);
    }
    if generate_channel {
        return Ok(Command::GenerateChannelKey);
    }
    if node_info {
        return Ok(Command::NodeInfo {
            identity_file,
            mesh_key_file,
            ipv6_prefix,
        });
    }
    if let Some(port) = provision {
        return Ok(Command::Provision {
            port,
            mesh_key_file,
            name,
            channel_key_file,
        });
    }
    if let Some((to, message)) = send {
        return Ok(Command::Send {
            control,
            to,
            message,
        });
    }
    if listen {
        return Ok(Command::Listen { control });
    }
    if list_channels {
        return Ok(Command::Channels { control });
    }
    if hues.is_empty() {
        return Err("at least one --hue is needed".into());
    }
    let mesh_key_file = mesh_key_file.ok_or("--mesh-key-file is needed")?;
    Ok(Command::Run(Config {
        identity_file,
        mesh_key_file,
        channel_key_file,
        name,
        web,
        hues,
        tun,
        ipv6_prefix,
        report_interval,
        control,
        verbose,
    }))
}

/// A `/64` written as an IPv6 address, optionally followed by `/64`.
fn parse_prefix(text: &str) -> Result<[u8; 8], String> {
    let address = text.strip_suffix("/64").unwrap_or(text);
    let address: std::net::Ipv6Addr = address
        .parse()
        .map_err(|_| format!("invalid IPv6 prefix {text:?}"))?;
    let octets = address.octets();
    if octets[8..] != [0; 8] {
        return Err(format!(
            "IPv6 prefix {text:?} must be a /64, like fd12:3456:789a::"
        ));
    }
    Ok(octets[..8].try_into().expect("8 bytes"))
}

fn parse_hue(spec: &str) -> Result<HueSpec, String> {
    let mut parts = spec.split(',');
    let device = parts.next().unwrap_or_default();
    if device.is_empty() || device.contains('=') {
        return Err(format!("hue {spec:?} must start with a device name"));
    }

    let (mut kind, mut bitrate, mut freq, mut mtu) = ("ethernet", None, None, None);
    let mut peers = Vec::new();
    for part in parts {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("expected SETTING=VALUE in hue {spec:?}, got {part:?}"))?;
        match key {
            "kind" => kind = value,
            "bitrate" => bitrate = Some(parse_bitrate(value)?),
            "freq" => freq = Some(parse_number(key, value)?),
            "mtu" => mtu = Some(parse_number(key, value)?),
            "peer" => peers.push(value.to_string()),
            _ => return Err(format!("unknown setting {key:?} in hue {spec:?}")),
        }
    }

    let kind = match kind {
        "ethernet" => HueKind::Ethernet,
        "wifi" => HueKind::Wifi {
            freq_mhz: freq.unwrap_or(2437),
        },
        "halow" => HueKind::Wifi {
            freq_mhz: freq.unwrap_or(915),
        },
        // The only modem firmware so far speaks ESP-NOW.
        "modem" => HueKind::EspNow,
        "udp" => HueKind::Ip,
        other => return Err(format!("unknown kind {other:?} in hue {spec:?}")),
    };
    let link = match kind {
        HueKind::EspNow => LinkType::Modem,
        HueKind::Ip => {
            if device.parse::<std::net::SocketAddr>().is_err() {
                return Err(format!(
                    "UDP hue {spec:?} must start with an address and port to listen on, \
                     such as [::]:4790"
                ));
            }
            LinkType::Udp { peers }
        }
        _ if !peers.is_empty() => {
            return Err(format!("only UDP hues have peers, in hue {spec:?}"));
        }
        _ => LinkType::Ethernet,
    };
    let bitrate_bps = match (bitrate, kind) {
        (Some(bitrate), _) => Some(bitrate),
        (None, HueKind::Ethernet) => Some(1_000_000_000),
        (None, HueKind::Ip) => Some(10_000_000),
        (None, HueKind::EspNow) => None,
        (None, _) => return Err(format!("hue {spec:?} needs a bitrate")),
    };
    // Small enough for any internet path, with room for tunnels.
    let mtu = mtu.or((kind == HueKind::Ip).then_some(1200));
    Ok(HueSpec {
        device: device.into(),
        kind,
        link,
        bitrate_bps,
        mtu,
    })
}

fn parse_bitrate(value: &str) -> Result<u64, String> {
    let (digits, scale) = match value.char_indices().last() {
        Some((i, 'k' | 'K')) => (&value[..i], 1_000),
        Some((i, 'M')) => (&value[..i], 1_000_000),
        Some((i, 'G')) => (&value[..i], 1_000_000_000),
        _ => (value, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .filter(|&bps| bps > 0)
        .ok_or_else(|| format!("invalid bitrate {value:?}"))
}

fn parse_number<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {key} {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &str) -> Result<Config, String> {
        match parse(args.split_whitespace().map(String::from))? {
            Command::Run(config) => Ok(config),
            other => Err(format!("{other:?}")),
        }
    }

    #[test]
    fn parses_a_full_command_line() {
        let config = parse_str(
            "--hue eth1 --hue phy1-mesh0,kind=wifi,freq=5805,bitrate=100M \
             --hue wlan2,kind=halow,bitrate=2M,mtu=1000 --mesh-key-file /tmp/mesh.key \
             --identity /tmp/node.key --report-interval 5 -v",
        )
        .unwrap();
        assert_eq!(config.mesh_key_file, PathBuf::from("/tmp/mesh.key"));
        assert_eq!(config.identity_file, PathBuf::from("/tmp/node.key"));
        assert_eq!(config.report_interval, Duration::from_secs(5));
        assert!(config.verbose);
        assert_eq!(
            config.hues,
            [
                HueSpec {
                    device: "eth1".into(),
                    kind: HueKind::Ethernet,
                    link: LinkType::Ethernet,
                    bitrate_bps: Some(1_000_000_000),
                    mtu: None,
                },
                HueSpec {
                    device: "phy1-mesh0".into(),
                    kind: HueKind::Wifi { freq_mhz: 5805 },
                    link: LinkType::Ethernet,
                    bitrate_bps: Some(100_000_000),
                    mtu: None,
                },
                HueSpec {
                    device: "wlan2".into(),
                    kind: HueKind::Wifi { freq_mhz: 915 },
                    link: LinkType::Ethernet,
                    bitrate_bps: Some(2_000_000),
                    mtu: Some(1000),
                },
            ]
        );
    }

    #[test]
    fn generating_a_key_needs_nothing_else() {
        assert_eq!(
            parse(["--generate-mesh-key".to_string()]),
            Ok(Command::GenerateMeshKey)
        );
    }

    #[test]
    fn modems_take_a_serial_port() {
        let config = parse_str("--hue /dev/ttyACM0,kind=modem --mesh-key-file k").unwrap();
        let hue = &config.hues[0];
        assert_eq!(hue.device, "/dev/ttyACM0");
        assert_eq!(hue.link, LinkType::Modem);
        assert_eq!(hue.kind, HueKind::EspNow);
        assert_eq!(hue.bitrate_bps, None);
    }

    #[test]
    fn parses_ip_options() {
        let config = parse_str(
            "--hue eth0 --mesh-key-file k --tun smesh0 --ipv6-prefix fd12:3456:789a::/64",
        )
        .unwrap();
        assert_eq!(config.tun.as_deref(), Some("smesh0"));
        assert_eq!(
            config.ipv6_prefix,
            Some([0xfd, 0x12, 0x34, 0x56, 0x78, 0x9a, 0, 0])
        );
        let err = parse_str("--hue eth0 --mesh-key-file k --ipv6-prefix fd00::1").unwrap_err();
        assert!(err.contains("must be a /64"), "{err}");
    }

    #[test]
    fn parses_client_commands() {
        let args = |text: &str| -> Vec<String> { text.split(' ').map(String::from).collect() };
        assert_eq!(
            parse(args("--control /tmp/c.sock --send !023e13d36796696d hello")),
            Ok(Command::Send {
                control: Some("/tmp/c.sock".into()),
                to: Destination::Node("!023e13d36796696d".parse().unwrap()),
                message: "hello".into(),
            })
        );
        assert_eq!(
            parse(args("--send-channel 3FA4 hi")),
            Ok(Command::Send {
                control: None,
                to: Destination::Channel("3fa4".into()),
                message: "hi".into(),
            })
        );
        assert_eq!(
            parse(args("--listen")),
            Ok(Command::Listen { control: None })
        );
        assert!(
            parse(args("--send !12 hi"))
                .unwrap_err()
                .contains("isn't a node ID")
        );
        assert!(
            parse(args("--send-channel nope hi"))
                .unwrap_err()
                .contains("isn't a channel")
        );
    }

    #[test]
    fn udp_hues_listen_and_have_peers() {
        let config = parse_str(
            "--hue [::]:4790,kind=udp,peer=hub.example.org:4790,peer=192.0.2.7:4790 \
             --mesh-key-file k",
        )
        .unwrap();
        assert_eq!(
            config.hues[0],
            HueSpec {
                device: "[::]:4790".into(),
                kind: HueKind::Ip,
                link: LinkType::Udp {
                    peers: vec!["hub.example.org:4790".into(), "192.0.2.7:4790".into()]
                },
                bitrate_bps: Some(10_000_000),
                mtu: Some(1200),
            }
        );
        let err = parse_str("--hue eth0,kind=udp --mesh-key-file k").unwrap_err();
        assert!(err.contains("address and port"), "{err}");
        let err = parse_str("--hue eth0,peer=a:1 --mesh-key-file k").unwrap_err();
        assert!(err.contains("only UDP hues have peers"), "{err}");
    }

    #[test]
    fn handles_fast_fiber() {
        let config = parse_str("--hue sfp0,bitrate=10G --mesh-key-file k").unwrap();
        assert_eq!(config.hues[0].bitrate_bps, Some(10_000_000_000));
    }

    #[test]
    fn explains_mistakes() {
        for (args, error) in [
            ("", "at least one --hue is needed"),
            ("--hue", "--hue needs a value"),
            ("--hue wlan0,kind=wifi", "needs a bitrate"),
            ("--hue eth0,speed=1G", "unknown setting"),
            ("--hue eth0,bitrate=fast", "invalid bitrate"),
            ("--hue eth0,kind=lora", "unknown kind"),
            ("--hue kind=wifi", "must start with a device name"),
            ("--hue eth0", "--mesh-key-file is needed"),
            ("--hue eth0 --frobnicate", "unknown argument"),
        ] {
            let err = parse_str(args).unwrap_err();
            assert!(err.contains(error), "{args:?} gave {err:?}");
        }
    }
}
