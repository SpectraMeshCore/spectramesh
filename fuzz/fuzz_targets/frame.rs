//! Decodes arbitrary bytes as a frame, including every TLV and fragment header.

#![no_main]

use libfuzzer_sys::fuzz_target;
use spectramesh_core::fragment::FragmentHeader;
use spectramesh_core::packet::{Body, Frame};

fuzz_target!(|data: &[u8]| {
    let Ok(frame) = Frame::decode(data) else {
        return;
    };
    match frame.body {
        Body::Control(tlvs) => tlvs.for_each(drop),
        Body::Data { .. } => {}
        Body::Fragment(body) => {
            let _ = FragmentHeader::decode(body);
        }
    }
});
