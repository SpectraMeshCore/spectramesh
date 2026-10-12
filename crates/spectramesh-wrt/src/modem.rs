//! A radio modem on a serial port, as a hue.
//!
//! The modem is a microcontroller running `spectramesh-modem`, such as an
//! ESP32 for ESP-NOW or 802.15.4. It passes frames between its radio and the serial link
//! using the protocol in `spectramesh_core::modem`; this daemon is the mesh
//! node, with its own identity and keys.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use spectramesh_core::modem::{
    FromModem, ModemInfo, ModemRadio, PROTOCOL_VERSION, SlipDecoder, ToModem, slip_encode,
};

use spectramesh_core::HueKind;

use crate::link::Mac;

/// The largest message the modem protocol carries: a frame and its header.
const MAX_MESSAGE: usize = 2048;

/// The sending side of a modem, shared with the router thread.
pub struct Modem {
    port: Mutex<File>,
    pub info: ModemInfo,
}

/// The receiving side, for the hue's reading thread.
pub struct ModemReader {
    port: File,
    decoder: SlipDecoder,
    buf: Vec<u8>,
}

impl Modem {
    /// Opens the modem at `path` and asks for its details.
    pub fn open(path: &str) -> io::Result<(Modem, ModemReader)> {
        let port = open_serial(path)?;
        let mut reader = ModemReader {
            port: port.try_clone()?,
            decoder: SlipDecoder::new(MAX_MESSAGE),
            buf: vec![0; 4096],
        };
        let mut modem = Modem {
            port: Mutex::new(port),
            info: ModemInfo {
                version: 0,
                mtu: 0,
                bitrate_bps: 0,
                channel: 0,
                address: [0; 6],
                radio: ModemRadio::EspNow,
            },
        };

        // The modem announces itself when it starts, but it may have
        // started long ago; ask, and wait a few seconds for the answer.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut asked = None;
        while Instant::now() < deadline {
            if asked.is_none_or(|at: Instant| at.elapsed() > Duration::from_millis(500)) {
                modem.write(ToModem::InfoRequest)?;
                asked = Some(Instant::now());
            }
            for message in reader.read_messages()? {
                if let Ok(FromModem::Info(info)) = FromModem::decode(&message) {
                    // Version 1 differs only in not naming its radio, ESP-NOW.
                    if !(1..=PROTOCOL_VERSION).contains(&info.version) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "{path} speaks modem protocol {}, not 1 to {PROTOCOL_VERSION}",
                                info.version
                            ),
                        ));
                    }
                    modem.info = info;
                    return Ok((modem, reader));
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{path} didn't answer as a SpectraMesh modem; is spectramesh-modem flashed?"),
        ))
    }

    /// Sends a SpectraMesh frame to `dst` over the modem's radio.
    pub fn send(&self, dst: Mac, frame: &[u8]) -> io::Result<()> {
        self.write(ToModem::Send { dst, frame })
    }

    fn write(&self, message: ToModem<'_>) -> io::Result<()> {
        let mut framed = Vec::new();
        slip_encode(&message.encode(), &mut framed);
        let mut port = self.port.lock().expect("no panics while holding the port");
        port.write_all(&framed)
    }
}

impl Modem {
    /// The kind of hue the modem's radio makes.
    pub fn hue_kind(&self) -> io::Result<HueKind> {
        match self.info.radio {
            ModemRadio::EspNow => Ok(HueKind::EspNow),
            ModemRadio::Ieee802154 => Ok(HueKind::Ieee802154 {
                channel: self.info.channel,
            }),
            ModemRadio::Unknown(code) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the modem has a radio this version doesn't know ({code})"),
            )),
        }
    }

    /// What to call the modem's hue on the map.
    pub fn describe(&self) -> String {
        match self.info.radio {
            ModemRadio::EspNow => "ESP-NOW via modem".into(),
            ModemRadio::Ieee802154 => format!("802.15.4 channel {} via modem", self.info.channel),
            ModemRadio::Unknown(_) => "modem".into(),
        }
    }
}

impl ModemReader {
    /// Reads what's available (waiting at most a tenth of a second) and
    /// returns the complete messages.
    pub fn read_messages(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let len = match self.port.read(&mut self.buf) {
            Ok(len) => len,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => 0,
            Err(err) => return Err(err),
        };
        Ok(self.buf[..len]
            .iter()
            .filter_map(|&byte| self.decoder.push(byte))
            .collect())
    }

    /// Waits for the next frame the modem's radio received. Returns its
    /// sender's link address, the frame, and its signal strength in dBm.
    pub fn recv(&mut self) -> io::Result<(Mac, Vec<u8>, i8)> {
        loop {
            for message in self.read_messages()? {
                if let Ok(FromModem::Received { src, frame, rssi }) = FromModem::decode(&message) {
                    return Ok((src, frame.to_vec(), rssi));
                }
            }
        }
    }
}

/// Opens a serial port in raw mode, with reads that wait at most a tenth of
/// a second.
pub fn open_serial(path: &str) -> io::Result<File> {
    let port = OpenOptions::new()
        .read(true)
        .write(true)
        // Non-blocking, or opening waits for a carrier signal USB serial
        // ports may never raise. `make_raw` switches reads back to blocking.
        .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    make_raw(&port)?;
    Ok(port)
}

/// Puts a serial port in raw mode: no echo, no line editing, no character
/// translation, no waiting for a carrier, and blocking reads that return
/// after at most a tenth of a second.
fn make_raw(port: &File) -> io::Result<()> {
    let fd = port.as_raw_fd();
    // SAFETY: termios is plain data, valid when zeroed; tcgetattr fills it in.
    let mut attrs: libc::termios = unsafe { mem::zeroed() };
    // SAFETY: `fd` is open and `attrs` is a valid termios.
    if unsafe { libc::tcgetattr(fd, &mut attrs) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `attrs` is a valid termios.
    unsafe { libc::cfmakeraw(&mut attrs) };
    attrs.c_cflag |= libc::CLOCAL | libc::CREAD;
    attrs.c_cc[libc::VMIN] = 0;
    attrs.c_cc[libc::VTIME] = 1;
    // SAFETY: `fd` is open and `attrs` is a valid termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &attrs) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: plain fcntl calls on an open descriptor.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
