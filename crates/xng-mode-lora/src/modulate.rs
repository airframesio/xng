//! LoRa modulator, used to generate test signals for the receiver.

use crate::coding::{encode, CodingParams};
use num_complex::Complex;
use std::f64::consts::PI;

/// A full LoRa frame description.
#[derive(Debug, Clone, Copy)]
pub struct TxParams {
    pub coding: CodingParams,
    pub bw_hz: f64,
    /// Programmed preamble length (upchirps), before the sync word.
    pub preamble: usize,
    /// One-byte sync word (0x2B Meshtastic, 0x12 MeshCore/private, 0x34 LoRaWAN).
    pub sync_word: u8,
}

/// Generates continuous-phase chirps at `oversample` samples per chip.
struct ChirpGen {
    n: usize,
    os: usize,
    phase: f64,
    /// Carrier offset in cycles per chip.
    cfo: f64,
    /// Chips advanced per output sample (1/os, scaled by the clock error).
    dt: f64,
    /// Time into the current chirp of the next sample, in chips.
    t: f64,
    /// The previous chirp ended between two samples; the next one owes the
    /// phase from its start to the first sample.
    carry: bool,
    out: Vec<Complex<f32>>,
}

impl ChirpGen {
    /// Cycles accumulated `t` chips into a chirp starting at value `k`: the
    /// exact integral of the instantaneous frequency, so a frequency wrap
    /// between two samples is placed where it really falls.
    fn cycles(&self, k: u32, up: bool, t: f64) -> f64 {
        let n = self.n as f64;
        let base = (self.cfo - 0.5) * t;
        if !up {
            return base + t - t * t / (2.0 * n);
        }
        let k = k as f64;
        let wrap = n - k;
        if t <= wrap {
            base + (k * t + t * t / 2.0) / n
        } else {
            let r = t - wrap;
            base + (k * wrap + wrap * wrap / 2.0 + r * r / 2.0) / n
        }
    }

    /// Emit `chips` chips of a chirp starting at value `k`. `up` selects the
    /// direction.
    fn chirp(&mut self, k: u32, up: bool, chips: f64) {
        if self.carry {
            self.phase = (self.phase + 2.0 * PI * self.cycles(k, up, self.t)) % (2.0 * PI);
            self.carry = false;
        }
        while self.t < chips {
            self.out.push(Complex::new(
                self.phase.cos() as f32,
                self.phase.sin() as f32,
            ));
            let next = self.t + self.dt;
            let end = next.min(chips);
            let dc = self.cycles(k, up, end) - self.cycles(k, up, self.t);
            self.phase = (self.phase + 2.0 * PI * dc) % (2.0 * PI);
            self.carry = next > chips;
            self.t = next;
        }
        self.t -= chips;
    }
}

/// Modulate one frame to complex baseband at `bw_hz * oversample` S/s, with
/// a carrier offset of `cfo_hz` and a transmitter clock error of
/// `clock_ppm` (real radios derive both from one crystal).
pub fn modulate(
    payload: &[u8],
    p: &TxParams,
    oversample: usize,
    cfo_hz: f64,
    clock_ppm: f64,
) -> Vec<Complex<f32>> {
    let sf = p.coding.sf as usize;
    let n = 1usize << sf;
    let mut g = ChirpGen {
        n,
        os: oversample,
        phase: 0.0,
        cfo: cfo_hz / p.bw_hz,
        dt: (1.0 + clock_ppm * 1e-6) / oversample as f64,
        t: 0.0,
        carry: false,
        out: Vec::new(),
    };
    let _ = g.os;
    for _ in 0..p.preamble {
        g.chirp(0, true, n as f64);
    }
    g.chirp(((p.sync_word >> 4) as u32) * 8, true, n as f64);
    g.chirp(((p.sync_word & 0x0F) as u32) * 8, true, n as f64);
    g.chirp(0, false, n as f64);
    g.chirp(0, false, n as f64);
    g.chirp(0, false, n as f64 / 4.0);
    for c in encode(payload, &p.coding) {
        g.chirp(c, true, n as f64);
    }
    g.out
}
