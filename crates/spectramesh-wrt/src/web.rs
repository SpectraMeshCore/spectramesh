//! A web page showing a live map of the mesh.
//!
//! The router thread writes a JSON snapshot of the mesh every second; a small
//! HTTP server serves it at `/api/mesh`, and the page at `/` draws it. Only
//! GET requests for those two paths are answered. The map shows the mesh's
//! layout, so the server listens on localhost unless told otherwise.

use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use log::{info, warn};
use spectramesh_core::{Instant, LinkKind, NodeId, Router};

const PAGE: &str = include_str!("web/index.html");

/// The latest JSON snapshot of the mesh, shared with the server.
pub type Snapshot = Arc<Mutex<String>>;

/// Starts the server on its own thread.
pub fn serve(address: SocketAddr, snapshot: Snapshot) -> io::Result<()> {
    let listener = TcpListener::bind(address)?;
    info!("mesh map at http://{}/", listener.local_addr()?);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let snapshot = snapshot.clone();
            thread::spawn(move || {
                if let Err(err) = answer(stream, &snapshot) {
                    warn!("web request failed: {err}");
                }
            });
        }
    });
    Ok(())
}

fn answer(stream: TcpStream, snapshot: &Snapshot) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(&stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // Read past the headers; nothing in them matters here.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }

    let mut parts = request.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (status, kind, body) = match (method, path) {
        ("GET", "/") => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        ("GET", "/api/mesh") => (
            "200 OK",
            "application/json",
            snapshot.lock().expect("no panics while holding it").clone(),
        ),
        ("GET", _) => ("404 Not Found", "text/plain", "not found\n".to_string()),
        _ => (
            "405 Method Not Allowed",
            "text/plain",
            "GET only\n".to_string(),
        ),
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body.as_bytes())
}

/// A JSON description of the mesh as this node sees it. `hue_names` names
/// each hue by its index, for the routes.
pub fn snapshot_json(router: &Router, hue_names: &[(u8, String)], now: Instant) -> String {
    let id = |node: NodeId| quoted(&node.to_string());
    let mut json = String::new();
    let _ = write!(
        json,
        "{{\"self\":{},\"name\":{},\"nodes\":[",
        id(router.id()),
        quoted(router.name())
    );
    for (i, report) in router.mesh_map().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let age = now.saturating_duration_since(report.received).as_secs();
        let _ = write!(
            json,
            "{{\"id\":{},\"name\":{},\"age_s\":{age},\"links\":[",
            id(report.node),
            quoted(&report.name)
        );
        for (j, link) in report.links.iter().enumerate() {
            if j > 0 {
                json.push(',');
            }
            let (kind, freq) = match link.kind {
                LinkKind::Ethernet => ("ethernet", 0),
                LinkKind::Wifi { freq_mhz } => ("wifi", freq_mhz),
                LinkKind::EspNow => ("esp-now", 0),
                LinkKind::Fsk { freq_mhz } => ("fsk", freq_mhz),
                LinkKind::LoRa { freq_mhz } => ("lora", freq_mhz),
                LinkKind::Ip => ("ip", 0),
            };
            let rssi = link.rssi.map_or("null".to_string(), |dbm| dbm.to_string());
            let _ = write!(
                json,
                "{{\"to\":{},\"kind\":\"{kind}\",\"freq_mhz\":{freq},\"cost\":{},\"rssi\":{rssi}}}",
                id(link.neighbor),
                link.cost
            );
        }
        json.push_str("]}");
    }
    json.push_str("],\"routes\":[");
    for (i, route) in router.routes().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let hue = hue_names
            .iter()
            .find(|(index, _)| *index == route.hue.0)
            .map_or("?", |(_, name)| name.as_str());
        let _ = write!(
            json,
            "{{\"dest\":{},\"next_hop\":{},\"hue\":{},\"cost\":{}}}",
            id(route.dest),
            id(route.next_hop),
            quoted(hue),
            route.metric
        );
    }
    json.push_str("]}");
    json
}

/// `text` as a JSON string. Node names come from other nodes, so everything
/// that could break out of the string is escaped.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // Also escaped: anything that could close a script tag or start
            // markup if the JSON were ever embedded in HTML.
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_describe_the_router() {
        use spectramesh_core::{Config, Identity, KeyRing, MeshKey};
        let identity = Identity::from_secret([1; 32]);
        let id = identity.node_id();
        let keys = KeyRing::new(&MeshKey::from_bytes([2; 32]));
        let mut router = Router::new(identity, keys, Config::default(), [3; 32]);
        router.set_name("a \"quoted\" name");
        let json = snapshot_json(&router, &[], Instant::default());
        assert_eq!(
            json,
            format!(
                "{{\"self\":\"{id}\",\"name\":\"a \\\"quoted\\\" name\",\"nodes\":[],\"routes\":[]}}"
            )
        );
    }

    #[test]
    fn hostile_names_stay_inside_their_strings() {
        assert_eq!(quoted("plain"), "\"plain\"");
        assert_eq!(
            quoted("a\"b\\c\n</script><img>"),
            "\"a\\\"b\\\\c\\u000a\\u003c/script\\u003e\\u003cimg\\u003e\""
        );
    }
}
