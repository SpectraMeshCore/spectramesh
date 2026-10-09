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

use spectramesh_core::{HueKind, NodeId};
use std::time::Duration;

pub const USAGE: &str = "\
Usage: spectrameshd [OPTIONS] --hue DEVICE[,SETTING=VALUE...]...

Routes SpectraMesh traffic across the given network devices.

Options:
  --hue SPEC              A network device to route over (repeatable). Settings:
                            kind=ethernet|wifi|halow   (default ethernet)
                            bitrate=BITS[k|M|G]        (default 1G for ethernet)
                            freq=MHZ                   (radio channel frequency)
                            mtu=BYTES                  (default: device MTU less 2)
  --node-id ID            This node's ID, as 8 hex digits (default: from the
                          first device's MAC address)
  --report-interval SECS  How often to log neighbors and routes (default 30)
  -v, --verbose           Log debug messages
  -h, --help              Show this help

Example:
  spectrameshd --hue eth1 --hue phy1-mesh0,kind=wifi,freq=5805,bitrate=100M
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub node_id: Option<NodeId>,
    pub hues: Vec<HueSpec>,
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
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Config, String> {
    let mut config = Config {
        node_id: None,
        hues: Vec::new(),
        report_interval: Duration::from_secs(30),
        verbose: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--hue" => config.hues.push(parse_hue(&value("--hue")?)?),
            "--node-id" => config.node_id = Some(parse_node_id(&value("--node-id")?)?),
            "--report-interval" => {
                let secs = value("--report-interval")?;
                let secs: u64 = secs
                    .parse()
                    .map_err(|_| format!("invalid report interval {secs:?}"))?;
                config.report_interval = Duration::from_secs(secs.max(1));
            }
            "-v" | "--verbose" => config.verbose = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if config.hues.is_empty() {
        return Err("at least one --hue is needed".into());
    }
    Ok(config)
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

/// Accepts the form nodes are printed in, `!a1b2c3d4`, or the hex digits alone.
fn parse_node_id(value: &str) -> Result<NodeId, String> {
    let hex = value.strip_prefix('!').unwrap_or(value);
    match u32::from_str_radix(hex, 16) {
        Ok(id) if hex.len() == 8 && id != u32::MAX => Ok(NodeId::from_u32(id)),
        _ => Err(format!("invalid node ID {value:?}; expected 8 hex digits")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &str) -> Result<Config, String> {
        parse(args.split_whitespace().map(String::from))
    }

    #[test]
    fn parses_a_full_command_line() {
        let config = parse_str(
            "--hue eth1 --hue phy1-mesh0,kind=wifi,freq=5805,bitrate=100M \
             --hue wlan2,kind=halow,bitrate=2M,mtu=1000 --node-id !0000002a \
             --report-interval 5 -v",
        )
        .unwrap();
        assert_eq!(config.node_id, Some(NodeId::from_u32(42)));
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
    fn handles_fast_fiber() {
        let config = parse_str("--hue sfp0,bitrate=10G").unwrap();
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
            ("--hue eth0 --node-id 42", "invalid node ID"),
            ("--hue eth0 --frobnicate", "unknown argument"),
        ] {
            let err = parse_str(args).unwrap_err();
            assert!(err.contains(error), "{args:?} gave {err:?}");
        }
    }
}
