//! Native airband voice core for xng.
//!
//! Airband is not a data mode: the product of a channel is audio, cut into
//! transmissions by a squelch. [`AirbandChannelDecoder`] turns capture IQ for
//! one channel into [`AirbandEvent`]s:
//!
//! - `Start` when the squelch opens (with signal and noise levels),
//! - `Audio` blocks of demodulated audio at [`CHANNEL_RATE`] while it is open
//!   (including a short pre-roll so the first syllable is kept, and the hang
//!   tail),
//! - `End` when it closes (with duration, peak and mean level).
//!
//! The chain per channel is: DDC to 16 kHz → optional AFC → AM envelope (with
//! carrier-normalised level, so loud and weak stations come out at the same
//! loudness) or NFM discriminator (+ de-emphasis) → voice band-pass, optional
//! notch → gain. The squelch is measured on 10 ms blocks of channel power;
//! see [`squelch`]. NFM channels can also require a CTCSS tone ([`ctcss`]).
//!
//! [`scan::ScanController`] is the scanner logic the runtime uses to build a
//! single "scan" stream that follows the active channel. [`codec`] holds the
//! PCM/µ-law/WAV/Ogg helpers that need no external library.
//!
//! VERIFICATION POSTURE: everything here is checked against self-generated
//! signals (`modulate`): AM and NFM bursts in white noise, at a channel offset
//! inside a wider capture, with squelch timing, tone recovery, CTCSS gating
//! and AFC pull-in asserted. The design is clean-room: RTLSDR-Airband and
//! airband-hub (both GPL) were read for the feature set and behaviour only.

pub mod codec;
pub mod ctcss;
pub mod filters;
pub mod modulate;
pub mod scan;
pub mod squelch;

use ctcss::CtcssGate;
use filters::{Biquad, Deemphasis};
use num_complex::Complex;
use serde::{Deserialize, Serialize};
use squelch::{Squelch, SquelchMode, Transition};
use std::collections::VecDeque;
use std::f64::consts::TAU;
use xng_dsp::{lowpass_taps, ChannelizedDdc, Ddc, Fir, SharedDdc};
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Audio and channel sample rate (Hz). Airband voice has no content above
/// ~3.5 kHz; 16 kHz is also what speech recognisers (Whisper) take and the
/// native wideband rate of Opus.
pub const CHANNEL_RATE: f64 = 16_000.0;
/// Widest one-sided channel passband the core will configure; the runtime
/// uses it to check that a channel fits inside the capture.
pub const CHANNEL_PASSBAND_HZ: f64 = 6_250.0;
/// Squelch block length (seconds).
pub const BLOCK_S: f64 = 0.010;
const BLOCK_LEN: usize = (CHANNEL_RATE * BLOCK_S) as usize;
/// One-sided passband of the shared multichannel front end (Hz).
const FRONT_PASSBAND_HZ: f64 = 7_000.0;
/// AFC correction limit per 10 ms block (Hz): pulls in 2.5 kHz in 1 s.
const AFC_MAX_STEP_HZ: f64 = 25.0;

/// Voice modulation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modulation {
    /// Double-sideband AM (VHF/UHF airband, HF AM).
    #[default]
    Am,
    /// Narrowband FM (land mobile, some military/ground ops, marine).
    Nfm,
}

impl std::str::FromStr for Modulation {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "am" => Ok(Self::Am),
            "nfm" | "fm" => Ok(Self::Nfm),
            other => Err(format!("unknown modulation {other:?} (am|nfm)")),
        }
    }
}

impl std::fmt::Display for Modulation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Am => "am",
            Self::Nfm => "nfm",
        })
    }
}

/// Per-channel settings. Every field has a sensible airband default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelConfig {
    pub modulation: Modulation,
    /// Two-sided RF filter width (Hz). Default 8 kHz for AM (fits 8.33 kHz
    /// spacing with margin for carrier offset), 12.5 kHz for NFM.
    pub bandwidth_hz: Option<f64>,
    pub squelch: SquelchMode,
    /// Signal must persist this long before the squelch opens (ms).
    pub open_ms: u32,
    /// Squelch stays open this long after the signal drops (ms).
    pub hang_ms: u32,
    /// CTCSS tone required to open (NFM), Hz.
    pub ctcss_hz: Option<f32>,
    /// Notch out a fixed audio tone (Hz), e.g. a heterodyne.
    pub notch_hz: Option<f32>,
    /// Notch Q (default 10).
    pub notch_q: f32,
    /// Voice high-pass corner (Hz); 0 disables.
    pub highpass_hz: f32,
    /// Voice low-pass corner (Hz); 0 disables.
    pub lowpass_hz: f32,
    /// Output gain (dB).
    pub gain_db: f32,
    /// Normalise AM audio by the carrier level (constant loudness).
    pub agc: bool,
    /// Follow carrier offset up to this many Hz while a signal is present;
    /// 0 disables.
    pub afc_max_hz: f32,
    /// NFM de-emphasis time constant (µs); 0 disables.
    pub tau_us: f32,
    /// NFM deviation that maps to full-scale audio (Hz).
    pub nfm_deviation_hz: f32,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            modulation: Modulation::Am,
            bandwidth_hz: None,
            squelch: SquelchMode::default(),
            open_ms: 20,
            hang_ms: 250,
            ctcss_hz: None,
            notch_hz: None,
            notch_q: 10.0,
            highpass_hz: 150.0,
            lowpass_hz: 3_400.0,
            gain_db: 0.0,
            agc: true,
            afc_max_hz: 0.0,
            tau_us: 200.0,
            nfm_deviation_hz: 2_500.0,
        }
    }
}

impl ChannelConfig {
    /// Effective two-sided RF bandwidth.
    pub fn bandwidth(&self) -> f64 {
        self.bandwidth_hz.unwrap_or(match self.modulation {
            Modulation::Am => 8_000.0,
            Modulation::Nfm => 12_500.0,
        })
    }

    /// Validate ranges; returns a readable error.
    pub fn validate(&self) -> Result<(), String> {
        let bw = self.bandwidth();
        if !(1_000.0..=2.0 * CHANNEL_PASSBAND_HZ).contains(&bw) {
            return Err(format!(
                "bandwidth {bw} Hz out of range (1000..={} Hz)",
                2.0 * CHANNEL_PASSBAND_HZ
            ));
        }
        if let Some(t) = self.ctcss_hz {
            if !(60.0..=260.0).contains(&t) {
                return Err(format!("ctcss tone {t} Hz out of range (60..=260)"));
            }
        }
        if let Some(n) = self.notch_hz {
            if !(10.0..CHANNEL_RATE as f32 / 2.0).contains(&n) {
                return Err(format!("notch {n} Hz out of range"));
            }
        }
        if self.lowpass_hz > 0.0 && self.lowpass_hz >= CHANNEL_RATE as f32 / 2.0 {
            return Err(format!("lowpass {} Hz must be below {} Hz", self.lowpass_hz, CHANNEL_RATE / 2.0));
        }
        Ok(())
    }
}

/// Output of [`AirbandChannelDecoder::process`]. `sample` positions count
/// channel-rate samples since the decoder was created.
#[derive(Clone, Debug, PartialEq)]
pub enum AirbandEvent {
    Start {
        sample: u64,
        level_dbfs: f32,
        noise_dbfs: f32,
    },
    Audio {
        sample: u64,
        samples: Vec<f32>,
    },
    End {
        sample: u64,
        /// From squelch open to the last block above the close threshold
        /// (hang time excluded).
        duration_s: f64,
        peak_dbfs: f32,
        mean_dbfs: f32,
        noise_dbfs: f32,
    },
}

/// One airband voice channel.
pub struct AirbandChannelDecoder {
    cfg: ChannelConfig,
    ddc: Option<Ddc>,
    /// Extra channel-rate selectivity when a shared front end is wider than
    /// this channel's bandwidth (e.g. an 8.33 kHz channel among 25 kHz ones).
    chan_fir: Option<Fir>,
    chan: Vec<Complex<f32>>,
    filtered: Vec<Complex<f32>>,

    // AFC
    afc_hz: f64,
    afc_phase: f64,
    afc_acc: Complex<f32>,

    // demod state
    prev: Complex<f32>,
    carrier: f32,
    carrier_alpha: f32,
    /// Short-term envelope (5 ms) used to snap the carrier estimate up at
    /// the start of a transmission.
    env_fast: f32,
    env_fast_alpha: f32,
    deemph: Option<Deemphasis>,
    hp: Option<[Biquad; 2]>,
    lp: Option<[Biquad; 2]>,
    notch: Option<Biquad>,
    gain: f32,

    // squelch
    squelch: Squelch,
    ctcss: Option<CtcssGate>,
    block_power: f64,
    block_raw: Vec<f32>,
    block_audio: Vec<f32>,
    preroll: VecDeque<Vec<f32>>,
    pos: u64,
    level_db: f32,

    // current transmission
    tx_start: u64,
    tx_last_loud: u64,
    tx_peak: f32,
    tx_sum_db: f64,
    tx_blocks: u32,

    // counters
    transmissions: u64,
    airtime_s: f64,
    /// Squelch `Off`: the one continuous "transmission" has been announced.
    continuous_started: bool,
}

impl AirbandChannelDecoder {
    /// `input_rate` is the capture rate (≥ 16 kHz, any ratio); `offset_hz`
    /// the channel relative to the capture center.
    pub fn new(input_rate: f64, offset_hz: f64, cfg: ChannelConfig) -> Result<Self, String> {
        cfg.validate()?;
        let half_bw = cfg.bandwidth() / 2.0;
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, offset_hz, half_bw)?)
        };
        let rate = CHANNEL_RATE;
        let pair = |mk: &dyn Fn() -> Biquad| [mk(), mk()];
        let hp = (cfg.highpass_hz > 0.0)
            .then(|| pair(&|| Biquad::highpass(cfg.highpass_hz as f64, std::f64::consts::FRAC_1_SQRT_2, rate)));
        let lp = (cfg.lowpass_hz > 0.0)
            .then(|| pair(&|| Biquad::lowpass(cfg.lowpass_hz as f64, std::f64::consts::FRAC_1_SQRT_2, rate)));
        let notch = cfg.notch_hz.map(|f| Biquad::notch(f as f64, cfg.notch_q.max(0.5) as f64, rate));
        let deemph = (cfg.modulation == Modulation::Nfm && cfg.tau_us > 0.0)
            .then(|| Deemphasis::new(cfg.tau_us as f64 * 1e-6, rate));
        let squelch = Squelch::new(
            cfg.squelch,
            BLOCK_S as f32,
            cfg.open_ms as f32 / 1_000.0,
            cfg.hang_ms as f32 / 1_000.0,
        );
        let ctcss = cfg.ctcss_hz.map(|t| CtcssGate::new(t, rate));
        Ok(Self {
            gain: 10f32.powf(cfg.gain_db / 20.0),
            cfg,
            ddc,
            chan_fir: None,
            chan: Vec::new(),
            filtered: Vec::new(),
            afc_hz: 0.0,
            afc_phase: 0.0,
            afc_acc: Complex::new(0.0, 0.0),
            prev: Complex::new(0.0, 0.0),
            carrier: 0.0,
            // 50 ms carrier tracker: well below the lowest voice frequency.
            carrier_alpha: (1.0 / (0.05 * rate)) as f32,
            env_fast: 0.0,
            env_fast_alpha: (1.0 / (0.005 * rate)) as f32,
            deemph,
            hp,
            lp,
            notch,
            squelch,
            ctcss,
            block_power: 0.0,
            block_raw: Vec::with_capacity(BLOCK_LEN),
            block_audio: Vec::with_capacity(BLOCK_LEN),
            preroll: VecDeque::new(),
            pos: 0,
            level_db: f32::NEG_INFINITY,
            tx_start: 0,
            tx_last_loud: 0,
            tx_peak: f32::NEG_INFINITY,
            tx_sum_db: 0.0,
            tx_blocks: 0,
            transmissions: 0,
            airtime_s: 0.0,
            continuous_started: false,
        })
    }

    /// A channel fed already-downconverted IQ at [`CHANNEL_RATE`] by a shared
    /// front end whose one-sided passband is `front_passband_hz`. Adds a
    /// channel-rate low-pass when this channel is narrower than the front.
    pub fn at_channel_rate(cfg: ChannelConfig, front_passband_hz: f64) -> Result<Self, String> {
        let mut d = Self::new(CHANNEL_RATE, 0.0, cfg)?;
        let half = d.cfg.bandwidth() / 2.0;
        if half + 250.0 < front_passband_hz {
            d.chan_fir = Some(Fir::new(lowpass_taps(half / CHANNEL_RATE, 95)));
        }
        Ok(d)
    }

    pub fn config(&self) -> &ChannelConfig {
        &self.cfg
    }

    /// Smoothed channel power (dBFS).
    pub fn level_dbfs(&self) -> f32 {
        self.level_db
    }

    pub fn noise_dbfs(&self) -> f32 {
        self.squelch.noise_db()
    }

    pub fn squelch_dbfs(&self) -> f32 {
        self.squelch.threshold_db()
    }

    pub fn is_open(&self) -> bool {
        self.squelch.is_open()
    }

    /// Carrier offset currently tracked by AFC (Hz).
    pub fn afc_offset_hz(&self) -> f32 {
        self.afc_hz as f32
    }

    pub fn transmissions(&self) -> u64 {
        self.transmissions
    }

    pub fn airtime_s(&self) -> f64 {
        self.airtime_s
    }

    /// Channel samples processed so far.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Force-close the squelch and clear filter state (scanning receivers
    /// call this after a retune). Returns the `End` event if a transmission
    /// was open.
    pub fn retune_reset(&mut self) -> Option<AirbandEvent> {
        let end = self.squelch.is_open().then(|| self.end_event());
        self.squelch.reset();
        self.preroll.clear();
        self.block_raw.clear();
        self.block_audio.clear();
        self.block_power = 0.0;
        self.carrier = 0.0;
        self.env_fast = 0.0;
        self.afc_acc = Complex::new(0.0, 0.0);
        if let Some(c) = &mut self.ctcss {
            c.reset();
        }
        for f in self.hp.iter_mut().chain(self.lp.iter_mut()).flatten() {
            f.reset();
        }
        end
    }

    /// Seed / read back the noise floor (scan mode keeps one per frequency).
    pub fn set_noise_floor(&mut self, db: f32) {
        self.squelch.set_floor(db);
    }

    /// Feed capture IQ; returns the events it produced, in order.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AirbandEvent> {
        let mut chan = std::mem::take(&mut self.chan);
        chan.clear();
        let samples: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                ddc.process(input, &mut chan);
                &chan
            }
            None => input,
        };
        let events = self.process_baseband(samples);
        self.chan = chan;
        events
    }

    /// Feed IQ that is already at [`CHANNEL_RATE`] and centred on the channel
    /// (the shared front-end path).
    pub fn process_baseband(&mut self, samples: &[Complex<f32>]) -> Vec<AirbandEvent> {
        let mut filtered = std::mem::take(&mut self.filtered);
        let samples = match &mut self.chan_fir {
            Some(f) => {
                filtered.clear();
                f.process(samples, &mut filtered);
                &filtered[..]
            }
            None => samples,
        };
        let mut events = Vec::new();
        for &x0 in samples {
            self.sample(x0, &mut events);
        }
        self.filtered = filtered;
        events
    }

    #[inline]
    fn sample(&mut self, x0: Complex<f32>, events: &mut Vec<AirbandEvent>) {
        // AFC: rotate by the tracked carrier offset.
        let x = if self.afc_hz != 0.0 {
            self.afc_phase = (self.afc_phase - TAU * self.afc_hz / CHANNEL_RATE) % TAU;
            let (s, c) = self.afc_phase.sin_cos();
            x0 * Complex::new(c as f32, s as f32)
        } else {
            x0
        };
        let p = x.norm_sqr();
        self.block_power += p as f64;
        let dphi = x * self.prev.conj();
        self.afc_acc += dphi;
        self.prev = x;

        let raw = match self.cfg.modulation {
            Modulation::Am => {
                let env = p.sqrt();
                self.env_fast += self.env_fast_alpha * (env - self.env_fast);
                self.carrier += self.carrier_alpha * (env - self.carrier);
                // Fast attack, slow release: a carrier that just keyed up is
                // followed within a few ms instead of a 50 ms climb.
                if self.env_fast > self.carrier {
                    self.carrier += 0.2 * (self.env_fast - self.carrier);
                }
                let ac = env - self.carrier;
                if self.cfg.agc {
                    // Never divide by less than twice the noise amplitude, so
                    // a quiet channel is not amplified into hiss.
                    let floor_amp = 2.0 * db_to_amp(self.squelch.noise_db());
                    ac / self.carrier.max(floor_amp).max(1e-6)
                } else {
                    ac * 4.0
                }
            }
            Modulation::Nfm => {
                let d = dphi.arg() * (CHANNEL_RATE as f32) / (std::f32::consts::TAU * self.cfg.nfm_deviation_hz);
                match &mut self.deemph {
                    Some(de) => de.process(d),
                    None => d,
                }
            }
        };
        self.block_raw.push(raw);

        let mut y = raw;
        if let Some(hp) = &mut self.hp {
            let t = hp[0].process(y);
            y = hp[1].process(t);
        }
        if let Some(lp) = &mut self.lp {
            let t = lp[0].process(y);
            y = lp[1].process(t);
        }
        if let Some(n) = &mut self.notch {
            y = n.process(y);
        }
        y = (0.5 * y * self.gain).clamp(-1.0, 1.0);
        self.block_audio.push(y);

        self.pos += 1;
        if self.block_audio.len() >= BLOCK_LEN {
            self.end_block(events);
        }
    }

    fn end_block(&mut self, events: &mut Vec<AirbandEvent>) {
        let n = self.block_audio.len();
        let power_db = 10.0 * ((self.block_power / n as f64).max(1e-20)).log10() as f32;
        self.block_power = 0.0;
        self.level_db = if self.level_db.is_finite() {
            self.level_db + 0.3 * (power_db - self.level_db)
        } else {
            power_db
        };
        let tone_ok = match &mut self.ctcss {
            Some(g) => g.push(&self.block_raw),
            None => true,
        };
        self.block_raw.clear();
        let block_start = self.pos - n as u64;
        let block = std::mem::replace(&mut self.block_audio, Vec::with_capacity(BLOCK_LEN));

        // AFC update on blocks that carry signal (a block where the carrier
        // dropped part-way is noise-dominated and would pull AFC off).
        if self.cfg.afc_max_hz > 0.0
            && self.squelch.is_open()
            && !self.squelch.in_hang()
            && power_db >= self.squelch.threshold_db()
            && (self.tx_blocks == 0 || power_db as f64 >= self.tx_sum_db / self.tx_blocks as f64 - 2.0)
        {
            let err = self.afc_acc.arg() as f64 * CHANNEL_RATE / TAU;
            let max = self.cfg.afc_max_hz as f64;
            // Slew-limited so one bad block cannot throw the loop far off.
            self.afc_hz = (self.afc_hz + (0.3 * err).clamp(-AFC_MAX_STEP_HZ, AFC_MAX_STEP_HZ)).clamp(-max, max);
        }
        self.afc_acc = Complex::new(0.0, 0.0);

        if matches!(self.cfg.squelch, SquelchMode::Off) && !self.continuous_started {
            // Continuous audio: announce one open-ended transmission.
            self.continuous_started = true;
            self.tx_start = block_start;
            self.tx_peak = power_db;
            events.push(AirbandEvent::Start {
                sample: block_start,
                level_dbfs: power_db,
                noise_dbfs: self.squelch.noise_db(),
            });
        }
        match self.squelch.update(power_db, tone_ok) {
            Transition::Opened => {
                let pre: Vec<Vec<f32>> = self.preroll.drain(..).collect();
                let pre_len: usize = pre.iter().map(|b| b.len()).sum();
                let start = block_start - pre_len as u64;
                self.tx_start = start;
                self.tx_last_loud = self.pos;
                self.tx_peak = power_db;
                self.tx_sum_db = power_db as f64;
                self.tx_blocks = 1;
                events.push(AirbandEvent::Start {
                    sample: start,
                    level_dbfs: power_db,
                    noise_dbfs: self.squelch.noise_db(),
                });
                let first = events.len();
                for b in pre {
                    push_audio(events, start, b);
                }
                push_audio(events, block_start, block);
                fade_in(&mut events[first..]);
            }
            Transition::Closed => {
                let e = self.end_event();
                events.push(e);
                self.remember_preroll(block);
            }
            Transition::None => {
                if self.squelch.is_open() {
                    if !self.squelch.in_hang() {
                        self.tx_last_loud = self.pos;
                        self.tx_peak = self.tx_peak.max(power_db);
                        self.tx_sum_db += power_db as f64;
                        self.tx_blocks += 1;
                    }
                    push_audio(events, block_start, block);
                } else {
                    self.remember_preroll(block);
                }
            }
        }
    }

    fn remember_preroll(&mut self, block: Vec<f32>) {
        // Keep as many blocks as it takes the squelch to open, minus the one
        // that will be passed directly.
        let keep = self.squelch.open_blocks().saturating_sub(1) as usize;
        if keep == 0 {
            self.preroll.clear();
            return;
        }
        self.preroll.push_back(block);
        while self.preroll.len() > keep {
            self.preroll.pop_front();
        }
    }

    fn end_event(&mut self) -> AirbandEvent {
        let duration_s = (self.tx_last_loud.saturating_sub(self.tx_start)) as f64 / CHANNEL_RATE;
        self.transmissions += 1;
        self.airtime_s += duration_s;
        AirbandEvent::End {
            sample: self.pos,
            duration_s,
            peak_dbfs: self.tx_peak,
            mean_dbfs: if self.tx_blocks > 0 { (self.tx_sum_db / self.tx_blocks as f64) as f32 } else { self.tx_peak },
            noise_dbfs: self.squelch.noise_db(),
        }
    }
}

/// Shared multi-channel front end (same contract as the ACARS one): a
/// polyphase channelizer when the channel set allows it, else a shared
/// decimation, else (one channel) a plain DDC inside the channel decoder.
enum Front {
    Channelized(ChannelizedDdc),
    Shared(SharedDdc),
    PerChannel,
}

/// Several airband channels out of one capture. The wideband work is done
/// once for every channel (the rtl_airband "multichannel" case), then each
/// channel runs its own demod and squelch.
pub struct AirbandMultiChannelDecoder {
    front: Front,
    chans: Vec<AirbandChannelDecoder>,
    bufs: Vec<Vec<Complex<f32>>>,
}

impl AirbandMultiChannelDecoder {
    /// `offsets[i]` is channel `i` relative to the capture center; `cfgs[i]`
    /// its settings.
    pub fn new(input_rate: f64, offsets: &[f64], cfgs: Vec<ChannelConfig>) -> Result<Self, String> {
        if offsets.is_empty() || offsets.len() != cfgs.len() {
            return Err("need one config per channel".into());
        }
        for c in &cfgs {
            c.validate()?;
        }
        if offsets.len() == 1 {
            let chans = vec![AirbandChannelDecoder::new(input_rate, offsets[0], cfgs[0].clone())?];
            return Ok(Self { front: Front::PerChannel, chans, bufs: vec![Vec::new()] });
        }
        // The front end keeps ±7 kHz around each channel (bins and decimation
        // sized for that), so nothing outside the 16 kHz channel rate can
        // alias in; each channel's own low-pass then sets the selectivity.
        // A narrower front lets a strong neighbour 8.33 kHz away fold back.
        let passband = FRONT_PASSBAND_HZ;
        let front = match ChannelizedDdc::new(input_rate, CHANNEL_RATE, offsets, passband) {
            Ok(c) => Front::Channelized(c),
            Err(chan_err) => Front::Shared(SharedDdc::new(input_rate, CHANNEL_RATE, offsets, passband).map_err(
                |shared_err| format!("channelizer rejected ({chan_err}); shared-decim also failed ({shared_err})"),
            )?),
        };
        let chans = cfgs
            .into_iter()
            .map(|c| AirbandChannelDecoder::at_channel_rate(c, passband))
            .collect::<Result<Vec<_>, _>>()?;
        let bufs = offsets.iter().map(|_| Vec::new()).collect();
        Ok(Self { front, chans, bufs })
    }

    pub fn num_channels(&self) -> usize {
        self.chans.len()
    }

    pub fn channel(&self, i: usize) -> &AirbandChannelDecoder {
        &self.chans[i]
    }

    pub fn channel_mut(&mut self, i: usize) -> &mut AirbandChannelDecoder {
        &mut self.chans[i]
    }

    /// Feed capture IQ; returns each channel's events (index-aligned).
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vec<AirbandEvent>> {
        match &mut self.front {
            Front::PerChannel => vec![self.chans[0].process(input)],
            Front::Channelized(c) => {
                c.process(input, &mut self.bufs);
                self.chans.iter_mut().zip(&self.bufs).map(|(d, b)| d.process_baseband(b)).collect()
            }
            Front::Shared(s) => {
                s.process(input, &mut self.bufs);
                self.chans.iter_mut().zip(&self.bufs).map(|(d, b)| d.process_baseband(b)).collect()
            }
        }
    }
}

fn db_to_amp(db: f32) -> f32 {
    if db.is_finite() {
        10f32.powf(db / 20.0)
    } else {
        0.0
    }
}

/// Key-up fade (samples): the carrier step at the start of a transmission
/// is an audible thump however fast the demodulator settles, so the first
/// 20 ms of each transmission are faded in.
const FADE_IN: usize = (CHANNEL_RATE * 0.020) as usize;

fn fade_in(events: &mut [AirbandEvent]) {
    let mut k = 0usize;
    for e in events {
        if let AirbandEvent::Audio { samples, .. } = e {
            for v in samples.iter_mut() {
                if k >= FADE_IN {
                    return;
                }
                let g = 0.5 - 0.5 * (std::f32::consts::PI * k as f32 / FADE_IN as f32).cos();
                *v *= g;
                k += 1;
            }
        }
    }
}

/// Append audio, merging with the previous Audio event when contiguous.
fn push_audio(events: &mut Vec<AirbandEvent>, sample: u64, block: Vec<f32>) {
    if let Some(AirbandEvent::Audio { sample: s, samples }) = events.last_mut() {
        if *s + samples.len() as u64 == sample {
            samples.extend_from_slice(&block);
            return;
        }
    }
    events.push(AirbandEvent::Audio { sample, samples: block });
}

/// Fields describing one transmission, for the bus message.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TransmissionInfo {
    /// Per-channel transmission counter.
    pub tx: u64,
    pub label: Option<String>,
    pub modulation: Modulation,
    /// ISO-8601 start time.
    pub start: String,
    /// ISO-8601 end time (end events only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    pub level_dbfs: f32,
    pub noise_dbfs: f32,
    /// ATC frequency key this channel is mapped to, if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atc_frequency: Option<String>,
}

/// Normalise a transmission start/end onto the bus. `kind` is `"tx_start"`
/// or `"tx_end"`.
pub fn to_message(kind: &str, info: &TransmissionInfo, freq_hz: u64, source: Provenance) -> Message {
    let snr = if info.noise_dbfs.is_finite() && info.level_dbfs.is_finite() {
        Some(info.level_dbfs - info.noise_dbfs)
    } else {
        None
    };
    Message {
        mode: Mode::Airband,
        timestamp: chrono::Utc::now(),
        frequency_hz: freq_hz,
        signal: SignalQuality {
            rssi_db: info.level_dbfs.is_finite().then_some(info.level_dbfs),
            snr_db: snr,
            noise_db: info.noise_dbfs.is_finite().then_some(info.noise_dbfs),
            ..Default::default()
        },
        decode: DecodeQuality { crc_ok: true, ..Default::default() },
        body: MessageBody::Airband {
            kind: kind.to_string(),
            details: serde_json::to_value(info).unwrap_or_default(),
        },
        raw: None,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modulate::{am_capture, nfm_capture, Burst};

    const RATE: f64 = 240_000.0;

    fn burst(start: f64, len: f64, amp: f64) -> Burst {
        Burst { start_s: start, len_s: len, amplitude: amp, tone_hz: 1_000.0, depth: 0.8, ctcss: None }
    }

    fn collect(events: &[AirbandEvent]) -> (Vec<(u64, u64)>, Vec<f32>) {
        // (start, end) sample pairs and the concatenated audio.
        let mut txs = Vec::new();
        let mut audio = Vec::new();
        let mut open = None;
        for e in events {
            match e {
                AirbandEvent::Start { sample, .. } => open = Some(*sample),
                AirbandEvent::Audio { samples, .. } => audio.extend_from_slice(samples),
                AirbandEvent::End { sample, .. } => {
                    txs.push((open.take().expect("end without start"), *sample));
                }
            }
        }
        (txs, audio)
    }

    fn run(dec: &mut AirbandChannelDecoder, iq: &[Complex<f32>]) -> Vec<AirbandEvent> {
        let mut out = Vec::new();
        for chunk in iq.chunks(24_000) {
            out.extend(dec.process(chunk));
        }
        out
    }

    /// Dominant frequency of `x` by a coarse DFT scan.
    fn peak_hz(x: &[f32], rate: f64) -> f64 {
        let mut best = (0.0, 0.0);
        for f in (200..3_400).step_by(25) {
            let mut g = filters::Goertzel::new(f as f64, rate);
            for &v in x {
                g.push(v);
            }
            if g.power() > best.1 {
                best = (f as f64, g.power());
            }
        }
        best.0
    }

    #[test]
    fn am_bursts_segment_and_demodulate() {
        let bursts = [burst(1.0, 1.5, 0.05), burst(3.5, 0.8, 0.02)];
        let iq = am_capture(RATE, 5.0, 25_000.0, &bursts, 1e-5, 7);
        let mut dec = AirbandChannelDecoder::new(RATE, 25_000.0, ChannelConfig::default()).unwrap();
        let ev = run(&mut dec, &iq);
        let (txs, audio) = collect(&ev);
        assert_eq!(txs.len(), 2, "{txs:?}");
        let s0 = txs[0].0 as f64 / CHANNEL_RATE;
        assert!((s0 - 1.0).abs() < 0.05, "first start {s0}");
        let e1 = txs[1].1 as f64 / CHANNEL_RATE;
        assert!((e1 - 4.3).abs() < 0.4, "second end {e1}");
        assert_eq!(dec.transmissions(), 2);
        assert!((dec.airtime_s() - 2.3).abs() < 0.15, "airtime {}", dec.airtime_s());
        // Demodulated tone is the modulating 1 kHz.
        let mid = &audio[4_000..12_000];
        assert!((peak_hz(mid, CHANNEL_RATE) - 1_000.0).abs() <= 25.0);
    }

    #[test]
    fn am_agc_levels_weak_and_strong_alike() {
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let mut levels = Vec::new();
        for amp in [0.2, 0.01] {
            let iq = am_capture(RATE, 2.0, -40_000.0, &[burst(0.5, 1.0, amp)], 1e-6, 3);
            let mut dec = AirbandChannelDecoder::new(RATE, -40_000.0, ChannelConfig::default()).unwrap();
            let (_, audio) = collect(&run(&mut dec, &iq));
            levels.push(rms(&audio[3_200..12_800]));
        }
        let ratio_db = 20.0 * (levels[0] / levels[1]).log10();
        assert!(ratio_db.abs() < 1.5, "levels {levels:?}");
    }

    #[test]
    fn no_thump_at_key_up() {
        // The first 20 ms of a transmission must not be louder than its body.
        let iq = am_capture(RATE, 2.0, 0.0, &[burst(0.5, 1.0, 0.05)], 1e-6, 13);
        let mut dec = AirbandChannelDecoder::new(RATE, 0.0, ChannelConfig::default()).unwrap();
        let (_, audio) = collect(&run(&mut dec, &iq));
        let peak = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs()));
        let head = peak(&audio[..320]);
        let body = peak(&audio[3_200..8_000]);
        assert!(head < 1.3 * body, "head {head} body {body}");
    }

    #[test]
    fn quiet_channel_never_opens() {
        let iq = am_capture(RATE, 3.0, 10_000.0, &[], 1e-5, 11);
        let mut dec = AirbandChannelDecoder::new(RATE, 10_000.0, ChannelConfig::default()).unwrap();
        assert!(run(&mut dec, &iq).is_empty());
        assert!((dec.noise_dbfs() - dec.level_dbfs()).abs() < 2.0);
    }

    #[test]
    fn adjacent_channel_does_not_open_neighbour() {
        // A strong carrier 25 kHz away must not open this channel.
        let iq = am_capture(RATE, 3.0, 25_000.0, &[burst(1.0, 1.0, 0.3)], 1e-5, 5);
        let mut dec = AirbandChannelDecoder::new(RATE, 0.0, ChannelConfig::default()).unwrap();
        let ev = run(&mut dec, &iq);
        assert!(collect(&ev).0.is_empty(), "{:?}", collect(&ev).0);
    }

    #[test]
    fn preroll_keeps_the_first_syllable() {
        let iq = am_capture(RATE, 2.0, 0.0, &[burst(0.5, 1.0, 0.05)], 1e-5, 9);
        let cfg = ChannelConfig { open_ms: 50, ..Default::default() };
        let mut dec = AirbandChannelDecoder::new(RATE, 0.0, cfg).unwrap();
        let ev = run(&mut dec, &iq);
        let AirbandEvent::Start { sample, .. } = ev[0] else { panic!("{:?}", ev[0]) };
        let AirbandEvent::Audio { sample: a0, .. } = &ev[1] else { panic!() };
        assert_eq!(sample, *a0);
        // Start is reported within one block of the real onset, not 50 ms late.
        assert!((sample as f64 / CHANNEL_RATE - 0.5).abs() <= 0.015, "{sample}");
    }

    #[test]
    fn nfm_demodulates_tone() {
        let b = Burst { start_s: 0.5, len_s: 1.0, amplitude: 0.05, tone_hz: 1_200.0, depth: 2_000.0, ctcss: None };
        let iq = nfm_capture(RATE, 2.0, 30_000.0, &[b], 1e-5, 2);
        let cfg = ChannelConfig { modulation: Modulation::Nfm, ..Default::default() };
        let mut dec = AirbandChannelDecoder::new(RATE, 30_000.0, cfg).unwrap();
        let (txs, audio) = collect(&run(&mut dec, &iq));
        assert_eq!(txs.len(), 1);
        assert!((peak_hz(&audio[3_200..12_800], CHANNEL_RATE) - 1_200.0).abs() <= 25.0);
    }

    #[test]
    fn ctcss_gate_selects_the_right_user() {
        let mk = |tone: f64, start: f64| Burst {
            start_s: start,
            len_s: 1.0,
            amplitude: 0.05,
            tone_hz: 1_000.0,
            depth: 2_000.0,
            ctcss: Some((tone, 500.0)),
        };
        // Wrong tone first, then the right one.
        let iq = nfm_capture(RATE, 4.0, 0.0, &[mk(123.0, 0.5), mk(100.0, 2.5)], 1e-5, 4);
        let cfg = ChannelConfig { modulation: Modulation::Nfm, ctcss_hz: Some(100.0), ..Default::default() };
        let mut dec = AirbandChannelDecoder::new(RATE, 0.0, cfg).unwrap();
        let (txs, _) = collect(&run(&mut dec, &iq));
        assert_eq!(txs.len(), 1, "{txs:?}");
        let start = txs[0].0 as f64 / CHANNEL_RATE;
        assert!(start > 2.5 && start < 3.5, "{start}");
    }

    #[test]
    fn afc_pulls_in_an_offset_carrier() {
        // Carrier 1.5 kHz off the nominal channel.
        let iq = am_capture(RATE, 3.0, 1_500.0, &[burst(0.5, 2.0, 0.05)], 1e-5, 6);
        let cfg = ChannelConfig { afc_max_hz: 2_500.0, ..Default::default() };
        let mut dec = AirbandChannelDecoder::new(RATE, 0.0, cfg).unwrap();
        let ev = run(&mut dec, &iq);
        assert_eq!(collect(&ev).0.len(), 1);
        assert!((dec.afc_offset_hz() - 1_500.0).abs() < 75.0, "afc {}", dec.afc_offset_hz());
    }

    #[test]
    fn squelch_off_streams_everything() {
        let iq = am_capture(RATE, 1.0, 0.0, &[], 1e-5, 1);
        let cfg = ChannelConfig { squelch: SquelchMode::Off, ..Default::default() };
        let mut dec = AirbandChannelDecoder::new(RATE, 0.0, cfg).unwrap();
        let ev = run(&mut dec, &iq);
        let n: usize = ev
            .iter()
            .map(|e| if let AirbandEvent::Audio { samples, .. } = e { samples.len() } else { 0 })
            .sum();
        assert_eq!(n as u64, dec.position() / BLOCK_LEN as u64 * BLOCK_LEN as u64);
    }

    #[test]
    fn multichannel_front_end_separates_channels() {
        // Three channels on a 25 kHz raster plus one 8.33 kHz channel; only
        // two carry traffic.
        let mut iq = am_capture(RATE, 3.0, -50_000.0, &[burst(0.5, 1.0, 0.05)], 1e-5, 21);
        let other = am_capture(RATE, 3.0, 25_000.0, &[burst(1.8, 0.7, 0.03)], 0.0, 22);
        for (a, b) in iq.iter_mut().zip(other) {
            *a += b;
        }
        let offsets = [-50_000.0, 0.0, 25_000.0, 33_333.0];
        let cfgs = vec![
            ChannelConfig::default(),
            ChannelConfig::default(),
            ChannelConfig::default(),
            ChannelConfig { bandwidth_hz: Some(5_000.0), ..Default::default() },
        ];
        let mut m = AirbandMultiChannelDecoder::new(RATE, &offsets, cfgs).unwrap();
        let mut per: Vec<Vec<AirbandEvent>> = vec![Vec::new(); 4];
        for chunk in iq.chunks(24_000) {
            for (i, ev) in m.process(chunk).into_iter().enumerate() {
                per[i].extend(ev);
            }
        }
        let txs: Vec<usize> = per.iter().map(|e| collect(e).0.len()).collect();
        assert_eq!(txs, vec![1, 0, 1, 0], "{txs:?}");
        let (t, _) = collect(&per[2]);
        let start = t[0].0 as f64 / CHANNEL_RATE;
        assert!((start - 1.8).abs() < 0.05, "{start}");
        assert_eq!(m.channel(0).transmissions(), 1);
    }

    #[test]
    fn rejects_bad_config() {
        let c = ChannelConfig { bandwidth_hz: Some(50_000.0), ..Default::default() };
        assert!(AirbandChannelDecoder::new(RATE, 0.0, c).is_err());
        let c = ChannelConfig { ctcss_hz: Some(20.0), ..Default::default() };
        assert!(c.validate().is_err());
    }

    #[test]
    fn config_toml_shape() {
        let c: ChannelConfig = serde_json::from_value(serde_json::json!({
            "modulation": "nfm",
            "squelch": {"type": "manual", "dbfs": -40.0},
            "ctcss_hz": 100.0
        }))
        .unwrap();
        assert_eq!(c.modulation, Modulation::Nfm);
        assert_eq!(c.squelch, SquelchMode::Manual { dbfs: -40.0 });
    }
}
