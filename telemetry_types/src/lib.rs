//! Wire format shared by the car-side CAN→XBee bridge and the chase-side
//! telemetry receiver.
//!
//! Keep this crate dependency-light (serde + heapless only) so both ends
//! build on any platform — the radio library carries these as opaque bytes
//! and knows nothing about CAN.

use heapless::Vec;
use serde::{Deserialize, Serialize};

/// Set on [`CanFrameMsg::id`] when the frame uses a 29-bit extended CAN id.
/// (Standard ids are 11-bit, extended 29-bit, so bit 31 is always free.)
pub const EXTENDED_ID_FLAG: u32 = 1 << 31;

/// One CAN frame on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanFrameMsg {
    /// Raw CAN id, with [`EXTENDED_ID_FLAG`] OR-ed in for extended frames.
    pub id: u32,
    /// Frame payload; CAN 2.0 carries at most 8 bytes (dlc = length).
    pub data: Vec<u8, 8>,
    /// Milliseconds since [`CanBatch::base_ts_ms`], saturating at u16::MAX.
    pub ts_offset_ms: u16,
}

impl CanFrameMsg {
    /// The CAN id without the extended-frame flag.
    pub fn can_id(&self) -> u32 {
        self.id & !EXTENDED_ID_FLAG
    }

    pub fn is_extended(&self) -> bool {
        self.id & EXTENDED_ID_FLAG != 0
    }
}

/// Upper bound chosen so a worst-case batch, after encryption and COBS
/// framing, fits one XBee RF packet (NP, 256 bytes) — see `size_budget` test.
pub const MAX_FRAMES_PER_BATCH: usize = 11;

/// A group of CAN frames sent as a single encrypted radio packet.
/// Batching amortizes the fixed per-packet crypto + radio overhead.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanBatch {
    /// Unix time in ms when the first frame of the batch was captured.
    pub base_ts_ms: u64,
    pub frames: Vec<CanFrameMsg, MAX_FRAMES_PER_BATCH>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worst_case_batch() -> CanBatch {
        let frame = CanFrameMsg {
            // Largest varint encoding: extended id with the flag set
            id: 0x1FFF_FFFF | EXTENDED_ID_FLAG,
            data: Vec::from_slice(&[0xFF; 8]).unwrap(),
            ts_offset_ms: u16::MAX,
        };
        let mut batch = CanBatch {
            base_ts_ms: u64::MAX,
            frames: Vec::new(),
        };
        for _ in 0..MAX_FRAMES_PER_BATCH {
            batch.frames.push(frame.clone()).unwrap();
        }
        batch
    }

    #[test]
    fn postcard_roundtrip() {
        let batch = worst_case_batch();
        let mut buf = [0u8; 512];
        let bytes = postcard::to_slice(&batch, &mut buf).unwrap();
        let decoded: CanBatch = postcard::from_bytes(bytes).unwrap();
        assert_eq!(decoded, batch);
    }

    /// One sealed batch must fit one XBee RF packet. The radio's NP (max RF
    /// payload) is 256 bytes; the session layer adds a 1-byte wire tag around
    /// the SecureFrame, which can cost at most 2 extra bytes through COBS.
    #[test]
    fn size_budget_fits_one_rf_packet() {
        use aes_gcm::{Aes256Gcm, Key};
        use xbee_rust_modem_library::framing::encode_cobs;
        use xbee_rust_modem_library::secure_packet::seal;

        let batch = worst_case_batch();
        let mut plain = [0u8; 512];
        let plain = postcard::to_slice(&batch, &mut plain).unwrap();

        let key = *Key::<Aes256Gcm>::from_slice(&[0x42; 32]);
        // u64::MAX seq = worst-case varint, even though a session never gets there
        let frame = seal(&key, [0xFF; 4], u64::MAX, plain).unwrap();

        let mut wire = [0u8; 1024];
        let wire = encode_cobs(&frame, &mut wire).unwrap();
        assert!(
            wire.len() + 2 <= 256,
            "worst-case batch is {} wire bytes; exceeds the 256-byte RF payload budget",
            wire.len() + 2
        );
    }
}
