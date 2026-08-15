//! Allocation-free RNode LoRa transmit framing.

use crate::{
    LengthError, RNODE_LORA_DATA_PER_FRAME, RnodeFrameHeader, SX1262_FRAME_MTU, plan_frames,
    validate_rns_packet_len,
};

/// Caller-owned storage for one maximum-size SX1262 frame.
pub type RnodeTxFrameBuffer = [u8; SX1262_FRAME_MTU];

/// Borrowed views of the one or two physical frames for an RNS packet.
///
/// Each returned slice includes its one-byte RNode LoRa header. A split
/// packet uses the same header byte in both frames because the wire format has
/// no fragment index or final-fragment flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnodeTxFrames<'a> {
    first: &'a [u8],
    second: Option<&'a [u8]>,
}

impl<'a> RnodeTxFrames<'a> {
    /// First physical frame, including its RNode header byte.
    pub const fn first(self) -> &'a [u8] {
        self.first
    }

    /// Second physical frame for a split packet, including its header byte.
    pub const fn second(self) -> Option<&'a [u8]> {
        self.second
    }

    /// Number of physical frames represented by this value.
    pub const fn frame_count(self) -> usize {
        if self.second.is_some() { 2 } else { 1 }
    }

    /// Return a physical frame by transmission order.
    pub const fn frame(self, index: usize) -> Option<&'a [u8]> {
        match index {
            0 => Some(self.first),
            1 => self.second,
            _ => None,
        }
    }
}

/// Frame one complete RNS packet into caller-owned SX1262 buffers.
///
/// Packets through 254 bytes produce one physical frame. Larger packets
/// through the 500-byte project RNS MTU produce two frames. The four-bit wire
/// sequence is the low nibble of `sequence`; values therefore wrap modulo 16.
/// Packet bytes are copied verbatim, including values that a serial KISS link
/// would treat as escapes.
///
/// This function only constructs bytes. It does not initialize a radio,
/// perform channel access, or authorize transmission.
pub fn frame_rns_packet<'a>(
    packet: &[u8],
    sequence: u8,
    first_output: &'a mut RnodeTxFrameBuffer,
    second_output: &'a mut RnodeTxFrameBuffer,
) -> Result<RnodeTxFrames<'a>, LengthError> {
    validate_rns_packet_len(packet.len())?;

    // `RNS_MTU` is narrower than the RNode physical MTU, so validation above
    // guarantees that this physical plan cannot fail.
    let plan = plan_frames(packet.len())?;
    let split = plan.second_data_len.is_some();
    let header = RnodeFrameHeader::encode(sequence, split);

    first_output[0] = header;
    let first_end = 1 + plan.first_data_len;
    first_output[1..first_end].copy_from_slice(&packet[..plan.first_data_len]);

    let second = match plan.second_data_len {
        Some(second_data_len) => {
            second_output[0] = header;
            let second_end = 1 + second_data_len;
            second_output[1..second_end].copy_from_slice(&packet[RNODE_LORA_DATA_PER_FRAME..]);
            Some(&second_output[..second_end])
        }
        None => None,
    };

    Ok(RnodeTxFrames {
        first: &first_output[..first_end],
        second,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RNODE_LORA_SEQUENCE_MASK, RNS_MTU, RnodeRxReassembler};

    fn packet_bytes() -> [u8; RNS_MTU] {
        let mut packet = [0u8; RNS_MTU];
        for (index, byte) in packet.iter_mut().enumerate() {
            *byte = index as u8;
        }
        packet
    }

    #[test]
    fn exact_required_boundaries_are_framed_and_round_trip() {
        let packet = packet_bytes();

        for packet_len in [0, 1, 253, 254, 255, 256, 499, 500] {
            let mut first = [0xa5; SX1262_FRAME_MTU];
            let mut second = [0x5a; SX1262_FRAME_MTU];
            let frames =
                frame_rns_packet(&packet[..packet_len], 0x2b, &mut first, &mut second).unwrap();
            let expected_split = packet_len > RNODE_LORA_DATA_PER_FRAME;
            let expected_header = RnodeFrameHeader::encode(0x2b, expected_split);

            assert_eq!(frames.first()[0], expected_header, "length {packet_len}");
            assert_eq!(
                frames.first().len(),
                1 + packet_len.min(RNODE_LORA_DATA_PER_FRAME),
                "length {packet_len}"
            );
            assert_eq!(
                &frames.first()[1..],
                &packet[..packet_len.min(RNODE_LORA_DATA_PER_FRAME)],
                "length {packet_len}"
            );

            if expected_split {
                let second_frame = frames.second().unwrap();
                assert_eq!(second_frame[0], expected_header, "length {packet_len}");
                assert_eq!(
                    &second_frame[1..],
                    &packet[RNODE_LORA_DATA_PER_FRAME..packet_len],
                    "length {packet_len}"
                );
                assert_eq!(frames.frame_count(), 2);
                assert_eq!(frames.frame(0), Some(frames.first()));
                assert_eq!(frames.frame(1), Some(second_frame));
            } else {
                assert_eq!(frames.second(), None, "length {packet_len}");
                assert_eq!(frames.frame_count(), 1);
                assert_eq!(frames.frame(1), None);
            }
            assert_eq!(frames.frame(2), None);

            let mut rx = RnodeRxReassembler::new();
            let mut reassembled = [0u8; RNS_MTU];
            let first_outcome = rx.feed(frames.first(), &mut reassembled).unwrap();
            if let Some(second_frame) = frames.second() {
                assert!(matches!(
                    first_outcome,
                    crate::ReceiveOutcome::AwaitingContinuation { .. }
                ));
                rx.feed(second_frame, &mut reassembled).unwrap();
            }
            assert_eq!(&reassembled[..packet_len], &packet[..packet_len]);
        }
    }

    #[test]
    fn rejects_packets_above_the_project_rns_mtu() {
        let packet = [0u8; RNS_MTU + 1];
        let mut first = [0u8; SX1262_FRAME_MTU];
        let mut second = [0u8; SX1262_FRAME_MTU];

        assert_eq!(
            frame_rns_packet(&packet, 0, &mut first, &mut second),
            Err(LengthError {
                actual: RNS_MTU + 1,
                maximum: RNS_MTU,
            })
        );
    }

    #[test]
    fn sequence_is_masked_to_four_bits_and_wraps() {
        let packet = [0u8; 1];

        for (sequence, expected) in [(0, 0), (15, 15), (16, 0), (31, 15), (255, 15)] {
            let mut first = [0u8; SX1262_FRAME_MTU];
            let mut second = [0u8; SX1262_FRAME_MTU];
            let frames = frame_rns_packet(&packet, sequence, &mut first, &mut second).unwrap();
            assert_eq!(frames.first()[0] & RNODE_LORA_SEQUENCE_MASK, expected << 4);
        }
    }

    #[test]
    fn both_split_frames_have_the_exact_same_header() {
        let packet = [0x42; RNODE_LORA_DATA_PER_FRAME + 1];
        let mut first = [0u8; SX1262_FRAME_MTU];
        let mut second = [0u8; SX1262_FRAME_MTU];
        let frames = frame_rns_packet(&packet, 0x9e, &mut first, &mut second).unwrap();

        assert_eq!(frames.first()[0], 0xe1);
        assert_eq!(frames.second().unwrap()[0], 0xe1);
    }

    #[test]
    fn serial_escape_values_are_ordinary_rf_payload_bytes() {
        let packet = [0xc0, 0xdb, 0x7e, 0x7d, 0x00, 0xff];
        let mut first = [0u8; SX1262_FRAME_MTU];
        let mut second = [0u8; SX1262_FRAME_MTU];
        let frames = frame_rns_packet(&packet, 3, &mut first, &mut second).unwrap();

        assert_eq!(&frames.first()[1..], &packet);
    }
}
