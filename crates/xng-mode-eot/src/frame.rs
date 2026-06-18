//! AAR S-9152 EOT / HOT 2-way telemetry frame parsing.
//!
//! GROUND TRUTH (cited): there is **no public formal AAR standard** for this
//! link; the bit layout below is the reverse-engineered field map shared,
//! byte-for-byte, by the two independent public decoders:
//!
//!   - ereuter/PyEOT      `eot_decoder.py`  (https://github.com/ereuter/PyEOT)
//!   - russinnes/EOTDecode `eot_decoder.py` (https://github.com/russinnes/EOTDecode)
//!
//! Both decoders model a 74-bit "packet" after Manchester decode + bit sync:
//!
//! ```text
//!   packet[ 0:11]  frame sync word  = 11100010010   (11 bits)
//!   packet[11:56]  data block       (45 bits, BCH-protected)
//!   packet[56:74]  BCH check word   (18 bits, ciphered)
//! ```
//!
//! On air a 17-bit run `10101011100010010` is hunted: the leading `101010`
//! is the tail of the bit-sync (clock) preamble and the trailing 11 bits are
//! the frame sync word. The decoders take `buffer[6:]` after the match, so the
//! frame sync becomes `packet[0:11]`.
//!
//! DATA-BLOCK FIELD MAP (bit indices into the 74-bit packet, exactly as the
//! cited decoders slice them; multi-bit fields are stored MSB-first on the
//! wire but the decoders reverse them to LSB-first before interpreting, which
//! we reproduce):
//!
//! ```text
//!   [11:13]  chaining bits          (2 bits; exact meaning undocumented*)
//!   [13:15]  battery condition      (2 bits, reversed): 11 OK, 10 Low,
//!                                    01 Very Low, 00 Not Monitored
//!   [15:18]  message type           (3 bits): 111 => status/arm message
//!   [18:35]  unit address           (17 bits, reversed) -> integer
//!   [35:42]  brake pipe pressure    (7 bits, reversed) -> psig integer
//!   [42:49]  battery charge         (7 bits, reversed) -> /127*100 %
//!   [49]     spare                  (1 bit)
//!   [50]     valve circuit / disc   (1 bit)
//!   [51]     conf/arm indicator     (1 bit; with msg_type 111: 0 Arming,
//!                                    1 Armed)
//!   [52]     turbine                (1 bit)
//!   [53]     motion                 (1 bit)
//!   [54]     marker light battery   (1 bit)
//!   [55]     marker light status    (1 bit)
//! ```
//!
//! (*) bits 11..13 fall inside the BCH-protected data block but neither cited
//! decoder names them; we surface them raw as `chaining` and note the gap.
//!
//! The DECODE layer here is anchored to that documented field map: the
//! integration test hand-builds a packet's exact bits per these slices, then
//! asserts both the field extraction AND the (independently computed) BCH
//! check verify together — spec-cited ground truth, not a self-modulator
//! round-trip.

use serde::Serialize;

use crate::bch;

/// 11-bit frame sync word that opens every packet (`packet[0:11]`), per the
/// cited decoders' `10101011100010010` hunt with a 6-bit bit-sync lead-in.
pub const FRAME_SYNC: [u8; 11] = [1, 1, 1, 0, 0, 0, 1, 0, 0, 1, 0];

/// 6-bit tail of the alternating bit-sync (clock) preamble preceding the
/// frame sync in the on-air hunt pattern (`101010`).
pub const BIT_SYNC_TAIL: [u8; 6] = [1, 0, 1, 0, 1, 0];

/// Total decoded packet length in bits (sync 11 + data 45 + check 18).
pub const PACKET_BITS: usize = 74;
/// Start of the BCH-protected 45-bit data block.
pub const DATA_START: usize = 11;
/// End (exclusive) of the data block / start of the 18-bit check word.
pub const DATA_END: usize = 56;

/// Human-readable battery condition (2-bit `[13:15]`, reversed-then-read).
fn battery_condition_text(code: u8) -> &'static str {
    match code {
        0b11 => "OK",
        0b10 => "Low",
        0b01 => "Very Low",
        0b00 => "Not Monitored",
        _ => "Unknown",
    }
}

/// One decoded EOT/HOT telemetry frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EotFrame {
    /// Whether the BCH(63,45) ciphered check verified over the data block.
    pub bch_ok: bool,
    /// 2 chaining bits (`packet[11:13]`); raw, meaning undocumented publicly.
    pub chaining: u8,
    /// Battery condition code (2 bits) and its decoded text.
    pub battery_condition: u8,
    pub battery_condition_text: String,
    /// 3-bit message type (`packet[15:18]`); `0b111` => status/arm message.
    pub message_type: u8,
    /// 17-bit EOT/HOT unit address (the device's programmed ID).
    pub unit_addr: u32,
    /// Brake-pipe pressure in psig (7-bit field).
    pub pressure_psi: u8,
    /// Marker-light / radio battery charge, percent (7-bit field / 127 * 100).
    pub battery_charge_pct: u8,
    /// Spare bit (`packet[49]`).
    pub spare: u8,
    /// Valve-circuit / disconnect status bit (`packet[50]`).
    pub valve_circuit: u8,
    /// Configuration / arm indicator (`packet[51]`); see `arm_status`.
    pub conf_indicator: u8,
    /// Turbine (charger) running bit (`packet[52]`).
    pub turbine: u8,
    /// Motion bit (`packet[53]`): 1 = EOT detects train motion.
    pub motion: u8,
    /// Marker-light battery condition bit (`packet[54]`).
    pub marker_light_batt: u8,
    /// Marker-light on/off status bit (`packet[55]`).
    pub marker_light: u8,
    /// Arm status text, only meaningful for message type `0b111`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm_status: Option<String>,
}

/// Read a multi-bit field from `bits[start..end]`, reversed (LSB-first) to an
/// integer — the cited decoders reverse each multi-bit field before
/// `int(..., 2)`. Reversing the slice and reading MSB-first yields the same
/// value as reading the original slice LSB-first.
fn field_rev(bits: &[u8], start: usize, end: usize) -> u32 {
    let mut v = 0u32;
    for &b in bits[start..end].iter().rev() {
        v = (v << 1) | (b as u32);
    }
    v
}

/// Parse a 74-bit decoded packet into an [`EotFrame`].
///
/// Returns `None` if the slice is too short or the frame sync word is wrong.
/// `bch_ok` reflects the documented ciphered BCH check over `packet[11:56]`.
pub fn parse_packet(packet: &[u8]) -> Option<EotFrame> {
    if packet.len() < PACKET_BITS {
        return None;
    }
    if packet[0..11] != FRAME_SYNC {
        return None;
    }

    let data_block = &packet[DATA_START..DATA_END]; // 45 bits
    let received_check = &packet[DATA_END..PACKET_BITS]; // 18 bits
    let bch_ok = bch::verify(data_block, received_check);

    // Chaining bits are stored as-is (no documented LSB/MSB convention).
    let chaining = (packet[11] << 1) | packet[12];

    let battery_condition = field_rev(packet, 13, 15) as u8;
    let message_type = field_rev(packet, 15, 18) as u8;
    let unit_addr = field_rev(packet, 18, 35);
    let pressure_psi = field_rev(packet, 35, 42) as u8;
    let battery_charge_raw = field_rev(packet, 42, 49);
    let battery_charge_pct = ((battery_charge_raw as f32 / 127.0) * 100.0).round() as u8;

    let spare = packet[49];
    let valve_circuit = packet[50];
    let conf_indicator = packet[51];
    let turbine = packet[52];
    let motion = packet[53];
    let marker_light_batt = packet[54];
    let marker_light = packet[55];

    // Arm status is only defined for the status message type (0b111), where
    // the configuration indicator distinguishes "Arming" from "Armed".
    let arm_status = if message_type == 0b111 {
        Some(
            if conf_indicator == 1 {
                "Armed"
            } else {
                "Arming"
            }
            .to_string(),
        )
    } else {
        None
    };

    Some(EotFrame {
        bch_ok,
        chaining,
        battery_condition,
        battery_condition_text: battery_condition_text(battery_condition).to_string(),
        message_type,
        unit_addr,
        pressure_psi,
        battery_charge_pct,
        spare,
        valve_circuit,
        conf_indicator,
        turbine,
        motion,
        marker_light_batt,
        marker_light,
        arm_status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `value` into `bits[start..end]` in the SAME on-air orientation
    /// the decoders read: the field is LSB-first on the wire, so MSB-first the
    /// slice is `value` reversed. (Inverse of [`field_rev`].)
    fn set_field_rev(bits: &mut [u8], start: usize, end: usize, value: u32) {
        let width = end - start;
        for i in 0..width {
            // field_rev reads bits[start..end] reversed, so on-wire bit at
            // position `start+i` carries value bit `i` (LSB-first).
            bits[start + i] = ((value >> i) & 1) as u8;
        }
    }

    #[test]
    fn field_rev_roundtrips_set_field() {
        let mut bits = vec![0u8; 74];
        set_field_rev(&mut bits, 18, 35, 0x1ABCD & 0x1FFFF);
        assert_eq!(field_rev(&bits, 18, 35), 0x1ABCD & 0x1FFFF);
    }

    #[test]
    fn rejects_bad_sync() {
        let mut p = vec![0u8; 74];
        p[0..11].copy_from_slice(&FRAME_SYNC);
        p[3] ^= 1; // corrupt sync
        assert!(parse_packet(&p).is_none());
    }

    #[test]
    fn rejects_short_packet() {
        assert!(parse_packet(&[1, 1, 1]).is_none());
    }
}
