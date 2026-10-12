//! Drives a router with frames from a neighbor that holds the mesh key.
//!
//! Random bytes almost never carry a valid link tag, so fed straight to a
//! router they'd only test the tag check. Instead, this harness plays a mesh
//! member: it passes the router's challenge, then turns the fuzzer's input
//! into a stream of correctly tagged control, data and fragment frames, plus
//! forged frames, time passing, and the router's own sends. That reaches
//! routing, identity discovery, handshakes, reassembly, reports and channels.

#![no_main]

use libfuzzer_sys::fuzz_target;
use spectramesh_core::packet::{Body, DataHeader, Frame, FrameKind, Header, Tlv, Trailer};
use spectramesh_core::{
    ChannelKey, Config, HueId, HueInfo, HueKind, Identity, Instant, KeyRing, MeshKey, NodeId,
    Router,
};

const HUE: HueId = HueId(0);
const MESH_KEY: [u8; 32] = [7; 32];
const CHANNEL_KEY: [u8; 32] = [9; 32];

/// The harness's neighbor: a mesh member as far as the router can tell.
struct Neighbor {
    id: NodeId,
    keys: KeyRing,
    index: u32,
    counter: u32,
}

impl Neighbor {
    /// A frame from this neighbor, tagged with the mesh key.
    fn frame(&mut self, kind: FrameKind, next_hop: NodeId, body: &[u8]) -> Vec<u8> {
        self.counter += 1;
        self.frame_with_counter(kind, next_hop, body, self.counter)
    }

    fn frame_with_counter(
        &self,
        kind: FrameKind,
        next_hop: NodeId,
        body: &[u8],
        counter: u32,
    ) -> Vec<u8> {
        let mut frame = Vec::new();
        Header {
            kind,
            key_id: self.keys.current_id(),
            sender: self.id,
            next_hop,
        }
        .encode(&mut frame);
        frame.extend_from_slice(body);
        Trailer::encode_signed_part(self.index, counter, &mut frame);
        let tag = self.keys.tag(&frame);
        frame.extend_from_slice(&tag);
        frame
    }
}

fn victim() -> Router {
    let keys = KeyRing::new(&MeshKey::from_bytes(MESH_KEY));
    let mut router = Router::new(
        Identity::from_secret([1; 32]),
        keys,
        Config::default(),
        [2; 32],
    );
    router.add_hue(HueInfo::new(HUE, HueKind::EspNow, 250, 1_000_000));
    router.join_channel(ChannelKey::from_bytes(CHANNEL_KEY));
    router
}

/// Takes everything the router queued, returning the nonce of any
/// challenge it sent.
fn drain(router: &mut Router) -> Option<u64> {
    let mut nonce = None;
    while let Some(tx) = router.poll_transmit() {
        if let Ok(Frame {
            body: Body::Control(tlvs),
            ..
        }) = Frame::decode(&tx.frame)
        {
            for tlv in tlvs.flatten() {
                if let Tlv::ChallengeRequest { nonce: n } = tlv {
                    nonce = Some(n);
                }
            }
        }
    }
    while router.poll_delivery().is_some() {}
    nonce
}

fuzz_target!(|data: &[u8]| {
    let mut router = victim();
    let me = router.id();
    let mut neighbor = Neighbor {
        id: Identity::from_secret([3; 32]).node_id(),
        keys: KeyRing::new(&MeshKey::from_bytes(MESH_KEY)),
        index: 0x5eed,
        counter: 0,
    };
    let other = NodeId::from_u64(0x0123_4567_89ab_cdef);
    let mut now = 0u64;

    // Become a verified neighbor: say anything, then answer the challenge.
    let hello = neighbor.frame(FrameKind::Control, NodeId::BROADCAST, &[]);
    let _ = router.handle_frame(HUE, &hello, Instant::from_millis(now));
    let nonce = drain(&mut router).expect("the router challenges a new neighbor");
    let mut reply = Vec::new();
    Tlv::ChallengeReply { nonce }.encode(&mut reply);
    let reply = neighbor.frame(FrameKind::Control, me, &reply);
    assert!(
        router
            .handle_frame(HUE, &reply, Instant::from_millis(now))
            .is_ok(),
        "the harness should pass the challenge"
    );
    drain(&mut router);

    // Each record: an operation (1), a parameter (1), a length (1), then that many bytes.
    let mut rest = data;
    while let [op, param, len, tail @ ..] = rest {
        let len = usize::from(*len).min(tail.len());
        let (bytes, tail) = tail.split_at(len);
        rest = tail;
        let at = Instant::from_millis(now);
        let next_hop = if param & 1 == 0 {
            me
        } else {
            NodeId::BROADCAST
        };

        match op % 8 {
            // Routing messages: the bytes are raw TLVs.
            0 => {
                let frame = neighbor.frame(FrameKind::Control, next_hop, bytes);
                let _ = router.handle_frame(HUE, &frame, at);
            }
            // Data for this node, for broadcast, or to forward; the bytes are the payload.
            1 => {
                let origin = [neighbor.id, other, me][usize::from(param >> 1) % 3];
                let dst = [me, NodeId::BROADCAST, other][usize::from(param >> 3) % 3];
                let mut body = Vec::new();
                DataHeader {
                    origin,
                    dst,
                    ttl: param >> 5,
                }
                .encode(&mut body);
                body.extend_from_slice(bytes);
                let frame = neighbor.frame(FrameKind::Data, next_hop, &body);
                let _ = router.handle_frame(HUE, &frame, at);
            }
            // A fragment: the bytes are its header and chunk.
            2 => {
                let frame = neighbor.frame(FrameKind::Fragment, next_hop, bytes);
                let _ = router.handle_frame(HUE, &frame, at);
            }
            // Raw bytes, as from an outsider or a damaged frame.
            3 => {
                let _ = router.handle_frame(HUE, bytes, at);
            }
            // A replay, or an out-of-order frame, from the neighbor.
            4 => {
                let counter = neighbor.counter.saturating_sub(u32::from(*param));
                let frame =
                    neighbor.frame_with_counter(FrameKind::Control, next_hop, bytes, counter);
                let _ = router.handle_frame(HUE, &frame, at);
            }
            // Time passes.
            5 => {
                now += u64::from(*param) * 250;
                router.poll(Instant::from_millis(now));
            }
            // The router sends something itself.
            6 => {
                let dst = [neighbor.id, other, NodeId::BROADCAST][usize::from(*param) % 3];
                let _ = router.send(dst, bytes, at);
            }
            _ => {
                let channel = ChannelKey::from_bytes(CHANNEL_KEY).id();
                let _ = router.send_to_channel(channel, bytes, at);
                router.record_signal(neighbor.id, HUE, *param as i8);
            }
        }
        drain(&mut router);
    }
    router.poll(Instant::from_millis(now + 1));
    drain(&mut router);
    // Building the map and route list must work whatever happened.
    let _ = router.mesh_map().count() + router.routes().count();
});
