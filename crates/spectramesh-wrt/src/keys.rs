//! The node's identity and the mesh keys, stored in files.
//!
//! Both files are secrets: the identity file proves who this node is, and a
//! mesh key admits anyone who has it to the mesh.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use log::{info, warn};
use spectramesh_core::{Identity, KeyRing, MeshKey};

/// Fills `buf` from the kernel's cryptographic random number generator.
pub fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    let mut filled = 0;
    while filled < N {
        // SAFETY: the pointer and length describe the unfilled part of `buf`.
        let got = unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), N - filled, 0) };
        if got < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        } else {
            filled += got as usize;
        }
    }
    Ok(buf)
}

/// Reads this node's identity from `path`, or creates one there on first run.
///
/// The file holds the 32-byte secret as 64 hex digits.
pub fn load_or_create_identity(path: &Path) -> io::Result<Identity> {
    let annotate =
        |err: io::Error| io::Error::new(err.kind(), format!("{}: {err}", path.display()));
    match fs::read_to_string(path) {
        Ok(text) => {
            warn_if_readable_by_others(path);
            let secret = parse_hex32(text.trim()).ok_or_else(|| {
                annotate(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected 64 hex digits",
                ))
            })?;
            Ok(Identity::from_secret(secret))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let identity = Identity::from_secret(random_bytes()?);
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(annotate)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(annotate)?;
            writeln!(file, "{}", hex(identity.secret())).map_err(annotate)?;
            info!(
                "created a new identity for node {} in {}",
                identity.node_id(),
                path.display()
            );
            Ok(identity)
        }
        Err(err) => Err(annotate(err)),
    }
}

/// Reads mesh keys from `path`, one per line. The first is the key frames
/// are sent with; any others are accepted too. Blank lines and lines starting
/// with `#` are ignored. Also returns the IPv6 prefix the first key implies.
pub fn load_mesh_keys(path: &Path) -> io::Result<(KeyRing, [u8; 8])> {
    let text = fs::read_to_string(path)
        .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", path.display())))?;
    warn_if_readable_by_others(path);
    let mut ring: Option<(KeyRing, [u8; 8])> = None;
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = MeshKey::from_text(line).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} line {}: {err}", path.display(), number + 1),
            )
        })?;
        match &mut ring {
            Some((ring, _)) => ring.accept(&key),
            None => ring = Some((KeyRing::new(&key), key.ipv6_prefix())),
        }
    }
    ring.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no mesh keys", path.display()),
        )
    })
}

fn warn_if_readable_by_others(path: &Path) {
    if fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0) {
        warn!(
            "{} holds a secret but other users can read it; run: chmod 600 {}",
            path.display(),
            path.display()
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spectramesh-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn identities_are_created_once_then_reused() {
        let dir = temp_dir("identity");
        let path = dir.join("sub/node.key");
        let first = load_or_create_identity(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let again = load_or_create_identity(&path).unwrap();
        assert_eq!(first.node_id(), again.node_id());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mesh_key_files_list_current_then_accepted_keys() {
        let dir = temp_dir("mesh");
        let path = dir.join("mesh.key");
        let (a, b) = (MeshKey::from_bytes([1; 32]), MeshKey::from_bytes([2; 32]));
        fs::write(
            &path,
            format!("# current\n{}\n\n{}\n", a.to_text(), b.to_text()),
        )
        .unwrap();
        let (ring, prefix) = load_mesh_keys(&path).unwrap();
        assert_eq!(ring.current_id(), a.id());
        assert_eq!(prefix, a.ipv6_prefix());

        fs::write(&path, "# nothing here\n").unwrap();
        assert!(
            load_mesh_keys(&path)
                .unwrap_err()
                .to_string()
                .contains("no mesh keys")
        );
        fs::write(&path, "smk1-typo\n").unwrap();
        assert!(
            load_mesh_keys(&path)
                .unwrap_err()
                .to_string()
                .contains("line 1")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0xa5; 32];
        assert_eq!(parse_hex32(&hex(&bytes)), Some(bytes));
        assert_eq!(parse_hex32("zz"), None);
    }
}
