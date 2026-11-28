//! Passive DME (distance measuring equipment) receiver.
//!
//! Listens to a ground transponder's reply frequency (962–1213 MHz) and
//! reports what it sends: pulse pairs (squitter and replies), the pair rate,
//! the 1350 pp/s Morse ident and, for TACAN beacons, the bearing reference
//! bursts. It never transmits and cannot measure the receiver's own range.
//!
//! * [`channels`]: the 252-channel X/Y plan, reply/interrogation
//!   frequencies, spacings and the VOR/ILS pairing table.
//! * [`detect`]: matched-filter pulse-pair detector.
//! * [`ident`]: periodicity-based ident keying and Morse decoding.
//! * [`modulate`]: synthetic transponder signal for loopback tests and the
//!   benchmark in `bench/dme`.
//!
//! Squitter and replies are identical pulse pairs, so on the reply frequency
//! alone only their sum is observable (`random_pps`). Tuning the
//! interrogation frequency of the same channel instead reports the aircraft
//! interrogations received from nearby aircraft (role `interrogation`): the
//! locally received interrogation rate, which is not the beacon's reply load
//! (the receiver and the beacon hear different aircraft).
//!
//! Written from ICAO Annex 10 Vol I §3.5. No code from gr-dme or other DME
//! decoders (see PROVENANCE.md).

pub mod channels;
pub mod detect;
pub mod ident;
pub mod modulate;

use channels::{DmeChannel, PairedKind, Role};
use detect::{PairDetector, PulsePair, Thresholds};
use ident::IdentDetector;
use num_complex::Complex;
use serde::Serialize;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Channel sample rate. 1 µs per sample: the 12/30/36 µs spacings are whole
/// samples and the 3.5 µs pulse spans about 7 samples.
pub const CHANNEL_RATE: f64 = 1_000_000.0;
/// One-sided DDC passband. A 3.5 µs Gaussian pulse has σ_f ≈ 107 kHz, so
/// ±300 kHz keeps the pulse and rejects the adjacent channel 1 MHz away.
pub const CHANNEL_PASSBAND_HZ: f64 = 300_000.0;
/// Stream time between status reports, s.
pub const STATUS_INTERVAL_S: f64 = 10.0;
/// Pairs a status interval needs before it is reported.
const MIN_STATUS_PAIRS: u64 = 20;
/// How long an ident from a spacing other than the busiest is held, µs: a
/// strong channel's pulse skirts make weak pairs at the other spacings that
/// repeat its ident, so a held ident is dropped if the busiest spacing
/// decodes the same text meanwhile.
const IDENT_HOLD_US: f64 = 3e6;
/// TACAN main reference burst: 12 pairs 30 µs apart (X mode), sent 15 times
/// a second. Counted from this many consecutive pairs at that gap.
const BURST_MIN_PAIRS: u32 = 8;

/// A decoded ident.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IdentReport {
    pub ident: String,
    pub morse: String,
    pub dot_ms: f32,
    pub ident_rate_hz: f64,
    /// How many times in a row this ident has been decoded.
    pub repeats: u32,
    #[serde(flatten)]
    pub link: LinkInfo,
}

/// Periodic channel summary.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StatusReport {
    pub interval_s: f64,
    /// All pairs at the report threshold, pairs/s.
    pub pps: f64,
    /// Pairs that were not ident pairs (squitter + replies, or interrogations).
    pub random_pps: f64,
    /// Ident pairs, pairs/s (averaged over the interval).
    pub ident_pps: f64,
    pub snr_db: f32,
    pub level_dbfs: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ident: Option<String>,
    /// TACAN main reference bursts (≥ 8 pairs 30 µs apart), bursts/s.
    pub reference_bursts_per_s: f64,
    pub tacan: bool,
    #[serde(flatten)]
    pub link: LinkInfo,
}

/// What the channel is.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LinkInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    pub spacing_us: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paired_vhf_hz: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paired_kind: Option<PairedKind>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DmeEvent {
    Ident(IdentReport),
    Status(StatusReport),
}

struct SpacingState {
    us: u32,
    ident: IdentDetector,
    pairs: u64,
    /// Report-threshold pairs since start.
    total: u64,
    snr_sum: f64,
    strong_hits_at_start: u64,
    last_t: f64,
    burst_len: u32,
    bursts: u64,
    last_ident: Option<String>,
    /// Stream time the last ident of this spacing ended, µs.
    last_ident_end_us: f64,
    repeats: u32,
}

/// An ident waiting out [`IDENT_HOLD_US`].
struct HeldIdent {
    spacing: usize,
    ident: ident::Ident,
    due_us: f64,
}

/// Decodes one DME channel from a capture.
pub struct DmeChannelDecoder {
    ddc: Option<Ddc>,
    /// Group delay of the DDC, µs (subtracted from pair times).
    ddc_delay_us: f64,
    channel_buf: Vec<Complex<f32>>,
    det: PairDetector,
    pairs: Vec<PulsePair>,
    states: Vec<SpacingState>,
    candidates: Vec<(DmeChannel, Role)>,
    interval_start_us: f64,
    now_us: f64,
    held: Vec<HeldIdent>,
}

impl DmeChannelDecoder {
    /// `input_rate` ≥ [`CHANNEL_RATE`]; `freq_offset_hz` = channel centre
    /// relative to the capture centre.
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        Self::with_thresholds(input_rate, freq_offset_hz, Thresholds::default())
    }

    pub fn with_thresholds(
        input_rate: f64,
        freq_offset_hz: f64,
        th: Thresholds,
    ) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(
                input_rate,
                CHANNEL_RATE,
                freq_offset_hz,
                CHANNEL_PASSBAND_HZ,
            )?)
        };
        let ddc_delay_us = if ddc.is_some() {
            measure_ddc_delay_us(input_rate)?
        } else {
            0.0
        };
        let mut d = Self {
            ddc,
            ddc_delay_us,
            channel_buf: Vec::new(),
            det: PairDetector::new(CHANNEL_RATE, &[12, 30, 36], th),
            pairs: Vec::new(),
            states: Vec::new(),
            candidates: Vec::new(),
            interval_start_us: 0.0,
            now_us: 0.0,
            held: Vec::new(),
        };
        d.set_spacings(&[12, 30, 36], th);
        Ok(d)
    }

    /// Tell the decoder the RF frequency it is on: it then only looks for the
    /// spacings DME uses there and names the channel.
    pub fn with_carrier(mut self, carrier_hz: u64, th: Thresholds) -> Self {
        let cands = DmeChannel::at_frequency(carrier_hz, 50_000);
        if !cands.is_empty() {
            let mut sp: Vec<u32> = cands.iter().map(|(c, r)| c.spacing_us(*r)).collect();
            sp.sort_unstable();
            sp.dedup();
            self.set_spacings(&sp, th);
        }
        self.candidates = cands;
        self
    }

    fn set_spacings(&mut self, sp: &[u32], th: Thresholds) {
        self.det = PairDetector::new(CHANNEL_RATE, sp, th);
        self.states = sp
            .iter()
            .map(|&us| SpacingState {
                us,
                ident: IdentDetector::new(),
                pairs: 0,
                total: 0,
                snr_sum: 0.0,
                strong_hits_at_start: 0,
                last_t: f64::NEG_INFINITY,
                burst_len: 0,
                bursts: 0,
                last_ident: None,
                last_ident_end_us: f64::NEG_INFINITY,
                repeats: 0,
            })
            .collect();
    }

    pub fn level_dbfs(&self) -> f32 {
        self.det.level_dbfs()
    }

    /// Feed capture IQ; returns idents and status reports.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<DmeEvent> {
        self.process_inner(input, None)
    }

    /// As [`process`](Self::process), also appending every report-threshold
    /// pair to `pairs_out` (benchmarks and tests).
    pub fn process_with_pairs(
        &mut self,
        input: &[Complex<f32>],
        pairs_out: &mut Vec<PulsePair>,
    ) -> Vec<DmeEvent> {
        self.process_inner(input, Some(pairs_out))
    }

    /// Flush pending detections at end of stream.
    pub fn finish(&mut self, pairs_out: Option<&mut Vec<PulsePair>>) -> Vec<DmeEvent> {
        let mut pairs = std::mem::take(&mut self.pairs);
        pairs.clear();
        self.det.flush(&mut pairs);
        let mut ev = Vec::new();
        self.consume(&pairs, pairs_out, &mut ev);
        // Let a trailing ident complete.
        let end = self.now_us + 5e6;
        for i in 0..self.states.len() {
            if let Some(id) = self.states[i].ident.advance_to(end) {
                self.on_ident(i, id, &mut ev);
            }
        }
        self.release_held(f64::INFINITY, &mut ev);
        self.pairs = pairs;
        ev
    }

    fn process_inner(
        &mut self,
        input: &[Complex<f32>],
        pairs_out: Option<&mut Vec<PulsePair>>,
    ) -> Vec<DmeEvent> {
        let mut chan = std::mem::take(&mut self.channel_buf);
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                chan.clear();
                ddc.process(input, &mut chan);
                &chan
            }
            None => input,
        };
        let mut pairs = std::mem::take(&mut self.pairs);
        pairs.clear();
        self.det.process(channel, &mut pairs);
        self.now_us += channel.len() as f64 / CHANNEL_RATE * 1e6;
        self.channel_buf = chan;

        let mut ev = Vec::new();
        self.consume(&pairs, pairs_out, &mut ev);
        // Quiet channel: keep the ident clocks moving.
        for i in 0..self.states.len() {
            if let Some(id) = self.states[i].ident.advance_to(self.now_us) {
                self.on_ident(i, id, &mut ev);
            }
        }
        self.release_held(self.now_us, &mut ev);
        if self.now_us - self.interval_start_us >= STATUS_INTERVAL_S * 1e6 {
            if let Some(s) = self.status() {
                ev.push(DmeEvent::Status(s));
            }
        }
        self.pairs = pairs;
        ev
    }

    fn consume(
        &mut self,
        pairs: &[PulsePair],
        mut pairs_out: Option<&mut Vec<PulsePair>>,
        ev: &mut Vec<DmeEvent>,
    ) {
        for p in pairs {
            let mut p = *p;
            p.t_us -= self.ddc_delay_us;
            let p = &p;
            let Some(i) = self.states.iter().position(|s| s.us == p.spacing_us) else {
                continue;
            };
            if let Some(id) = self.states[i].ident.push_pair(p.t_us, !p.weak) {
                self.on_ident(i, id, ev);
            }
            if p.weak {
                continue;
            }
            let st = &mut self.states[i];
            st.pairs += 1;
            st.total += 1;
            st.snr_sum += p.snr_db as f64;
            // TACAN main reference bursts: runs of pairs 30 µs apart.
            let gap = p.t_us - st.last_t;
            if (gap - 30.0).abs() <= 1.5 {
                st.burst_len = if st.burst_len == 0 {
                    2
                } else {
                    st.burst_len + 1
                };
                if st.burst_len == BURST_MIN_PAIRS {
                    st.bursts += 1;
                }
            } else {
                st.burst_len = 0;
            }
            st.last_t = p.t_us;
            if let Some(out) = pairs_out.as_deref_mut() {
                out.push(*p);
            }
        }
    }

    /// The busiest spacing in this interval (index into `states`).
    fn busiest(&self) -> usize {
        (0..self.states.len())
            .max_by_key(|&i| self.states[i].pairs)
            .unwrap_or(0)
    }

    /// Whether spacing `i` carries the most pairs since start.
    fn is_busiest(&self, i: usize) -> bool {
        let n = self.states[i].total;
        self.states.iter().all(|s| s.total <= n)
    }

    /// Whether another, busier spacing decoded `text` within the hold time
    /// of `end_us`: then spacing `i`'s copy is a pulse-skirt echo.
    fn echoes_busier(&self, i: usize, text: &str, end_us: f64) -> bool {
        let n = self.states[i].total;
        self.states.iter().enumerate().any(|(j, s)| {
            j != i
                && s.total > n
                && s.last_ident.as_deref() == Some(text)
                && (s.last_ident_end_us - end_us).abs() <= IDENT_HOLD_US
        })
    }

    /// Route an ident decoded on spacing `i`. Interrogations carry no ident,
    /// so a spacing known to be one is ignored. The busiest spacing reports
    /// at once; another spacing (a second channel sharing the frequency, or
    /// echoes of the busiest one) is held and dropped if it repeats a busier
    /// spacing's ident.
    fn on_ident(&mut self, i: usize, id: ident::Ident, ev: &mut Vec<DmeEvent>) {
        if self.link(i).role == Some(Role::Interrogation) {
            return;
        }
        if self.is_busiest(i) {
            let text = id.text.clone();
            let end = id.end_us;
            ev.push(DmeEvent::Ident(self.ident_report(i, id)));
            self.held
                .retain(|h| h.ident.text != text || (h.ident.end_us - end).abs() > IDENT_HOLD_US);
        } else if !self.echoes_busier(i, &id.text, id.end_us) {
            let due_us = self.now_us + IDENT_HOLD_US;
            self.held.push(HeldIdent {
                spacing: i,
                ident: id,
                due_us,
            });
        }
    }

    /// Report held idents due by `now` that no busier spacing repeated.
    fn release_held(&mut self, now: f64, ev: &mut Vec<DmeEvent>) {
        let mut k = 0;
        while k < self.held.len() {
            if self.held[k].due_us > now {
                k += 1;
                continue;
            }
            let h = self.held.remove(k);
            if !self.echoes_busier(h.spacing, &h.ident.text, h.ident.end_us) {
                ev.push(DmeEvent::Ident(self.ident_report(h.spacing, h.ident)));
            }
        }
    }

    fn link(&self, i: usize) -> LinkInfo {
        let us = self.states[i].us;
        let hit = self.candidates.iter().find(|(c, r)| c.spacing_us(*r) == us);
        LinkInfo {
            channel: hit.map(|(c, _)| c.to_string()),
            role: hit.map(|(_, r)| *r),
            spacing_us: us,
            paired_vhf_hz: hit.and_then(|(c, _)| c.paired_vhf_hz()),
            paired_kind: hit.and_then(|(c, _)| c.paired_kind()),
        }
    }

    fn ident_report(&mut self, i: usize, id: ident::Ident) -> IdentReport {
        let st = &mut self.states[i];
        if st.last_ident.as_deref() == Some(id.text.as_str()) {
            st.repeats += 1;
        } else {
            st.repeats = 1;
            st.last_ident = Some(id.text.clone());
        }
        st.last_ident_end_us = id.end_us;
        let repeats = st.repeats;
        IdentReport {
            ident: id.text,
            morse: id.morse,
            dot_ms: id.dot_ms,
            ident_rate_hz: id.rate_hz,
            repeats,
            link: self.link(i),
        }
    }

    fn status(&mut self) -> Option<StatusReport> {
        let secs = (self.now_us - self.interval_start_us) / 1e6;
        self.interval_start_us = self.now_us;
        let i = self.busiest();
        let link = self.link(i);
        let total: u64 = self.states.iter().map(|s| s.pairs).sum();
        let st = &self.states[i];
        let pairs = st.pairs;
        // Strong pairs only, like `pps`.
        let ident_hits = st.ident.strong_hits - st.strong_hits_at_start;
        let report = (total >= MIN_STATUS_PAIRS).then(|| {
            let pps = pairs as f64 / secs;
            // Remove the random coincidences the ident test also counts.
            let ident_pps = (ident_hits as f64 / secs - st.ident.strong_background_hits_per_s())
                .clamp(0.0, pps);
            let bursts = st.bursts as f64 / secs;
            StatusReport {
                interval_s: secs,
                pps,
                random_pps: (pps - ident_pps).max(0.0),
                ident_pps,
                snr_db: if pairs > 0 {
                    (st.snr_sum / pairs as f64) as f32
                } else {
                    0.0
                },
                level_dbfs: self.det.level_dbfs(),
                last_ident: st.last_ident.clone(),
                reference_bursts_per_s: bursts,
                // Main bursts come at 15 Hz; DME has none.
                tacan: (10.0..=20.0).contains(&bursts),
                link,
            }
        });
        for s in self.states.iter_mut() {
            s.pairs = 0;
            s.snr_sum = 0.0;
            s.bursts = 0;
            s.strong_hits_at_start = s.ident.strong_hits;
        }
        report
    }
}

/// Group delay of the channel DDC: run a pulse through an identical DDC and
/// find where it comes out.
fn measure_ddc_delay_us(input_rate: f64) -> Result<f64, String> {
    let mut ddc = Ddc::new(input_rate, CHANNEL_RATE, 0.0, CHANNEL_PASSBAND_HZ)?;
    let n = (input_rate * 2e-3) as usize;
    let t0 = 1e-3;
    let sigma = 1.5e-6;
    let x: Vec<Complex<f32>> = (0..n)
        .map(|i| {
            let dt = i as f64 / input_rate - t0;
            Complex::new((-dt * dt / (2.0 * sigma * sigma)).exp() as f32, 0.0)
        })
        .collect();
    let mut y = Vec::new();
    ddc.process(&x, &mut y);
    let m: Vec<f32> = y.iter().map(|c| c.norm()).collect();
    let k = (1..m.len().saturating_sub(1))
        .max_by(|&a, &b| m[a].total_cmp(&m[b]))
        .ok_or("DDC probe too short")?;
    let den = m[k - 1] - 2.0 * m[k] + m[k + 1];
    let frac = if den.abs() > 1e-12 {
        0.5 * (m[k - 1] - m[k + 1]) / den
    } else {
        0.0
    };
    Ok(((k as f64 + frac as f64) / CHANNEL_RATE - t0) * 1e6)
}

/// Normalize a decoder event into an xng message.
pub fn to_message(
    ev: &DmeEvent,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let (kind, details, snr) = match ev {
        DmeEvent::Ident(r) => ("ident", serde_json::to_value(r), None),
        DmeEvent::Status(s) => ("status", serde_json::to_value(s), Some(s.snr_db)),
    };
    Message {
        mode: Mode::Dme,
        timestamp: chrono::Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            snr_db: snr,
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: true,
            fec_corrected: None,
            errors: None,
        },
        body: MessageBody::Dme {
            kind: kind.to_string(),
            details: details.unwrap_or(serde_json::Value::Null),
        },
        raw: None,
        source,
    }
}
