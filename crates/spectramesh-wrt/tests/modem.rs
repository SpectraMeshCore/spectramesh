//! A daemon with a radio modem that disappears and comes back, as one does
//! when it's unplugged or the computer sleeps. The modem is a fake, on a
//! pseudo-terminal behind a symlink, like the paths in /dev/serial/by-id.

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use common::*;
use spectramesh_core::modem::{
    FromModem, ModemInfo, ModemRadio, PROTOCOL_VERSION, SlipDecoder, ToModem, slip_encode,
};

/// A fake modem: answers info requests, and swallows frames to send.
struct FakeModem {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl FakeModem {
    /// Starts one on a new pseudo-terminal, which `link` then points at.
    fn start(link: &Path) -> FakeModem {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: openpty writes two descriptors; the null arguments are allowed.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0, "openpty failed");
        // SAFETY: both descriptors are open and owned by nobody else.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // Or daemons started later inherit it, and it never hangs up.
        // SAFETY: a plain fcntl on an open descriptor.
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let name = std::fs::read_link(format!("/proc/self/fd/{}", slave.as_raw_fd())).unwrap();
        drop(slave);
        let _ = std::fs::remove_file(link);
        std::os::unix::fs::symlink(name, link).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::spawn({
            let stop = stop.clone();
            move || {
                let mut port = File::from(master);
                let mut decoder = SlipDecoder::new(2048);
                let mut buf = [0u8; 512];
                while !stop.load(Ordering::Relaxed) {
                    let mut poll = libc::pollfd {
                        fd: port.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: a valid pollfd for an open descriptor.
                    if unsafe { libc::poll(&mut poll, 1, 50) } <= 0
                        || poll.revents & libc::POLLIN == 0
                    {
                        continue;
                    }
                    let Ok(len) = port.read(&mut buf) else {
                        continue;
                    };
                    for message in buf[..len].iter().filter_map(|&b| decoder.push(b)) {
                        if ToModem::decode(&message) == Ok(ToModem::InfoRequest) {
                            let info = FromModem::Info(ModemInfo {
                                version: PROTOCOL_VERSION,
                                mtu: 250,
                                bitrate_bps: 1_000_000,
                                channel: 1,
                                address: [2, 0, 0, 0, 0, 1],
                                radio: ModemRadio::EspNow,
                            });
                            let mut framed = Vec::new();
                            slip_encode(&info.encode(), &mut framed);
                            let _ = port.write_all(&framed);
                        }
                    }
                }
                // Dropping the port hangs it up, as unplugging would.
            }
        });
        FakeModem { stop, thread }
    }

    /// Unplugs it.
    fn unplug(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().unwrap();
    }
}

#[test]
fn modems_that_disappear_are_picked_up_again() {
    let dir = mesh_dir("modem");
    let link = dir.join("modem");
    let modem = FakeModem::start(&link);
    let mut daemons = Daemons(vec![start(
        &dir,
        "laptop",
        &[format!("{},kind=modem", link.display())],
    )]);
    wait_for(&dir, "laptop", "ESP-NOW via modem on");

    modem.unplug();
    wait_for(
        &dir,
        "laptop",
        "the modem disconnected; waiting for it to come back",
    );

    // Plugged in again: a new device, at the same path.
    let modem = FakeModem::start(&link);
    wait_for(&dir, "laptop", "is back");
    assert!(
        daemons.0[0].try_wait().unwrap().is_none(),
        "the daemon stopped:\n{}",
        log(&dir, "laptop")
    );
    modem.unplug();
    clean_up(&dir);
}
