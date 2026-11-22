//! Opus encoding for airband audio (20 ms mono frames at 16 kHz), plus an
//! Ogg/Opus stream builder for recordings and Icecast.

use xng_mode_airband::codec::{opus_head, opus_tags, OggWriter};

/// Samples per 20 ms frame at the hub rate.
pub const FRAME: usize = (super::hub::AUDIO_RATE as usize) / 50;
/// Ogg/Opus granule positions count 48 kHz samples.
const GRANULE_PER_FRAME: u64 = 960;

pub struct OpusEncoder {
    enc: opus::Encoder,
    pending: Vec<f32>,
    out: Vec<u8>,
    lookahead: u16,
}

impl OpusEncoder {
    pub fn new(bitrate: u32) -> anyhow::Result<Self> {
        let mut enc = opus::Encoder::new(super::hub::AUDIO_RATE, opus::Channels::Mono, opus::Application::Voip)?;
        enc.set_bitrate(opus::Bitrate::Bits(bitrate.clamp(6_000, 128_000) as i32))?;
        enc.set_vbr(true)?;
        // Survive the occasional lost datagram (QUIC feed) gracefully.
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(5)?;
        let lookahead = enc.get_lookahead().unwrap_or(0).clamp(0, u16::MAX as i32) as u16;
        Ok(Self { enc, pending: Vec::with_capacity(FRAME * 2), out: vec![0u8; 4_000], lookahead })
    }

    /// Encoder delay in 48 kHz samples (Ogg `pre_skip`).
    pub fn pre_skip(&self) -> u16 {
        // get_lookahead reports samples at the encoder rate (16 kHz).
        self.lookahead.saturating_mul(3)
    }

    /// Buffer `samples`; return a packet for every complete 20 ms frame.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(samples);
        let mut packets = Vec::new();
        while self.pending.len() >= FRAME {
            let frame: Vec<f32> = self.pending.drain(..FRAME).collect();
            if let Some(p) = self.encode(&frame) {
                packets.push(p);
            }
        }
        packets
    }

    /// Encode whatever is buffered, padded with silence to a full frame.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty() {
            return None;
        }
        let mut frame = std::mem::take(&mut self.pending);
        frame.resize(FRAME, 0.0);
        self.encode(&frame)
    }

    /// Samples buffered toward the next frame.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn encode(&mut self, frame: &[f32]) -> Option<Vec<u8>> {
        match self.enc.encode_float(frame, &mut self.out) {
            Ok(n) => Some(self.out[..n].to_vec()),
            Err(e) => {
                tracing::warn!("opus encode failed: {e}");
                None
            }
        }
    }
}

/// An Ogg/Opus byte stream: headers first, then one page per packet.
pub struct OggOpus {
    enc: OpusEncoder,
    ogg: OggWriter,
    granule: u64,
    /// Real (unpadded) input so far, at the hub rate.
    input: u64,
}

impl OggOpus {
    /// Returns the stream and its two header pages.
    pub fn new(bitrate: u32, serial: u32, title: &str) -> anyhow::Result<(Self, Vec<u8>)> {
        let enc = OpusEncoder::new(bitrate)?;
        let mut ogg = OggWriter::new(serial);
        let mut head = ogg.page(&opus_head(super::hub::AUDIO_RATE, enc.pre_skip()), 0, true, false);
        let vendor = concat!("xng ", env!("CARGO_PKG_VERSION"));
        head.extend(ogg.page(&opus_tags(vendor, &[("TITLE", title)]), 0, false, false));
        Ok((Self { enc, ogg, granule: 0, input: 0 }, head))
    }

    /// Encode samples; returns the Ogg pages produced.
    pub fn push(&mut self, samples: &[f32]) -> Vec<u8> {
        let mut out = Vec::new();
        self.input += samples.len() as u64;
        for p in self.enc.push(samples) {
            self.granule += GRANULE_PER_FRAME;
            out.extend(self.ogg.page(&p, self.granule, false, false));
        }
        out
    }

    /// Flush the last partial frame, encode enough silence to push the
    /// encoder's delay out, and end the stream. The last page's granule is
    /// the real input length plus `pre_skip` (RFC 7845 §4), so players trim
    /// the padding and the decoded length matches what went in.
    pub fn finish(&mut self) -> Vec<u8> {
        let end = self.input * 3 + self.enc.pre_skip() as u64;
        let mut packets: Vec<Vec<u8>> = self.enc.flush().into_iter().collect();
        while self.granule + packets.len() as u64 * GRANULE_PER_FRAME < end || packets.is_empty() {
            match self.enc.encode(&[0.0; FRAME]) {
                Some(p) => packets.push(p),
                None => break,
            }
        }
        let mut out = Vec::new();
        let n = packets.len();
        for (i, p) in packets.into_iter().enumerate() {
            self.granule += GRANULE_PER_FRAME;
            let last = i + 1 == n;
            let granule = if last { end.min(self.granule) } else { self.granule };
            out.extend(self.ogg.page(&p, granule, false, last));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_a_tone() {
        let mut enc = OpusEncoder::new(24_000).unwrap();
        let tone: Vec<f32> =
            (0..16_000).map(|i| 0.5 * (std::f32::consts::TAU * 1_000.0 * i as f32 / 16_000.0).sin()).collect();
        let packets = enc.push(&tone);
        assert_eq!(packets.len(), 50);
        let mut dec = opus::Decoder::new(16_000, opus::Channels::Mono).unwrap();
        let mut pcm = vec![0f32; FRAME];
        let mut energy = 0.0;
        for p in &packets[10..] {
            let n = dec.decode_float(p, &mut pcm, false).unwrap();
            assert_eq!(n, FRAME);
            energy += pcm.iter().map(|v| v * v).sum::<f32>();
        }
        let rms = (energy / (40.0 * FRAME as f32)).sqrt();
        assert!((rms - 0.354).abs() < 0.1, "rms {rms}");
    }

    #[test]
    fn ogg_stream_starts_with_headers() {
        let (mut s, head) = OggOpus::new(24_000, 7, "test").unwrap();
        assert_eq!(&head[0..4], b"OggS");
        assert_eq!(&head[28..36], b"OpusHead");
        let body = s.push(&vec![0.1; FRAME * 3]);
        assert_eq!(body.windows(4).filter(|w| w == b"OggS").count(), 3);
        let skip = s.enc.pre_skip() as u64;
        let end = s.finish();
        // The last page ends the stream, and its granule is the real input
        // (3 frames of 320 samples, x3 to 48 kHz) plus pre_skip.
        let last = end.windows(4).rposition(|w| w == b"OggS").unwrap();
        assert_eq!(end[last + 5] & 0x04, 0x04);
        let granule = u64::from_le_bytes(end[last + 6..last + 14].try_into().unwrap());
        assert_eq!(granule, (FRAME as u64 * 3) * 3 + skip);
    }
}
