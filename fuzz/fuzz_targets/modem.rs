//! Feeds arbitrary bytes through the modem link's SLIP decoder and message
//! parsers, and checks that SLIP and every message round-trip.

#![no_main]

use libfuzzer_sys::fuzz_target;
use spectramesh_core::modem::{FromModem, SlipDecoder, ToModem, slip_encode};

fuzz_target!(|data: &[u8]| {
    let mut decoder = SlipDecoder::new(2048);
    for message in data.iter().filter_map(|&byte| decoder.push(byte)) {
        if let Ok(to) = ToModem::decode(&message) {
            assert_eq!(ToModem::decode(&to.encode()), Ok(to));
        }
        if let Ok(from) = FromModem::decode(&message) {
            assert_eq!(FromModem::decode(&from.encode()), Ok(from));
        }
    }

    // Any message survives SLIP framing unchanged.
    if !data.is_empty() && data.len() <= 2048 {
        let mut framed = Vec::new();
        slip_encode(data, &mut framed);
        let mut decoder = SlipDecoder::new(2048);
        let frames: Vec<Vec<u8>> = framed.iter().filter_map(|&b| decoder.push(b)).collect();
        assert_eq!(frames, [data.to_vec()]);
    }
});
