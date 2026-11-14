//! CHU (NRC Canada) AFSK time-code decoder — the flagship.
//!
//! CHU broadcasts a digital time code in audio seconds 31–39 of each minute as
//! Bell-103 AFSK: **MARK = 2225 Hz** (logical 1 / idle), **SPACE = 2025 Hz**
//! (logical 0), **300 baud**, **8N2** async framing (1 start bit = space, 8
//! data bits LSB-first, 2 stop bits = mark; 11 bits per character). A packet is
//! **10 bytes = 110 bits**; per second the audio carries 0–10 ms of a 1000 Hz
//! tick, 10–133.3 ms of MARK preamble, then the 110 data bits from 133.3 ms to
//! exactly 500 ms (the NRC "CHU broadcast format" + NTP `refclock_chu`/driver7
//! description; see PROVENANCE.md).
//!
//! Two packet formats, each 10 bytes = 5 data bytes + 5 redundancy bytes; each
//! byte = 2 BCD nibbles:
//!
//! - **Format A** (seconds 32–39): data nibbles `[6][D][D][D][H][H][M][M][S][S]`
//!   — 6 = frame id, DDD = day-of-year, HH/MM/SS = UTC. Redundancy bytes are an
//!   EXACT COPY of the 5 data bytes (validate: `data == copy`).
//! - **Format B** (second 31, once/min): data nibbles
//!   `[X][Z][Y][Y][Y][Y][T][T][A][A]` — X = leap/DUT1-sign code, Z = |DUT1| in
//!   tenths, YYYY = Gregorian year, TT = TAI−UTC, AA = Canada DST byte.
//!   Redundancy bytes are the ONE'S COMPLEMENT of the 5 data bytes (validate:
//!   `data == ~copy`).
//!
//! This module owns the decode chain: [`afsk_bits`] (mark/space discriminator
//! → UART async receiver) and [`parse_packet`] (BCD → fields, redundancy gate).
//! The IQ → audio front end and combiner live in [`crate`].

use crate::audio::{Biquad, Goertzel};

/// AFSK mark tone (logical 1 / idle), Hz.
pub const MARK_HZ: f64 = 2225.0;
/// AFSK space tone (logical 0), Hz.
pub const SPACE_HZ: f64 = 2025.0;
/// Discriminator decision center between the tones, Hz.
pub const CENTER_HZ: f64 = (MARK_HZ + SPACE_HZ) / 2.0; // 2125
/// Symbol rate.
pub const BAUD: f64 = 300.0;
/// Data bits per packet (10 bytes × 11-bit chars).
pub const PACKET_BITS: usize = 110;
/// Bytes per packet (5 data + 5 redundancy).
pub const PACKET_BYTES: usize = 10;

/// One demodulated 8N2 character: the 8 data bits as a byte, plus whether the
/// start/stop framing was valid (start = space, both stops = mark).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uart8N2 {
    pub byte: u8,
    pub framing_ok: bool,
}

/// AFSK tone discriminator over a CHU audio slice: bandpass ~1900–2350 Hz, then
/// per-bit-window Goertzel pair at 2225 (mark) / 2025 (space) and slice toward
/// whichever is stronger. Returns one logical bit per symbol (1 = mark).
///
/// `start_sample` is where bit 0 begins; `samples_per_bit = fs / 300`. The
/// caller locates the first start edge (UART falling edge, mark→space).
pub fn afsk_bits(audio: &[f32], sample_rate: f64, start_sample: f64, n_bits: usize) -> Vec<u8> {
    // Bandpass keeps the two AFSK tones and rejects the 1000 Hz tick / voice.
    let mut bp = Biquad::bandpass(CENTER_HZ, 6.0, sample_rate);
    let filt = bp.filter(audio);

    let spb = sample_rate / BAUD;
    let mut bits = Vec::with_capacity(n_bits);
    for k in 0..n_bits {
        // Integrate the central ~70% of each bit window (skip edges where the
        // tone is transitioning) through a fresh Goertzel pair.
        let center = start_sample + (k as f64 + 0.5) * spb;
        let half = spb * 0.35;
        let lo = (center - half).round().max(0.0) as usize;
        let hi = ((center + half).round() as usize).min(filt.len());
        if lo >= hi {
            bits.push(1); // ran out of audio → treat as idle mark
            continue;
        }
        let mut g_mark = Goertzel::new(MARK_HZ, sample_rate);
        let mut g_space = Goertzel::new(SPACE_HZ, sample_rate);
        for &s in &filt[lo..hi] {
            g_mark.add(s);
            g_space.add(s);
        }
        bits.push((g_mark.power() >= g_space.power()) as u8);
    }
    bits
}

/// Decode an 8N2 async character from a bit window: `bits` must be 11 bits —
/// `[start, d0..d7, stop, stop]`. Start must be space (0), both stops mark (1);
/// data is read LSB-first.
pub fn decode_8n2(bits: &[u8]) -> Option<Uart8N2> {
    if bits.len() < 11 {
        return None;
    }
    let start_ok = bits[0] == 0;
    let stop_ok = bits[9] == 1 && bits[10] == 1;
    let mut byte = 0u8;
    for i in 0..8 {
        byte |= bits[1 + i] << i; // LSB-first
    }
    Some(Uart8N2 {
        byte,
        framing_ok: start_ok && stop_ok,
    })
}

/// Read a stream of demodulated logical bits as a sequence of 8N2 characters.
/// `bits` is the contiguous on-air bit stream starting at the first start bit;
/// every 11 bits is one character. Returns the recovered bytes and whether
/// each had valid framing.
pub fn read_chars(bits: &[u8]) -> Vec<Uart8N2> {
    bits.chunks_exact(11).filter_map(decode_8n2).collect()
}

/// A two-BCD-nibble byte → its high and low decimal digits, if both nibbles
/// are valid BCD (0–9).
fn bcd_byte(b: u8) -> Option<(u8, u8)> {
    let hi = b >> 4;
    let lo = b & 0x0F;
    if hi <= 9 && lo <= 9 {
        Some((hi, lo))
    } else {
        None
    }
}

/// CHU packet format discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChuFormat {
    /// Seconds 32–39: time-of-day, redundancy = exact copy.
    A,
    /// Second 31: year/DUT1/leap/DST, redundancy = ones-complement.
    B,
}

/// Decoded CHU packet fields. Format A carries time-of-day; Format B carries
/// the year / DUT1 / leap-second / DST metadata.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChuPacket {
    pub format: Option<ChuFormatFields>,
    /// Redundancy gate result (A: data == copy; B: data == ~copy).
    pub redundancy_ok: bool,
}

/// Format-tagged decoded fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChuFormatFields {
    /// Time-of-day from a Format A packet.
    A {
        day_of_year: u16,
        hour: u8,
        minute: u8,
        second: u8,
    },
    /// Metadata from a Format B packet.
    B {
        year: u16,
        /// DUT1 in seconds (signed, tenths resolution).
        dut1_s: f32,
        /// TAI − UTC seconds.
        tai_minus_utc: u8,
        /// Leap-second pending flag (decoded from the X nibble).
        leap_pending: bool,
        /// Raw Canada DST byte (AA), surfaced as-is.
        dst_code: u8,
    },
}

/// Parse a 10-byte CHU packet (5 data + 5 redundancy). The format is inferred:
/// if the redundancy bytes equal the data bytes it's Format A; if they equal
/// the ones-complement it's Format B. The matching gate sets `redundancy_ok`.
///
/// Returns `None` only if the byte count is wrong; a packet that fails the
/// redundancy gate or BCD validity still returns with `redundancy_ok = false`
/// / `format = None` so the caller can see it was attempted.
pub fn parse_packet(bytes: &[u8]) -> Option<ChuPacket> {
    if bytes.len() != PACKET_BYTES {
        return None;
    }
    let data = &bytes[0..5];
    let copy = &bytes[5..10];

    let is_a = data == copy;
    let is_b = data.iter().zip(copy).all(|(&d, &c)| d == !c);

    if is_a {
        Some(ChuPacket {
            format: parse_format_a(data),
            redundancy_ok: true,
        })
    } else if is_b {
        Some(ChuPacket {
            format: parse_format_b(data),
            redundancy_ok: true,
        })
    } else {
        // Redundancy failed: still try a best-effort parse so partial info is
        // available, but flag it as unvalidated.
        let format = parse_format_a(data).or_else(|| parse_format_b(data));
        Some(ChuPacket {
            format,
            redundancy_ok: false,
        })
    }
}

/// Format A data bytes → time-of-day. Nibbles `[6][D][D][D][H][H][M][M][S][S]`.
/// Byte 0 = `0x6D` (frame id 6, day hundreds), byte1 = day tens/units, byte2 =
/// hour, byte3 = minute, byte4 = second (each a BCD pair).
fn parse_format_a(data: &[u8]) -> Option<ChuFormatFields> {
    let (n0, d_h) = bcd_byte(data[0])?; // n0 = frame id (6), d_h = day hundreds
    if n0 != 6 {
        return None;
    }
    let (d_t, d_u) = bcd_byte(data[1])?;
    let (h_t, h_u) = bcd_byte(data[2])?;
    let (m_t, m_u) = bcd_byte(data[3])?;
    let (s_t, s_u) = bcd_byte(data[4])?;

    let day_of_year = d_h as u16 * 100 + d_t as u16 * 10 + d_u as u16;
    let hour = h_t * 10 + h_u;
    let minute = m_t * 10 + m_u;
    let second = s_t * 10 + s_u;

    if !(1..=366).contains(&day_of_year) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(ChuFormatFields::A {
        day_of_year,
        hour,
        minute,
        second,
    })
}

/// Format B data bytes → metadata. Nibbles `[X][Z][Y][Y][Y][Y][T][T][A][A]`.
/// X = leap/DUT1-sign code (high nibble of byte0), Z = |DUT1| tenths (low
/// nibble of byte0), YYYY = year (bytes 1–2 BCD), TT = TAI−UTC (byte3 BCD),
/// AA = DST byte (byte4).
fn parse_format_b(data: &[u8]) -> Option<ChuFormatFields> {
    let x = data[0] >> 4; // sign/leap code nibble
    let z = data[0] & 0x0F; // |DUT1| tenths nibble
    let (y_th, y_h) = bcd_byte(data[1])?; // thousands, hundreds
    let (y_t, y_u) = bcd_byte(data[2])?; // tens, units
    let (t_t, t_u) = bcd_byte(data[3])?; // TAI-UTC BCD
    let dst_code = data[4];

    if z > 9 {
        return None;
    }
    let year = y_th as u16 * 1000 + y_h as u16 * 100 + y_t as u16 * 10 + y_u as u16;
    if !(1900..=2200).contains(&year) {
        return None;
    }
    // The X nibble carries the DUT1 sign and a leap-second-pending indication.
    // NRC encodes a positive DUT1 sign and the pending leap in this nibble;
    // we surface a signed DUT1 (sign bit = high bit of X) and a leap flag
    // (any non-zero leap code). The exact sub-bit layout is not publicly
    // pinned, so we keep the conservative interpretation documented in
    // PROVENANCE.md.
    let dut1_negative = (x & 0x8) != 0;
    let leap_pending = (x & 0x7) != 0;
    let mag = z as f32 * 0.1;
    let dut1_s = if dut1_negative { -mag } else { mag };
    let tai_minus_utc = t_t * 10 + t_u;

    Some(ChuFormatFields::B {
        year,
        dut1_s,
        tai_minus_utc,
        leap_pending,
        dst_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the 11-bit 8N2 on-air bit sequence for one byte, LSB-first data,
    /// start=0, two stop=1.
    fn char_bits(byte: u8) -> Vec<u8> {
        let mut v = vec![0u8]; // start = space
        for i in 0..8 {
            v.push((byte >> i) & 1);
        }
        v.push(1); // stop
        v.push(1); // stop
        v
    }

    #[test]
    fn uart_8n2_round_trip() {
        for b in [0x00u8, 0x69, 0xA5, 0xFF, 0x6D] {
            let c = decode_8n2(&char_bits(b)).unwrap();
            assert_eq!(c.byte, b);
            assert!(c.framing_ok);
        }
    }

    #[test]
    fn uart_8n2_detects_bad_framing() {
        let mut bits = char_bits(0x42);
        bits[0] = 1; // start should be space=0
        assert!(!decode_8n2(&bits).unwrap().framing_ok);
        let mut bits = char_bits(0x42);
        bits[9] = 0; // stop should be mark=1
        assert!(!decode_8n2(&bits).unwrap().framing_ok);
    }

    /// Format A packet bytes for a known UTC: doy 159, 12:34:56.
    fn format_a_bytes(doy: u16, h: u8, m: u8, s: u8) -> Vec<u8> {
        let bcd2 = |hi: u8, lo: u8| (hi << 4) | lo;
        let data = vec![
            bcd2(6, (doy / 100) as u8),
            bcd2(((doy / 10) % 10) as u8, (doy % 10) as u8),
            bcd2(h / 10, h % 10),
            bcd2(m / 10, m % 10),
            bcd2(s / 10, s % 10),
        ];
        let mut full = data.clone();
        full.extend_from_slice(&data); // exact copy
        full
    }

    #[test]
    fn format_a_parses_and_validates() {
        let bytes = format_a_bytes(159, 12, 34, 56);
        let pkt = parse_packet(&bytes).unwrap();
        assert!(pkt.redundancy_ok);
        match pkt.format.unwrap() {
            ChuFormatFields::A { day_of_year, hour, minute, second } => {
                assert_eq!(day_of_year, 159);
                assert_eq!(hour, 12);
                assert_eq!(minute, 34);
                assert_eq!(second, 56);
            }
            other => panic!("expected Format A, got {other:?}"),
        }
    }

    #[test]
    fn format_a_redundancy_mismatch_flagged() {
        let mut bytes = format_a_bytes(100, 1, 2, 3);
        bytes[7] ^= 0x10; // corrupt a copy byte
        let pkt = parse_packet(&bytes).unwrap();
        assert!(!pkt.redundancy_ok);
    }

    /// Format B packet bytes for a known year/DUT1/TAI.
    fn format_b_bytes(year: u16, x: u8, z: u8, tai: u8, dst: u8) -> Vec<u8> {
        let bcd2 = |hi: u8, lo: u8| (hi << 4) | lo;
        let data = vec![
            (x << 4) | z,
            bcd2((year / 1000) as u8, ((year / 100) % 10) as u8),
            bcd2(((year / 10) % 10) as u8, (year % 10) as u8),
            bcd2(tai / 10, tai % 10),
            dst,
        ];
        let mut full = data.clone();
        // ones-complement redundancy
        full.extend(data.iter().map(|b| !b));
        full
    }

    #[test]
    fn format_b_parses_and_validates() {
        // year 2026, X=0 (positive sign, no leap), Z=3 → DUT1 +0.3 s, TAI 37.
        let bytes = format_b_bytes(2026, 0x0, 3, 37, 0);
        let pkt = parse_packet(&bytes).unwrap();
        assert!(pkt.redundancy_ok);
        match pkt.format.unwrap() {
            ChuFormatFields::B { year, dut1_s, tai_minus_utc, leap_pending, .. } => {
                assert_eq!(year, 2026);
                assert!((dut1_s - 0.3).abs() < 1e-6);
                assert_eq!(tai_minus_utc, 37);
                assert!(!leap_pending);
            }
            other => panic!("expected Format B, got {other:?}"),
        }
    }

    #[test]
    fn format_b_negative_dut1_and_leap() {
        // X high bit set → negative DUT1; low bits non-zero → leap pending.
        let bytes = format_b_bytes(2024, 0x9, 2, 36, 0);
        let pkt = parse_packet(&bytes).unwrap();
        assert!(pkt.redundancy_ok);
        match pkt.format.unwrap() {
            ChuFormatFields::B { dut1_s, leap_pending, .. } => {
                assert!((dut1_s + 0.2).abs() < 1e-6);
                assert!(leap_pending);
            }
            other => panic!("expected Format B, got {other:?}"),
        }
    }

    #[test]
    fn format_b_redundancy_is_ones_complement() {
        // Format B data with an EXACT-copy redundancy: the copy matches, so the
        // packet takes the Format-A gate (redundancy_ok = true), but the bytes
        // don't parse as a valid Format A (byte0 high nibble isn't the frame id
        // 6), so no fields decode — proving the complement is what makes a
        // packet decode AS Format B.
        let bytes = format_b_bytes(2026, 0x0, 3, 37, 0);
        let data = &bytes[0..5];
        let mut copy_redundancy = data.to_vec();
        copy_redundancy.extend_from_slice(data); // exact copy, not complement
        let pkt = parse_packet(&copy_redundancy).unwrap();
        // The exact copy satisfies the A gate but yields no valid A fields.
        assert!(pkt.format.is_none(), "exact-copy of B data must not parse");

        // The correct complement redundancy decodes as Format B.
        let pkt_b = parse_packet(&bytes).unwrap();
        assert!(pkt_b.redundancy_ok);
        assert!(matches!(pkt_b.format, Some(ChuFormatFields::B { .. })));
    }

    #[test]
    fn read_chars_recovers_packet_bytes() {
        let bytes = format_a_bytes(200, 23, 59, 58);
        let mut bits = Vec::new();
        for &b in &bytes {
            bits.extend(char_bits(b));
        }
        let chars = read_chars(&bits);
        assert_eq!(chars.len(), PACKET_BYTES);
        let recovered: Vec<u8> = chars.iter().map(|c| c.byte).collect();
        assert_eq!(recovered, bytes);
        assert!(chars.iter().all(|c| c.framing_ok));
    }
}
