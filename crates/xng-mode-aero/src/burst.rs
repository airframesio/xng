//! Aero R/T-channel burst decoding (C-band feeder side; ported from
//! JAERO `burstmskdemodulator.cpp` + `aerol.h` RTChannelDeleaveFECScram).
//!
//! Burst layout: unmodulated carrier section → alternating 1010 section →
//! data starting with the UW (within ~250 bits).
//! After the UW: one 64×5 interleaver section (→ 20 decoded bytes), then
//! 64×3 sections (→ 12 bytes each). The first section holds either one
//! 19-byte R-channel SU or a 6-byte T-burst header + the first 12-byte
//! P-style SU; T bursts continue with more SUs.

use crate::demod::MskDemod;
use crate::frame::UW;
use crate::su;
use num_complex::Complex;
use xng_dsp::checksum::HDLC_FCS;
use xng_dsp::scramble::Lfsr15;
use xng_dsp::viterbi::Viterbi;

const UW_TOLERANCE: u32 = 4;
/// UW must appear within this many bits of burst start.
const UW_SEARCH_BITS: usize = 300;
const SECTION1_CODED: usize = 64 * 5; // 320 coded → 20 bytes
const GROUP_CODED: usize = 64 * 3; // 192 coded → 12 bytes

/// Deinterleave one 64×cols section (same row order as the P channel).
fn deinterleave(soft: &[f32], cols: usize, out: &mut Vec<f32>) {
    for j in 0..cols {
        for i in 0..64 {
            out.push(soft[((27 * i) % 64) * cols + j]);
        }
    }
}

/// Interleave one section (transmit side, used by loopback tests).
pub fn interleave(bits: &[u8], cols: usize, out: &mut Vec<u8>) {
    let mut block = vec![0u8; 64 * cols];
    for (k, &b) in bits.iter().enumerate() {
        block[((27 * (k % 64)) % 64) * cols + k / 64] = b;
    }
    out.extend_from_slice(&block);
}

/// One decoded R or T burst.
pub struct BurstResult {
    /// Completed user-data units (T bursts feed the P-style reassembler,
    /// R bursts the R-channel reassembler).
    pub users: Vec<su::AeroUserData>,
    /// Named control/signalling SUs decoded from this burst (R-channel
    /// access-request / call-progress / telephony-ack / RQA / ACK etc.,
    /// or T-burst P-style control SUs) — see [`su::parse_r_su`] /
    /// [`su::parse_p_su`].
    pub su_events: Vec<serde_json::Value>,
    pub is_t: bool,
}

/// Packet layer shared by both burst rates.
pub struct BurstPacketizer {
    viterbi: Viterbi,
    t_reasm: su::Reassembler,
    r_reasm: su::RIsuReassembler,
}

impl BurstPacketizer {
    pub fn new() -> Self {
        Self {
            viterbi: Viterbi::k7(),
            t_reasm: su::Reassembler::new(),
            r_reasm: su::RIsuReassembler::new(),
        }
    }

    /// Process one demodulated burst bit stream (soft, hard).
    pub fn process(&mut self, bits: &[(f32, u8)]) -> Option<BurstResult> {
        // UW hunt (the discriminator demod has no polarity ambiguity).
        let mut shift: u32 = 0;
        let mut uw_end = None;
        for (i, &(_, hard)) in bits.iter().enumerate().take(UW_SEARCH_BITS + 32) {
            shift = (shift << 1) | hard as u32;
            if i >= 31 && (shift ^ UW).count_ones() <= UW_TOLERANCE {
                uw_end = Some(i + 1);
                break;
            }
        }
        let start = uw_end?;
        let coded: Vec<f32> = bits[start..].iter().map(|&(s, _)| s).collect();
        if coded.len() < SECTION1_CODED {
            return None;
        }

        // Deinterleave: one 5-col section, then as many 3-col groups as fit.
        let mut deleaved = Vec::with_capacity(coded.len());
        deinterleave(&coded[..SECTION1_CODED], 5, &mut deleaved);
        let mut off = SECTION1_CODED;
        while off + GROUP_CODED <= coded.len() {
            deinterleave(&coded[off..off + GROUP_CODED], 3, &mut deleaved);
            off += GROUP_CODED;
        }

        let mut decoded = self.viterbi.decode(&deleaved);
        Lfsr15::new().apply(&mut decoded);
        let bytes: Vec<u8> = decoded
            .chunks_exact(8)
            .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << i)))
            .collect();

        // T burst: 6-byte header (AES 3 + GES 1 + CRC 2)?
        if bytes.len() >= 6 && HDLC_FCS.checksum(&bytes[..4]) == u16::from_le_bytes([bytes[4], bytes[5]]) {
            let mut users = Vec::new();
            let mut su_events = Vec::new();
            let mut p = 6;
            while p + su::SU_LEN <= bytes.len() {
                let su_bytes = &bytes[p..p + su::SU_LEN];
                if !su::su_crc_ok(su_bytes) {
                    break;
                }
                if let Some(a) = su::parse_p_su(su_bytes) {
                    su_events.push(a);
                }
                if let Some(u) = self.t_reasm.push(su_bytes) {
                    users.push(u);
                }
                p += su::SU_LEN;
            }
            return Some(BurstResult { users, su_events, is_t: true });
        }

        // R burst: one 19-byte SU.
        if bytes.len() >= su::R_SU_LEN {
            let su_bytes = &bytes[..su::R_SU_LEN];
            if su::r_su_crc_ok(su_bytes) {
                let mut su_events = Vec::new();
                if let Some(a) = su::parse_r_su(su_bytes) {
                    su_events.push(a);
                }
                let users = self.r_reasm.push(su_bytes).into_iter().collect();
                return Some(BurstResult { users, su_events, is_t: false });
            }
        }
        None
    }
}

impl Default for BurstPacketizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Burst gate: collects samples while energy is present, then hands the
/// whole burst to a fresh demod pass (CFO measured from the carrier
/// section, timing locked on the alternating section).
pub struct BurstGate {
    noise: f32,
    power_ma: f32,
    /// Slow average of the in-burst power; burst end is detected relative
    /// to this (10 dB drop), independent of the noise-floor estimate.
    burst_power: f32,
    active: Option<Vec<Complex<f32>>>,
    quiet: u32,
    max_samples: usize,
}

impl BurstGate {
    pub fn new(max_samples: usize) -> Self {
        Self { noise: 1e-6, power_ma: 0.0, burst_power: 0.0, active: None, quiet: 0, max_samples }
    }

    /// Push samples; returns completed bursts.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vec<Complex<f32>>> {
        let mut out = Vec::new();
        for &x in input {
            let p = x.norm_sqr();
            self.power_ma += 0.2 * (p - self.power_ma);
            match &mut self.active {
                None => {
                    self.noise += 1e-4 * (p - self.noise);
                    if self.power_ma > self.noise * 8.0 {
                        self.active = Some(Vec::with_capacity(4096));
                        self.burst_power = self.power_ma;
                        self.quiet = 0;
                    }
                }
                Some(buf) => {
                    buf.push(x);
                    self.burst_power += 0.01 * (self.power_ma - self.burst_power).max(0.0);
                    if self.power_ma < self.burst_power * 0.1 {
                        self.quiet += 1;
                        if self.quiet > 256 {
                            out.push(self.active.take().unwrap());
                        }
                    } else {
                        self.quiet = 0;
                    }
                    if let Some(buf) = &self.active {
                        if buf.len() > self.max_samples {
                            out.push(self.active.take().unwrap());
                        }
                    }
                }
            }
        }
        out
    }
}

/// Demodulate one collected burst at a fixed bit rate: estimate the CFO
/// from the leading carrier section, then run the discriminator demod.
pub fn demod_burst(samples: &[Complex<f32>], channel_rate: f64, bit_rate: f64) -> Vec<(f32, u8)> {
    let spb = channel_rate / bit_rate;
    let window = (30.0 * spb) as usize;
    if samples.len() < window + 16 {
        return Vec::new();
    }

    // The gate may include leading noise (cold-start): locate the actual
    // signal by power — first point reaching half the burst's smoothed
    // peak — and measure the CFO on the carrier section right after it.
    let mut ma = 0.0f32;
    let mut smoothed = Vec::with_capacity(samples.len());
    for x in samples {
        ma += 0.1 * (x.norm_sqr() - ma);
        smoothed.push(ma);
    }
    let peak = smoothed.iter().cloned().fold(0.0f32, f32::max);
    let start = smoothed.iter().position(|&p| p > 0.5 * peak).unwrap_or(0);
    let skip = (start + (2.0 * spb) as usize).min(samples.len().saturating_sub(window));

    let mut sum = Complex::new(0.0f32, 0.0);
    for w in samples[skip..skip + window].windows(2) {
        sum += w[1] * w[0].conj();
    }
    let cfo = sum.arg(); // radians per sample

    // Mix down by the CFO and demod from just before the signal start;
    // pad the tail so the demod's filter flushes the final bits through.
    let from = start.saturating_sub((2.0 * spb) as usize);
    let mut shifted: Vec<Complex<f32>> = Vec::with_capacity(samples.len() - from + 256);
    let mut phase = 0.0f32;
    for &x in &samples[from..] {
        shifted.push(x * Complex::from_polar(1.0, -phase));
        phase += cfo;
    }
    shifted.extend(std::iter::repeat(Complex::new(0.0, 0.0)).take(256));
    let mut demod = MskDemod::new(channel_rate, bit_rate);
    let mut bits = Vec::new();
    demod.process(&shifted, &mut bits);
    bits
}
