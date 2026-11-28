//! Native VOR / ILS receive core for xng.
//!
//! VOR, ILS localizer and glideslope are carriers with low-frequency AM, so
//! one decoder serves all three ([`NavaidChannelDecoder`]):
//!
//! 1. DDC to a 48 kHz channel ([`CHANNEL_RATE`], ±18 kHz kept so a VOR
//!    subcarrier survives an RTL-SDR's frequency error);
//! 2. FFT acquisition of the carrier (two on a two-frequency localizer or
//!    glideslope), each tracked by a PLL for coherent AM detection
//!    ([`carrier`]);
//! 3. per carrier, [`vor::VorAnalyzer`] (radial by matched correlation) or
//!    [`ils::IlsAnalyzer`] (DDM/SDM), and the Morse ident
//!    ([`paths::IdentPath`], [`morse`]);
//! 4. a [`NavaidObservation`] per reporting interval with status, levels,
//!    ident and measurements.
//!
//! Receive only. These measurements are for monitoring, never for
//! navigation.
//!
//! VERIFICATION POSTURE: validated against the self-generated signals in
//! [`modulate`] (CVOR and DVOR at every radial, localizers and glideslopes at
//! known DDM, two-frequency systems, frequency errors, AWGN sweeps) and
//! benchmarked against voracious, vortrack and dumpils on identical IQ (see
//! `docs/notes/NAVAID.md`). No off-air capture is in the corpus yet.

pub mod carrier;
pub mod catalog;
pub mod ils;
pub mod modulate;
pub mod morse;
pub mod paths;
pub mod vor;

pub use catalog::{kind_for_freq, NavaidKind};
pub use ils::IlsMeasure;
pub use vor::VorMeasure;

use carrier::{acquire, Carrier, CarrierPll};
use chrono::Utc;
use ils::IlsAnalyzer;
use num_complex::Complex;
use paths::IdentPath;
use serde::{Deserialize, Serialize};
use vor::VorAnalyzer;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Channel sample rate (Hz).
pub const CHANNEL_RATE: f64 = 48_000.0;
/// One-sided channel passband kept by the DDC (Hz).
/// Intermediate rate the front end decimates wide captures to first.
const PRE_DECIM_RATE: f64 = 200_000.0;
pub const CHANNEL_PASSBAND_HZ: f64 = 18_000.0;
/// Default reporting interval (s).
pub const DEFAULT_REPORT_S: f64 = 10.0;
/// Nominal ident tone (Hz).
pub const IDENT_HZ: f64 = 1_020.0;

const ACQ_N: usize = 32_768; // 0.68 s
const REACQ_S: f64 = 20.0;
/// With no carrier, search again this often (s).
const RETRY_S: f64 = 5.0;
const PLL_BN_HZ: f64 = 15.0;
/// A carrier found again within this of a tracked one is the same carrier.
const SAME_CARRIER_HZ: f64 = 60.0;
/// Chains work through input in pieces of at most this (50 ms), so the
/// carrier amplitude the ident is normalized by is never far from the
/// samples it divides, whatever chunk size the caller uses.
const FEED_BLOCK: usize = 2_400;
/// After this long on the air with nothing decoded, report "no ident".
const NO_IDENT_AFTER_S: f64 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavaidStatus {
    /// No carrier in the channel.
    NoSignal,
    /// Carrier found, still collecting.
    Acquiring,
    /// On the air with its ident.
    Normal,
    /// Carrier on the air but no ident: the usual maintenance signature.
    NoIdent,
    /// Sending TEST / TST as its ident.
    Test,
}

/// One reporting interval for one navaid channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NavaidObservation {
    pub kind: NavaidKind,
    /// Seconds of input processed when this was produced.
    pub t_s: f64,
    pub interval_s: f64,
    pub status: NavaidStatus,
    pub level_dbfs: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cn0_dbhz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carrier_offset_hz: Option<f64>,
    /// Voted ident (heard at least twice).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ident: Option<String>,
    pub ident_votes: u32,
    /// Latest word decoded, voted or not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ident_last: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ident_depth_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ident_tone_hz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vor: Option<VorMeasure>,
    /// ILS carriers, strongest first (the one a capture-effect receiver uses).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub ils: Vec<IlsMeasure>,
}

struct Chain {
    pll: CarrierPll,
    peak_db: f64,
    ibuf: Vec<f32>,
    vor: Option<VorAnalyzer>,
    ils: Option<IlsAnalyzer>,
    ident: Option<IdentPath>,
}

impl Chain {
    fn feed(&mut self, x: &[Complex<f32>]) {
        for part in x.chunks(FEED_BLOCK) {
            self.ibuf.clear();
            self.pll.process(part, &mut self.ibuf);
            if let Some(v) = &mut self.vor {
                v.process(&self.ibuf);
            }
            if let Some(a) = &mut self.ils {
                a.process(&self.ibuf);
            }
            if let Some(id) = &mut self.ident {
                id.process(&self.ibuf, self.pll.amp);
            }
        }
    }
}

pub struct NavaidChannelDecoder {
    kind: NavaidKind,
    /// Capture → 48 kHz channel (one or two stages).
    ddc: Option<Vec<Ddc>>,
    mid: Vec<Complex<f32>>,
    chan: Vec<Complex<f32>>,
    acq: Vec<Complex<f32>>,
    chains: Vec<Chain>,
    report_n: u64,
    since_report: u64,
    total: u64,
    on_air_since: Option<u64>,
    last_try: Option<u64>,
    level: f64,
}

impl NavaidChannelDecoder {
    /// `input_rate`: capture rate; `freq_offset_hz`: navaid channel relative
    /// to the capture center.
    pub fn new(kind: NavaidKind, input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else if input_rate >= 2.0 * PRE_DECIM_RATE {
            // Wide captures: a cheap integer decimation to about 200 kS/s
            // first, then the sharp channel filter at the low rate. One
            // sharp filter at the capture rate costs over twice as much (2
            // MS/s → 48 kHz is a prime decimation, 861 taps at full rate).
            let d1 = (input_rate / PRE_DECIM_RATE).floor();
            let mid = input_rate / d1;
            Some(vec![
                Ddc::new(input_rate, mid, freq_offset_hz, CHANNEL_PASSBAND_HZ)?,
                Ddc::new(mid, CHANNEL_RATE, 0.0, CHANNEL_PASSBAND_HZ)?,
            ])
        } else {
            Some(vec![Ddc::new(
                input_rate,
                CHANNEL_RATE,
                freq_offset_hz,
                CHANNEL_PASSBAND_HZ,
            )?])
        };
        Ok(Self {
            kind,
            ddc,
            mid: Vec::new(),
            chan: Vec::new(),
            acq: Vec::with_capacity(ACQ_N * 2),
            chains: Vec::new(),
            report_n: (DEFAULT_REPORT_S * CHANNEL_RATE) as u64,
            since_report: 0,
            total: 0,
            on_air_since: None,
            last_try: None,
            level: 0.0,
        })
    }

    /// Choose the kind from the channel frequency.
    pub fn for_freq(freq_hz: u64, input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let kind = kind_for_freq(freq_hz)
            .ok_or_else(|| format!("{freq_hz} Hz is not a VOR, localizer or glideslope channel"))?;
        Self::new(kind, input_rate, freq_offset_hz)
    }

    pub fn kind(&self) -> NavaidKind {
        self.kind
    }

    /// Reporting interval (s); the default is [`DEFAULT_REPORT_S`].
    pub fn set_report_interval(&mut self, seconds: f64) {
        self.report_n = (seconds.max(0.5) * CHANNEL_RATE) as u64;
    }

    pub fn level_dbfs(&self) -> f32 {
        if self.level > 0.0 {
            (10.0 * self.level.log10()) as f32
        } else {
            f32::NEG_INFINITY
        }
    }

    fn max_carriers(&self) -> usize {
        match self.kind {
            NavaidKind::Vor => 1,
            _ => 2,
        }
    }

    /// Carrier search span either side of the channel (Hz): the widest
    /// two-frequency separation (Annex 10: localizer 14 kHz, glideslope 32
    /// kHz, carriers symmetric about the channel) plus receiver frequency
    /// error, inside the ±18 kHz channel.
    fn span_hz(&self) -> f64 {
        match self.kind {
            NavaidKind::Vor => 8_000.0,
            NavaidKind::Localizer => 14_000.0,
            NavaidKind::Glideslope => 17_000.0,
        }
    }

    fn find_carriers(&self) -> Vec<Carrier> {
        acquire(
            &self.acq,
            CHANNEL_RATE,
            self.span_hz(),
            self.max_carriers(),
            2_500.0,
        )
    }

    fn new_chain(&self, c: &Carrier) -> Chain {
        Chain {
            pll: CarrierPll::new(CHANNEL_RATE, c.freq_hz, PLL_BN_HZ),
            peak_db: c.peak_db,
            ibuf: Vec::new(),
            vor: (self.kind == NavaidKind::Vor).then(VorAnalyzer::new),
            ils: (self.kind != NavaidKind::Vor).then(|| IlsAnalyzer::new(self.kind)),
            ident: None,
        }
    }

    fn new_ident(&self) -> Option<IdentPath> {
        (self.kind != NavaidKind::Glideslope).then(|| {
            IdentPath::new(
                IDENT_HZ,
                if self.kind == NavaidKind::Vor {
                    morse::IdentRule::Vor
                } else {
                    morse::IdentRule::Localizer
                },
            )
        })
    }

    /// Track the carriers `found`. A tracked chain whose carrier is found
    /// again (by frequency, whatever its rank now) keeps running with its
    /// state and the interval's measurements; only new carriers get new
    /// chains, which are run through the acquisition window so they start
    /// with its samples. The ident follows the strongest carrier.
    fn build_chains(&mut self, found: &[Carrier]) {
        let same = |c: &Chain, f: &Carrier| (f.freq_hz - c.pll.freq_hz()).abs() <= SAME_CARRIER_HZ;
        let dist = |c: &Chain, f: &Carrier| (f.freq_hz - c.pll.freq_hz()).abs();
        let mut old = std::mem::take(&mut self.chains);
        let mut slots: Vec<Option<Chain>> = found
            .iter()
            .map(|f| {
                let p = (0..old.len())
                    .filter(|&p| same(&old[p], f))
                    .min_by(|&a, &b| dist(&old[a], f).total_cmp(&dist(&old[b], f)))?;
                let mut c = old.remove(p);
                c.peak_db = f.peak_db;
                Some(c)
            })
            .collect();
        let kept_any = slots.iter().any(Option::is_some);
        // The ident path belongs to the strongest carrier. If the carriers
        // swapped rank, the existing one (with its votes) moves across.
        let mut moved = None;
        if slots
            .first()
            .is_some_and(|c| c.as_ref().is_none_or(|c| c.ident.is_none()))
        {
            moved = slots
                .iter_mut()
                .skip(1)
                .flatten()
                .find_map(|c| c.ident.take())
                .or_else(|| old.iter_mut().find_map(|c| c.ident.take()));
        }
        let acq = std::mem::take(&mut self.acq);
        let mut chains = Vec::with_capacity(found.len());
        for (i, (f, slot)) in found.iter().zip(slots).enumerate() {
            let mut c = match slot {
                Some(c) => c,
                None => {
                    let mut c = self.new_chain(f);
                    if i == 0 && moved.is_none() {
                        // A new ident path hears the window with its chain.
                        c.ident = self.new_ident();
                    }
                    c.feed(&acq);
                    c
                }
            };
            if i == 0 && c.ident.is_none() {
                c.ident = moved.take().or_else(|| self.new_ident());
            }
            chains.push(c);
        }
        self.acq = acq;
        self.chains = chains;
        if self.chains.is_empty() {
            self.on_air_since = None;
        } else if !kept_any {
            // A new transmitter gets the full time to send its ident.
            self.on_air_since = Some(self.total);
        }
    }

    /// Feed capture IQ; returns any observations completed.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<NavaidObservation> {
        // A non-finite sample would poison the filters, FFT and PLL: treat
        // it as a dropped sample (zero).
        let cleaned: Vec<Complex<f32>>;
        let input = if input.iter().all(|s| s.re.is_finite() && s.im.is_finite()) {
            input
        } else {
            cleaned = input
                .iter()
                .map(|s| {
                    if s.re.is_finite() && s.im.is_finite() {
                        *s
                    } else {
                        Complex::new(0.0, 0.0)
                    }
                })
                .collect();
            &cleaned
        };
        let chan = match &mut self.ddc {
            Some(stages) => {
                self.chan.clear();
                if let [first, second] = &mut stages[..] {
                    self.mid.clear();
                    first.process(input, &mut self.mid);
                    second.process(&self.mid, &mut self.chan);
                } else {
                    stages[0].process(input, &mut self.chan);
                }
                std::mem::take(&mut self.chan)
            }
            None => input.to_vec(),
        };
        let mut out = Vec::new();
        // Work in pieces that end at each report and carrier search, so
        // both run on time and see only the samples up to then.
        let mut rest = &chan[..];
        while !rest.is_empty() {
            if self.since_report >= self.report_n {
                // Due already (the interval was shortened): report first.
                out.push(self.report());
                continue;
            }
            let to_report = self.report_n - self.since_report;
            let to_acq = self.next_search().saturating_sub(self.total);
            let take = rest.len().min(to_report.min(to_acq).max(1) as usize);
            let (piece, tail) = rest.split_at(take);
            rest = tail;
            self.step(piece);
            if self.since_report >= self.report_n {
                out.push(self.report());
            }
        }
        if self.ddc.is_some() {
            self.chan = chan;
        }
        out
    }

    /// Sample count at which the next carrier search is due.
    fn next_search(&self) -> u64 {
        if self.chains.is_empty() {
            let full = self.total + ACQ_N.saturating_sub(self.acq.len()) as u64;
            let retry = self
                .last_try
                .map_or(0, |t| t + (RETRY_S * CHANNEL_RATE) as u64);
            full.max(retry)
        } else {
            let every = (REACQ_S * CHANNEL_RATE) as u64;
            (self.total / every + 1) * every
        }
    }

    fn step(&mut self, x: &[Complex<f32>]) {
        for s in x {
            self.level += 2e-5 * (s.norm_sqr() as f64 - self.level);
        }
        let before = self.total;
        self.total += x.len() as u64;
        self.since_report += x.len() as u64;
        self.acq.extend_from_slice(x);
        if self.acq.len() > ACQ_N {
            let drop = self.acq.len() - ACQ_N;
            self.acq.drain(..drop);
        }
        if self.chains.is_empty() {
            // Look for a carrier as soon as the window is full, then every 5 s.
            let due = self
                .last_try
                .is_none_or(|t| self.total - t >= (RETRY_S * CHANNEL_RATE) as u64);
            if self.acq.len() >= ACQ_N && due {
                self.last_try = Some(self.total);
                let found = self.find_carriers();
                self.build_chains(&found);
            }
            return;
        }
        for c in &mut self.chains {
            c.feed(x);
        }
        let every = (REACQ_S * CHANNEL_RATE) as u64;
        if before / every != self.total / every {
            // Matched chains keep their state and take the new peak level.
            let found = self.find_carriers();
            self.build_chains(&found);
        }
    }

    fn report(&mut self) -> NavaidObservation {
        let interval_s = self.since_report as f64 / CHANNEL_RATE;
        self.since_report = 0;
        let t_s = self.total as f64 / CHANNEL_RATE;
        let level_dbfs = self.level_dbfs();
        let kind = self.kind;
        if self.chains.is_empty() {
            return NavaidObservation {
                kind,
                t_s,
                interval_s,
                status: NavaidStatus::NoSignal,
                level_dbfs,
                cn0_dbhz: None,
                carrier_offset_hz: None,
                ident: None,
                ident_votes: 0,
                ident_last: None,
                ident_depth_pct: None,
                ident_tone_hz: None,
                vor: None,
                ils: Vec::new(),
            };
        }
        let main_db = self.chains[0].peak_db;
        let mut ils = Vec::new();
        let mut vor = None;
        for c in &mut self.chains {
            if let Some(v) = &mut c.vor {
                vor = v.take();
            }
            if let Some(a) = &mut c.ils {
                if let Some(m) = a.take(c.pll.freq_hz(), c.peak_db - main_db) {
                    ils.push(m);
                }
            }
        }
        let main = &self.chains[0];
        let cn0 = main.pll.cn0_dbhz();
        let (ident, votes, last, recent, depth, tone) = match &main.ident {
            Some(p) => {
                let id = p.decoder.ident();
                (
                    id.as_ref().map(|x| x.0.clone()),
                    id.map(|x| x.1).unwrap_or(0),
                    p.decoder.last_word.clone(),
                    p.decoder.recent_word().map(str::to_string),
                    (p.depth > 0.0).then_some(100.0 * p.depth as f64),
                    p.tone_offset_hz().map(|o| IDENT_HZ + o),
                )
            }
            None => (None, 0, None, None, None, None),
        };
        let on_air_s = self
            .on_air_since
            .map(|t| (self.total - t) as f64 / CHANNEL_RATE)
            .unwrap_or(0.0);
        // Status follows what is keyed now; `last` is only history.
        let is_test = |w: &Option<String>| w.as_deref().is_some_and(|w| w == "TEST" || w == "TST");
        let status = match &ident {
            // A station keying TEST now is in test, whatever it sent before.
            _ if is_test(&recent) || (recent.is_none() && is_test(&ident)) => NavaidStatus::Test,
            Some(_) => NavaidStatus::Normal,
            _ if kind == NavaidKind::Glideslope => NavaidStatus::Normal,
            _ if on_air_s >= NO_IDENT_AFTER_S
                && main
                    .ident
                    .as_ref()
                    .is_some_and(|p| p.decoder.words_heard() == 0) =>
            {
                NavaidStatus::NoIdent
            }
            _ => NavaidStatus::Acquiring,
        };
        NavaidObservation {
            kind,
            t_s,
            interval_s,
            status,
            level_dbfs,
            cn0_dbhz: cn0.is_finite().then_some(cn0),
            carrier_offset_hz: Some(main.pll.freq_hz()),
            ident,
            ident_votes: votes,
            ident_last: last,
            ident_depth_pct: depth,
            ident_tone_hz: tone,
            vor,
            ils,
        }
    }
}

/// Normalize an observation onto the xng message bus.
pub fn to_message(obs: &NavaidObservation, frequency_hz: u64, source: Provenance) -> Message {
    let details = serde_json::to_value(obs).unwrap_or(serde_json::Value::Null);
    Message {
        mode: match obs.kind {
            NavaidKind::Vor => Mode::Vor,
            _ => Mode::Ils,
        },
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(obs.level_dbfs),
            // C/N0 (dB-Hz) is in the details; it is not an SNR.
            snr_db: None,
            freq_skew_hz: obs.carrier_offset_hz.map(|f| f as f32),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: obs.status != NavaidStatus::NoSignal,
            fec_corrected: None,
            errors: None,
        },
        body: MessageBody::Navaid {
            kind: obs.kind.as_str().to_string(),
            ident: obs.ident.clone(),
            details,
        },
        raw: None,
        source,
    }
}
