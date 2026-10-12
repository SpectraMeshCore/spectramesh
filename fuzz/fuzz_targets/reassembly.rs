//! Feeds arbitrary fragments to reassembly, and checks every datagram it
//! completes is within the limits.

#![no_main]

use libfuzzer_sys::fuzz_target;
use spectramesh_core::fragment::{FragmentHeader, MAX_DATAGRAM, MAX_PENDING, Reassembly};
use spectramesh_core::{HueId, Instant, NodeId};

fuzz_target!(|data: &[u8]| {
    let mut reassembly = Reassembly::default();
    let mut now = 0u64;
    let mut rest = data;
    // Each record: sender (1), datagram (1), offset (2), total (2), time step
    // (1), chunk length (1), chunk.
    while let [sender, datagram, o1, o2, t1, t2, step, len, tail @ ..] = rest {
        let len = usize::from(*len).min(tail.len());
        let (chunk, tail) = tail.split_at(len);
        rest = tail;
        now += u64::from(*step) * 50;
        let header = FragmentHeader {
            datagram: u16::from(*datagram),
            offset: u16::from_be_bytes([*o1, *o2]),
            total: u16::from_be_bytes([*t1, *t2]),
        };
        let sender = NodeId::from_u64(u64::from(*sender % 4));
        let at = Instant::from_millis(now);
        if let Ok(Some(whole)) = reassembly.add(sender, HueId(0), header, chunk, at) {
            assert_eq!(whole.len(), usize::from(header.total));
            assert!(whole.len() <= MAX_DATAGRAM);
        }
        assert!(reassembly.len() <= MAX_PENDING);
    }
});
