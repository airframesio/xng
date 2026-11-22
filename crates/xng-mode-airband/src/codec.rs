//! Sample-format helpers and container writers that need no codec library:
//! f32 → PCM16, G.711 µ-law, RIFF/WAVE headers and Ogg pages (RFC 3533) for
//! Ogg/Opus (RFC 7845) when an Opus encoder is available.

/// Convert to signed 16-bit PCM with clipping.
pub fn to_i16(x: &[f32]) -> Vec<i16> {
    x.iter().map(|&v| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16).collect()
}

/// Little-endian bytes of PCM16 samples.
pub fn i16_le_bytes(x: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(x.len() * 2);
    for s in x {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

const ULAW_BIAS: i32 = 0x84;
const ULAW_CLIP: i32 = 32_635;

/// G.711 µ-law encode one PCM16 sample.
pub fn ulaw_encode(sample: i16) -> u8 {
    let mut s = sample as i32;
    let sign = if s < 0 {
        s = -s;
        0x80
    } else {
        0
    };
    s = s.min(ULAW_CLIP) + ULAW_BIAS;
    let mut exponent = 7;
    let mut mask = 0x4000;
    while exponent > 0 && s & mask == 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = (s >> (exponent + 3)) & 0x0F;
    !(sign | (exponent << 4) | mantissa) as u8
}

/// G.711 µ-law decode one byte.
pub fn ulaw_decode(byte: u8) -> i16 {
    let u = !byte;
    let sign = u & 0x80;
    let exponent = ((u >> 4) & 0x07) as i32;
    let mantissa = (u & 0x0F) as i32;
    let s = (((mantissa << 3) + ULAW_BIAS) << exponent) - ULAW_BIAS;
    if sign != 0 {
        -s as i16
    } else {
        s as i16
    }
}

/// 44-byte canonical WAVE header for mono PCM16. `data_len` is the byte
/// length of the sample data (use 0xFFFF_FFFF minus the header for an
/// open-ended live stream).
pub fn wav_header(sample_rate: u32, data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    let byte_rate = sample_rate * 2;
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&data_len.saturating_add(36).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// CRC-32 as used by Ogg (poly 0x04C11DB7, no reflection, init 0).
fn ogg_crc(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04C1_1DB7 } else { r << 1 };
            }
            *e = r;
        }
        t
    });
    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ t[((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

/// Writes one logical Ogg bitstream, one packet per page (fine for 20 ms
/// voice packets and keeps latency to a single packet).
pub struct OggWriter {
    serial: u32,
    seq: u32,
}

impl OggWriter {
    pub fn new(serial: u32) -> Self {
        Self { serial, seq: 0 }
    }

    /// Build a page holding `packet`. `granule` is the absolute granule
    /// position after this packet (48 kHz samples for Opus).
    pub fn page(&mut self, packet: &[u8], granule: u64, bos: bool, eos: bool) -> Vec<u8> {
        let mut lacing = Vec::with_capacity(packet.len() / 255 + 1);
        let mut left = packet.len();
        loop {
            if left >= 255 {
                lacing.push(255u8);
                left -= 255;
            } else {
                lacing.push(left as u8);
                break;
            }
        }
        assert!(lacing.len() <= 255, "packet too large for a single Ogg page");
        let mut p = Vec::with_capacity(27 + lacing.len() + packet.len());
        p.extend_from_slice(b"OggS");
        p.push(0);
        p.push(if bos { 0x02 } else { 0 } | if eos { 0x04 } else { 0 });
        p.extend_from_slice(&granule.to_le_bytes());
        p.extend_from_slice(&self.serial.to_le_bytes());
        p.extend_from_slice(&self.seq.to_le_bytes());
        p.extend_from_slice(&[0, 0, 0, 0]);
        p.push(lacing.len() as u8);
        p.extend_from_slice(&lacing);
        p.extend_from_slice(packet);
        let crc = ogg_crc(&p);
        p[22..26].copy_from_slice(&crc.to_le_bytes());
        self.seq = self.seq.wrapping_add(1);
        p
    }
}

/// RFC 7845 `OpusHead` identification header (mono, no pre-skip beyond
/// `pre_skip`, mapping family 0).
pub fn opus_head(input_rate: u32, pre_skip: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(1); // channels
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&input_rate.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // mapping family
    h
}

/// RFC 7845 `OpusTags` comment header.
pub fn opus_tags(vendor: &str, comments: &[(&str, &str)]) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(b"OpusTags");
    h.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    h.extend_from_slice(vendor.as_bytes());
    h.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for (k, v) in comments {
        let c = format!("{k}={v}");
        h.extend_from_slice(&(c.len() as u32).to_le_bytes());
        h.extend_from_slice(c.as_bytes());
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulaw_roundtrip_is_close() {
        for s in [-32_000i16, -1_000, -10, 0, 10, 1_000, 8_000, 32_000] {
            let d = ulaw_decode(ulaw_encode(s));
            let err = (d as i32 - s as i32).abs();
            assert!(err <= (s as i32).abs() / 16 + 8, "{s} -> {d}");
        }
    }

    #[test]
    fn ulaw_known_values() {
        // G.711: zero encodes to 0xFF, full positive to 0x80, full negative to 0x00.
        assert_eq!(ulaw_encode(0), 0xFF);
        assert_eq!(ulaw_encode(32_767), 0x80);
        assert_eq!(ulaw_encode(-32_768), 0x00);
    }

    #[test]
    fn wav_header_layout() {
        let h = wav_header(16_000, 32_000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 32_036);
        assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(h[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 32_000);
    }

    #[test]
    fn ogg_crc_matches_reference() {
        // CRC of an Ogg page with its CRC field zeroed must verify: recompute
        // over the finished page with the field zeroed again.
        let mut w = OggWriter::new(0x1234_5678);
        let page = w.page(&opus_head(16_000, 312), 0, true, false);
        let mut z = page.clone();
        z[22..26].copy_from_slice(&[0; 4]);
        assert_eq!(ogg_crc(&z).to_le_bytes(), page[22..26]);
        assert_eq!(&page[0..4], b"OggS");
        assert_eq!(page[5], 0x02);
        // Known vector: CRC-32/MPEG-2-without-final-xor of "123456789" with init 0.
        assert_eq!(ogg_crc(b"123456789"), 0x89A1_897F);
    }

    #[test]
    fn ogg_lacing_for_large_packet() {
        let mut w = OggWriter::new(1);
        let page = w.page(&vec![7u8; 600], 960, false, false);
        assert_eq!(page[26], 3); // 255 + 255 + 90
        assert_eq!(&page[27..30], &[255, 255, 90]);
        assert_eq!(page.len(), 27 + 3 + 600);
    }

    #[test]
    fn pcm_conversion_clips() {
        assert_eq!(to_i16(&[2.0, -2.0, 0.0]), vec![32767, -32767, 0]);
    }
}
