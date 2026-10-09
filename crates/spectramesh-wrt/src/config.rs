//! Command-line configuration.
//!
//! Each `--hue` names a Linux network device and describes the link on it,
//! as the device name followed by comma-separated settings:
//!
//! | Setting   | Values                                                   | Default |
//! |-----------|----------------------------------------------------------|---------|
//! | `kind`    | `ethernet` (including fiber), `wifi` or `halow`          | `ethernet` |
//! | `bitrate` | Bits per second, optionally with a `k`, `M` or `G` suffix | `1G` for Ethernet; required for radios |
//! | `freq`    | Channel frequency in MHz                                 | 2437 for `wifi`, 915 for `halow` |
//! | `mtu`     | Largest SpectraMesh frame, in bytes                      | The device's MTU, less 2 |
//!
//! For example, `--hue eth1` or `--hue phy1-mesh0,kind=wifi,freq=5805,bitrate=100M`.

use spectramesh_core::HueKind;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
Usage: spectrameshd [OPTIONS] --mesh-key-file FILE --hue DEVICE[,SETTING=VALUE...]...
       spectrameshd --generate-mesh-key
       spectrameshd --node-info [--identity FILE] [--mesh-key-file FILE]

Routes SpectraMesh traffic across the given network devices.

Options:
  --hue SPEC              A network device to route over (repeatable). Settings:
                            kind=ethernet|wifi|halow   (default ethernet)
                            bitrate=BITS[k|M|G]        (default 1G for ethernet)
                            freq=MHZ                   (radio channel frequency)
                            mtu=BYTES                  (default: device MTU less 2)
  --mesh-key-file FILE    The mesh keys, one per line. The first is sent with;
                          any others are also accepted, while keys are changed.
  --identity FILE         This node's secret key, created on first run
                          (default /etc/spectramesh/node.key)
  --tun NAME              Carry IPv6 over the mesh through TUN device NAME,
                          with this node's address on it
  --ipv6-prefix PREFIX    The mesh's /64, such as fd12:3456:789a:: (default:
                          derived from the first mesh key)
  --report-interval SECS  How often to log neighbors and routes (default 30)
  -v, --verbose           Log debug messages
  --generate-mesh-key     Print a new random mesh key and exit
  --node-info             Print this node's ID and IPv6 address and exit
  -h, --help              Show this help

Example:
  spectrameshd --generate-mesh-key > /etc/spectramesh/mesh.key
  spectrameshd --mesh-key-file /etc/spectramesh/mesh.key --hue eth1
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Run(Config),
    GenerateMeshKey,
    NodeInfo {
        identity_file: PathBuf,
        mesh_key_file: Option<PathBuf>,
        ipv6_prefix: Option<[u8; 8]>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub identity_file: PathBuf,
    pub mesh_key_file: PathBuf,
    pub hues: Vec<HueSpec>,
    pub tun: Option<String>,
    pub ipv6_prefix: Option<[u8; 8]>,
    pub report_interval: Duration,
    pub verbose: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HueSpec {
    pub device: String,
    pub kind: HueKind,
    pub bitrate_bps: u64,
    /// `None` to use the device's MTU.
    pub mtu: Option<u16>,
}

/// Parses command-line arguments, not including the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut identity_file = PathBuf::from("/etc/spectramesh/node.key");
    let mut mesh_key_file = None;
    let mut hues = Vec::new();
    let mut report_interval = Duration::from_secs(30);
    let mut verbose = false;
    let mut generate = false;
    let mut node_info = false;
    let mut tun = None;
    let mut ipv6_prefix = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--hue" => hues.push(parse_hue(&value("--hue")?)?),
            "--mesh-key-file" => mesh_key_file = Some(value("--mesh-key-file")?.into()),
            "--identity" => identity_file = value("--identity")?.into(),
            "--generate-mesh-key" => generate = true,
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
            "-v" | "--verbose" => verbose = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if generate {
        return Ok(Command::GenerateMeshKey);
    }
    if node_info {
        return Ok(Command::NodeInfo {
            identity_file,
            mesh_key_file,
            ipv6_prefix,
        });
    }
    if hues.is_empty() {
        return Err("at least one --hue is needed".into());
    }
    let mesh_key_file = mesh_key_file.ok_or("--mesh-key-file is needed")?;
    Ok(Command::Run(Config {
        identity_file,
        mesh_key_file,
        hues,
        tun,
        ipv6_prefix,
        report_interval,
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
    for part in parts {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("expected SETTING=VALUE in hue {spec:?}, got {part:?}"))?;
        match key {
            "kind" => kind = value,
            "bitrate" => bitrate = Some(parse_bitrate(value)?),
            "freq" => freq = Some(parse_number(key, value)?),
            "mtu" => mtu = Some(parse_number(key, value)?),
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
        other => return Err(format!("unknown kind {other:?} in hue {spec:?}")),
    };
    let bitrate_bps = match (bitrate, kind) {
        (Some(bitrate), _) => bitrate,
        (None, HueKind::Ethernet) => 1_000_000_000,
        (None, _) => return Err(format!("hue {spec:?} needs a bitrate")),
    };
    Ok(HueSpec {
        device: device.into(),
        kind,
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
                    bitrate_bps: 1_000_000_000,
                    mtu: None,
                },
                HueSpec {
                    device: "phy1-mesh0".into(),
                    kind: HueKind::Wifi { freq_mhz: 5805 },
                    bitrate_bps: 100_000_000,
                    mtu: None,
                },
                HueSpec {
                    device: "wlan2".into(),
                    kind: HueKind::Wifi { freq_mhz: 915 },
                    bitrate_bps: 2_000_000,
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
    fn handles_fast_fiber() {
        let config = parse_str("--hue sfp0,bitrate=10G --mesh-key-file k").unwrap();
        assert_eq!(config.hues[0].bitrate_bps, 10_000_000_000);
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
