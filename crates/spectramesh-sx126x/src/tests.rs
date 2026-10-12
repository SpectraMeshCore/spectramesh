//! Tests against a recording SPI bus: each test checks the commands the
//! driver sends, byte for byte where the datasheet fixes them.

extern crate std;

use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

use embassy_futures::block_on;
use embedded_hal::digital::{ErrorType as PinErrorType, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::{ErrorType, Operation, SpiDevice};

use super::*;

/// Records what's written in each transaction, and answers reads from a script.
#[derive(Default)]
struct Bus {
    written: Vec<Vec<u8>>,
    answers: VecDeque<Vec<u8>>,
}

impl ErrorType for Bus {
    type Error = core::convert::Infallible;
}

impl SpiDevice for Bus {
    async fn transaction(
        &mut self,
        operations: &mut [Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        let mut written = Vec::new();
        let mut status_read = false;
        for operation in operations {
            match operation {
                Operation::Write(data) => written.extend_from_slice(data),
                // Every answer follows a status byte, which reads as 0.
                Operation::Read(out) if !status_read => {
                    out.fill(0);
                    status_read = true;
                }
                Operation::Read(out) => {
                    let answer = self.answers.pop_front().unwrap_or_default();
                    out.copy_from_slice(&answer[..out.len()]);
                }
                _ => unreachable!("the driver only writes and reads"),
            }
        }
        self.written.push(written);
        Ok(())
    }
}

struct Pin;

impl PinErrorType for Pin {
    type Error = core::convert::Infallible;
}

impl OutputPin for Pin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Wait for Pin {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct Delay;

impl DelayNs for Delay {
    async fn delay_ns(&mut self, _ns: u32) {}
}

const BOARD: Board = Board {
    chip: Chip::Sx1262,
    tcxo: Some(TcxoVoltage::V1_8),
    dio2_rf_switch: true,
    dc_dc: true,
};

fn new_radio(modulation: Modulation, answers: Vec<Vec<u8>>) -> Sx126x<Bus, Pin, Pin, Pin, Delay> {
    let bus = Bus {
        written: Vec::new(),
        answers: answers.into(),
    };
    let config = Config {
        board: BOARD,
        frequency_hz: 915_000_000,
        power_dbm: 30,
        modulation,
    };
    Sx126x::new(bus, Pin, Pin, Pin, Delay, config)
}

fn sent(radio: &Sx126x<Bus, Pin, Pin, Pin, Delay>, opcode: u8) -> Vec<Vec<u8>> {
    radio
        .spi
        .written
        .iter()
        .filter(|t| t.first() == Some(&opcode))
        .cloned()
        .collect()
}

#[test]
fn init_sets_up_lora_at_915_mhz() {
    // Register reads during init: TX clamp, TX modulation, IQ polarity.
    let mut radio = new_radio(
        Modulation::LONG_RANGE_LORA,
        vec![vec![0xC0], vec![0x00], vec![0x0D]],
    );
    block_on(radio.init()).unwrap();

    assert_eq!(
        sent(&radio, op::SET_DIO3_AS_TCXO_CTRL),
        [vec![0x97, 2, 0x00, 0x01, 0x40]]
    );
    assert_eq!(sent(&radio, op::SET_PACKET_TYPE), [vec![0x8A, 1]]);
    assert_eq!(
        sent(&radio, op::SET_RF_FREQUENCY),
        [vec![0x86, 0x39, 0x30, 0x00, 0x00]]
    );
    assert_eq!(sent(&radio, op::CALIBRATE_IMAGE), [vec![0x98, 0xE1, 0xE9]]);
    assert_eq!(
        sent(&radio, op::SET_PA_CONFIG),
        [vec![0x95, 0x04, 0x07, 0x00, 0x01]]
    );
    // 30 dBm asked for; the SX1262's 22 is the most it gets.
    assert_eq!(sent(&radio, op::SET_TX_PARAMS), [vec![0x8E, 22, 0x04]]);
    // SF9, 125 kHz, 4/5, no low data rate optimization.
    assert_eq!(
        sent(&radio, op::SET_MODULATION_PARAMS),
        [vec![0x8B, 9, 0x04, 1, 0]]
    );
    // 16-symbol preamble, explicit header, 255 bytes, CRC, standard IQ.
    assert_eq!(
        sent(&radio, op::SET_PACKET_PARAMS),
        [vec![0x8C, 0, 16, 0, 255, 1, 0]]
    );
    assert_eq!(
        sent(&radio, op::SET_DIO2_AS_RF_SWITCH_CTRL),
        [vec![0x9D, 1]]
    );
    assert_eq!(sent(&radio, op::SET_REGULATOR_MODE), [vec![0x96, 1]]);

    let registers = sent(&radio, op::WRITE_REGISTER);
    let wrote = |address: u16, value: &[u8]| {
        let [a0, a1] = address.to_be_bytes();
        let mut expected = vec![op::WRITE_REGISTER, a0, a1];
        expected.extend_from_slice(value);
        registers.contains(&expected)
    };
    assert!(wrote(reg::LORA_SYNC_WORD, &[0x14, 0x24]));
    assert!(wrote(reg::OCP, &[0x38]));
    assert!(wrote(reg::TX_CLAMP, &[0xDE]));
    assert!(wrote(reg::TX_MODULATION, &[0x04]));
    assert!(wrote(reg::LORA_IQ_POLARITY, &[0x0D]));
    assert!(wrote(reg::RX_GAIN, &[0x96]));
}

#[test]
fn init_sets_up_fast_gfsk() {
    let mut radio = new_radio(Modulation::FAST_FSK, vec![vec![0x00]]);
    block_on(radio.init()).unwrap();
    assert_eq!(sent(&radio, op::SET_PACKET_TYPE), [vec![0x8A, 0]]);
    // 250 kbit/s, BT 0.5, 467 kHz, 125 kHz deviation.
    assert_eq!(
        sent(&radio, op::SET_MODULATION_PARAMS),
        [vec![0x8B, 0x00, 0x10, 0x00, 0x09, 0x09, 0x02, 0x00, 0x00]]
    );
    // 64 preamble bits, 16-bit detector, 32-bit sync word, variable length, CRC, whitening.
    assert_eq!(
        sent(&radio, op::SET_PACKET_PARAMS),
        [vec![0x8C, 0, 64, 0x05, 32, 0, 1, 255, 0x02, 0x01]]
    );
}

#[test]
fn transmit_loads_the_buffer_and_waits_for_tx_done() {
    let mut radio = new_radio(
        Modulation::FAST_FSK,
        vec![(irq::TX_DONE).to_be_bytes().to_vec()],
    );
    block_on(radio.transmit(b"hello")).unwrap();
    assert_eq!(sent(&radio, op::WRITE_BUFFER), [b"\x0e\x00hello".to_vec()]);
    // Length 5 for this frame.
    assert_eq!(sent(&radio, op::SET_PACKET_PARAMS)[0][7], 5);
    let tx = &sent(&radio, op::SET_TX)[0];
    // Twice the 640 µs airtime, and 100 ms, in 15.625 µs steps.
    assert_eq!(u32::from_be_bytes([0, tx[1], tx[2], tx[3]]), 6481);

    let mut radio = new_radio(
        Modulation::FAST_FSK,
        vec![(irq::TIMEOUT).to_be_bytes().to_vec()],
    );
    assert_eq!(block_on(radio.transmit(b"lost")), Err(Error::Timeout));
    assert_eq!(block_on(radio.transmit(&[0; 256])), Err(Error::TooLong));
}

#[test]
fn received_packets_are_read_with_their_signal() {
    let answers = vec![
        (irq::RX_DONE | irq::HEADER_VALID).to_be_bytes().to_vec(),
        vec![4, 0x80],                 // 4 bytes, starting at 0x80
        b"mesh".to_vec(),              // the buffer
        vec![180, (-20i8) as u8, 170], // RSSI -90 dBm, SNR -5 dB
    ];
    let mut radio = new_radio(Modulation::LONG_RANGE_LORA, answers);
    let mut buf = [0u8; MAX_PACKET];
    let packet = block_on(radio.wait_for_packet(&mut buf)).unwrap().unwrap();
    assert_eq!(&buf[..packet.len], b"mesh");
    assert_eq!(packet.rssi_dbm, -90);
    assert_eq!(packet.snr_db, Some(-5));
    assert_eq!(sent(&radio, op::READ_BUFFER), [vec![op::READ_BUFFER, 0x80]]);

    // A bad CRC is dropped before anything is read.
    let answers = vec![(irq::RX_DONE | irq::CRC_ERROR).to_be_bytes().to_vec()];
    let mut radio = new_radio(Modulation::LONG_RANGE_LORA, answers);
    assert_eq!(block_on(radio.wait_for_packet(&mut buf)), Ok(None));
    assert!(sent(&radio, op::READ_BUFFER).is_empty());
}

#[test]
fn arriving_frames_hold_off_sending() {
    let answers = vec![
        irq::SYNC_WORD_VALID.to_be_bytes().to_vec(),
        (irq::SYNC_WORD_VALID | irq::RX_DONE).to_be_bytes().to_vec(),
        0u16.to_be_bytes().to_vec(),
    ];
    let mut radio = new_radio(Modulation::FAST_FSK, answers);
    assert_eq!(block_on(radio.is_receiving()), Ok(true));
    assert_eq!(block_on(radio.is_receiving()), Ok(false));
    assert_eq!(block_on(radio.is_receiving()), Ok(false));
}
