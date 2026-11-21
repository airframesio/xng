//! Native multi-band radio time-signal decode core for xng.
//!
//! "Time" is a meta-mode: a family of standard-frequency / time-signal
//! broadcasts spread across the LF (< 300 kHz) and HF (3–30 MHz) bands. This
//! crate provides:
//!
//! - [`catalog`] — the worldwide station table (WWV, WWVH, CHU, WWVB, DCF77,
//!   MSF, JJY, BPM, RWM, TDF, RBU, YVTO) with carriers, band class, modulation
//!   family, and decode capability, plus [`catalog::receivable`], the
//!   capability-ranked auto-scan an SDR's tunable range maps to channels.
//! - [`chu`] — the flagship CHU AFSK (Bell-103, 300 baud, 8N2) decoder.
//! - [`wwv`] — the WWV/WWVH 100 Hz subcarrier BCD time-code decoder.
//! - [`audio`] — shared AM-envelope / Goertzel / biquad audio DSP.
//! - [`modulate`] — synthesis used by the self-generated demod tests.
//!
//! [`TimeChannelDecoder`] is the channelized IQ entry point, mirroring the
//! NAVTEX/EOT crate template: it owns an [`xng_dsp::Ddc`] that mixes a wideband
//! capture by `freq_offset_hz` and decimates to [`CHANNEL_RATE`], AM-demodulates
//! to audio, and runs whichever decoder the tuned carrier maps to (CHU AFSK or
//! WWV/WWVH BCD). [`to_message`] normalizes a decoded frame onto the
//! [`xng_types`] bus.
//!
//! VERIFICATION POSTURE: the catalog and the BCD/AFSK/redundancy decode cores
//! are anchored by their own table tests (the published broadcast formats); the
//! IQ → audio → time path is validated by self-generated `modulate → AWGN →
//! demod` loopback tests (`*_synth`). No off-air IQ exists, so no real-RF claim
//! is made — exactly the posture DSC/EOT/ATCS landed under. The LF stations
//! (WWVB/DCF77/MSF/JJY/TDF/RBU) are catalog-only pending an LF capture path
//! (see docs/notes/TIME.md).

pub mod audio;
pub mod catalog;
pub mod chu;
pub mod modulate;
pub mod wwv;

use chrono::{TimeZone, Utc};
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal audio sample rate after AM-demod. 12 kHz carries the CHU AFSK
/// tones (2025/2225 Hz, 40 samples/bit at 300 baud) and the WWV 100 Hz
/// subcarrier with ample margin, at a clean integer multiple of 300 baud.
pub const CHANNEL_RATE: f64 = 12_000.0;
/// One-sided DDC passband. HF time stations are AM with a few-kHz audio
/// bandwidth; ±5 kHz passes the full CHU/WWV audio (incl. the 2225 Hz tone and
/// the seconds ticks) while rejecting adjacent traffic.
pub const CHANNEL_PASSBAND_HZ: f64 = 5_000.0;

/// Which audio decoder a channel runs, chosen from the tuned carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeDecoder {
    /// CHU AFSK (Bell-103) packet decoder.
    Chu,
    /// WWV/WWVH 100 Hz subcarrier BCD decoder.
    Wwv,
}

/// Pick the decoder for a tuned carrier frequency by matching it to the
/// catalog. CHU carriers → CHU; WWV/WWVH carriers → WWV. Returns `None` for a
/// carrier no decodable HF station sits on. The match tolerance is ±2 kHz so a
/// carrier-tone neighbour 4 kHz away (e.g. RWM 9996 vs WWV 10000) does not
/// falsely snap to a decodable station.
pub fn decoder_for_carrier(freq_hz: u64) -> Option<TimeDecoder> {
    for st in catalog::CATALOG {
        if st.capability != catalog::Capability::Decode {
            continue;
        }
        for &c in st.carriers {
            if (c as i64 - freq_hz as i64).unsigned_abs() <= 2_000 {
                return match st.decoder {
                    catalog::Decoder::Chu => Some(TimeDecoder::Chu),
                    catalog::Decoder::Wwv => Some(TimeDecoder::Wwv),
                    catalog::Decoder::None => None,
                };
            }
        }
    }
    None
}

/// One decoded time frame: the station, the recovered UTC fields, and the
/// metadata the bus message carries.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeFrame {
    /// Station identifier ("CHU", "WWV", "WWVH").
    pub station: String,
    /// Decoded UTC, when a full date+time is available.
    pub utc: Option<chrono::DateTime<Utc>>,
    /// Day of year (1–366).
    pub day_of_year: Option<u16>,
    pub year: Option<u16>,
    pub hour: Option<u8>,
    pub minute: Option<u8>,
    pub second: Option<u8>,
    /// DUT1 (UT1 − UTC) in seconds, where broadcast.
    pub dut1_s: Option<f32>,
    /// Leap-second pending flag.
    pub leap_pending: bool,
    /// Daylight-saving / DST indicator (station-specific raw value or bit).
    pub dst: Option<i64>,
    /// Validity gate: redundancy match (CHU) or frame sync (WWV).
    pub valid: bool,
    /// 0.0–1.0 sync confidence (CHU: redundancy + framing; WWV: marker grid).
    pub sync_confidence: f32,
    /// Which decoder produced this.
    pub decoder: TimeDecoder,
}

/// Decodes one time-signal channel out of a wideband (or channel-rate) capture.
///
/// Mirrors the NAVTEX/EOT template: owns an internal [`Ddc`] that mixes by
/// `freq_offset_hz` and decimates the capture to [`CHANNEL_RATE`],
/// AM-demodulates to audio, and runs the carrier-selected decoder. The tuned
/// carrier (set via [`TimeChannelDecoder::with_carrier`]) selects CHU vs WWV;
/// without it the decoder defaults to CHU (the richer digital format) but tries
/// both on the buffered audio.
pub struct TimeChannelDecoder {
    ddc: Option<Ddc>,
    channel_buf: Vec<Complex<f32>>,
    /// Buffered AM-demodulated audio (time stations are slow; we accumulate a
    /// minute-scale window and re-scan).
    audio: Vec<f32>,
    /// Smoothed channel power for the level estimate.
    level: f32,
    /// The decoder selected by the tuned carrier, if known.
    decoder: Option<TimeDecoder>,
    /// Frames already reported (dedup by station+utc fields).
    seen: Vec<String>,
}

/// Channel power smoothing factor for the level estimate.
const LEVEL_ALPHA: f32 = 0.002;
/// AM-envelope DC-removal corner (slow high-pass).
const DC_ALPHA: f32 = 0.001;

impl TimeChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`]; a non-integer
    /// multiple is resampled by the DDC. `freq_offset_hz` is the station
    /// carrier relative to the capture center (0 if already centered).
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
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
        Ok(Self {
            ddc,
            channel_buf: Vec::new(),
            audio: Vec::new(),
            level: 0.0,
            decoder: None,
            seen: Vec::new(),
        })
    }

    /// Select the decoder from the tuned carrier frequency (CHU vs WWV/WWVH).
    /// The runtime calls this so the channel runs the right audio decoder.
    pub fn with_carrier(mut self, carrier_hz: u64) -> Self {
        self.decoder = decoder_for_carrier(carrier_hz);
        self
    }

    /// Set the decoder explicitly.
    pub fn set_decoder(&mut self, d: TimeDecoder) {
        self.decoder = Some(d);
    }

    /// Feed capture IQ; returns newly completed time frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<TimeFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        for &x in channel {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);
        }
        let mut audio = audio::am_envelope(channel, DC_ALPHA);
        self.audio.append(&mut audio);

        let mut out = Vec::new();
        // CHU: scan for one-second AFSK packets across the buffered audio.
        let want_chu = matches!(self.decoder, Some(TimeDecoder::Chu) | None);
        let want_wwv = matches!(self.decoder, Some(TimeDecoder::Wwv) | None);

        if want_chu {
            for f in self.scan_chu() {
                let key = frame_key(&f);
                if !self.seen.contains(&key) {
                    self.seen.push(key);
                    out.push(f);
                }
            }
        }
        if want_wwv {
            if let Some(f) = self.scan_wwv() {
                let key = frame_key(&f);
                if !self.seen.contains(&key) {
                    self.seen.push(key);
                    out.push(f);
                }
            }
        }
        out
    }

    /// Scan the buffered audio for CHU one-second AFSK packets. A CHU packet's
    /// data field is 110 bits at 300 baud (≈ 366.7 ms); we hunt for the first
    /// UART start edge (idle MARK → SPACE falling edge) after the MARK
    /// preamble and decode 110 bits from there.
    fn scan_chu(&self) -> Vec<TimeFrame> {
        let sr = CHANNEL_RATE;
        let spb = sr / chu::BAUD;
        let field_bits = chu::PACKET_BITS;
        let field_samples = (field_bits as f64 * spb).ceil() as usize;
        let mut out = Vec::new();
        if self.audio.len() < field_samples {
            return out;
        }

        // Mark/space instantaneous discriminator over the whole buffer (per
        // chu::afsk_bits the bandpass is applied internally; here we only need
        // start-edge detection, which we get by comparing windowed mark vs
        // space power on a coarse grid).
        let starts = self.chu_start_candidates();
        for &start in &starts {
            let bits = chu::afsk_bits(&self.audio, sr, start as f64, field_bits);
            let chars = chu::read_chars(&bits);
            if chars.len() != chu::PACKET_BYTES {
                continue;
            }
            let bytes: Vec<u8> = chars.iter().map(|c| c.byte).collect();
            let framing = chars.iter().filter(|c| c.framing_ok).count();
            if let Some(pkt) = chu::parse_packet(&bytes) {
                if let Some(frame) = chu_to_frame(&pkt, framing) {
                    out.push(frame);
                }
            }
        }
        out
    }

    /// Coarse list of sample offsets where a CHU UART start bit may begin: the
    /// falling edges (idle MARK → START SPACE) in the buffered audio. We slice
    /// the bandpassed AFSK envelope and look for mark→space transitions, then
    /// return their sample positions.
    fn chu_start_candidates(&self) -> Vec<usize> {
        let sr = CHANNEL_RATE;
        let spb = sr / chu::BAUD;
        let win = (spb * 0.5).round().max(1.0) as usize;
        let mut bp = audio::Biquad::bandpass(chu::CENTER_HZ, 6.0, sr);
        let filt = bp.filter(&self.audio);

        // Windowed mark vs space power on a grid; a transition from
        // mark-dominant to space-dominant marks a candidate start.
        let mut prev_mark = true;
        let mut cands = Vec::new();
        let step = (win / 2).max(1);
        let mut i = 0;
        while i + win <= filt.len() {
            let mut g_m = audio::Goertzel::new(chu::MARK_HZ, sr);
            let mut g_s = audio::Goertzel::new(chu::SPACE_HZ, sr);
            for &s in &filt[i..i + win] {
                g_m.add(s);
                g_s.add(s);
            }
            let mark = g_m.power() >= g_s.power();
            if prev_mark && !mark {
                // Falling edge near i: the start bit begins around here.
                cands.push(i);
            }
            prev_mark = mark;
            i += step;
        }
        // Refine: a start edge is followed by 110 data bits; keep candidates
        // with room for a full field, and dedup near-duplicates.
        let field_samples = (chu::PACKET_BITS as f64 * spb).ceil() as usize;
        cands.retain(|&c| c + field_samples <= filt.len());
        cands.dedup_by(|a, b| (*a as i64 - *b as i64).abs() < (spb as i64));
        cands
    }

    /// Scan the buffered audio for a full WWV/WWVH minute. Needs ≥ 60 s of
    /// audio; measures each second's 100 Hz tone-burst length, classifies, and
    /// frame-syncs on the markers + the sec-0 hole.
    fn scan_wwv(&self) -> Option<TimeFrame> {
        let sr = CHANNEL_RATE;
        let sec = sr as usize;
        if self.audio.len() < 60 * sec {
            return None;
        }
        // Take the most recent 60 whole seconds.
        let total_secs = self.audio.len() / sec;
        let start_sec = total_secs - 60;
        let base = start_sec * sec;

        let mut symbols = Vec::with_capacity(60);
        for s in 0..60 {
            let lo = base + s * sec;
            let hi = lo + sec;
            let len = wwv::tone_length(&self.audio[lo..hi], sr);
            symbols.push(wwv::classify(len));
        }
        let frame = wwv::parse_minute(&symbols)?;
        let station = wwv::label_station(&self.audio[base..base + 60 * sec], sr);
        Some(wwv_to_frame(&frame, station))
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

/// Dedup / identity key for a decoded frame.
fn frame_key(f: &TimeFrame) -> String {
    format!(
        "{}|{:?}|{:?}|{:?}:{:?}:{:?}|{:?}",
        f.station, f.year, f.day_of_year, f.hour, f.minute, f.second, f.decoder
    )
}

/// CHU packet → frame. `framing` is how many of the 10 chars had valid 8N2
/// framing (a soft confidence input). A Format A packet yields time-of-day; a
/// Format B yields year/DUT1/leap (no time-of-day). Validity = redundancy gate.
fn chu_to_frame(pkt: &chu::ChuPacket, framing: usize) -> Option<TimeFrame> {
    let fields = pkt.format?;
    let framing_conf = framing as f32 / chu::PACKET_BYTES as f32;
    let sync_confidence = if pkt.redundancy_ok {
        0.5 + 0.5 * framing_conf
    } else {
        0.4 * framing_conf
    };
    let mut frame = TimeFrame {
        station: "CHU".to_string(),
        utc: None,
        day_of_year: None,
        year: None,
        hour: None,
        minute: None,
        second: None,
        dut1_s: None,
        leap_pending: false,
        dst: None,
        valid: pkt.redundancy_ok,
        sync_confidence,
        decoder: TimeDecoder::Chu,
    };
    match fields {
        chu::ChuFormatFields::A {
            day_of_year,
            hour,
            minute,
            second,
        } => {
            frame.day_of_year = Some(day_of_year);
            frame.hour = Some(hour);
            frame.minute = Some(minute);
            frame.second = Some(second);
        }
        chu::ChuFormatFields::B {
            year,
            dut1_s,
            tai_minus_utc: _,
            leap_pending,
            dst_code,
        } => {
            frame.year = Some(year);
            frame.dut1_s = Some(dut1_s);
            frame.leap_pending = leap_pending;
            frame.dst = Some(dst_code as i64);
        }
    }
    Some(frame)
}

/// WWV frame → frame. WWV carries a full date+time in one minute, so we can
/// build a complete UTC `DateTime` (second = 0, the minute-mark reference).
fn wwv_to_frame(f: &wwv::WwvFrame, station: wwv::WwvStation) -> TimeFrame {
    let utc = utc_from_doy(f.year, f.day_of_year, f.hour, f.minute, 0);
    TimeFrame {
        station: station.name().to_string(),
        utc,
        day_of_year: Some(f.day_of_year),
        year: Some(f.year),
        hour: Some(f.hour),
        minute: Some(f.minute),
        second: Some(0),
        dut1_s: Some(f.dut1_s_tenths as f32 * 0.1),
        leap_pending: f.leap_pending,
        dst: Some(f.dst1 as i64),
        valid: true,
        sync_confidence: f.sync_score as f32 / 7.0,
        decoder: TimeDecoder::Wwv,
    }
}

/// Build a UTC `DateTime` from year + day-of-year + h:m:s.
fn utc_from_doy(year: u16, doy: u16, h: u8, m: u8, s: u8) -> Option<chrono::DateTime<Utc>> {
    use chrono::Datelike;
    let jan1 = chrono::NaiveDate::from_ymd_opt(year as i32, 1, 1)?;
    let date = jan1.with_ordinal(doy as u32)?;
    let time = chrono::NaiveTime::from_hms_opt(h as u32, m as u32, s as u32)?;
    Some(Utc.from_utc_datetime(&date.and_time(time)))
}

/// Convert a decoded time frame into the normalized bus message.
///
/// `details` JSON carries: station, decoded UTC (ISO-8601, when full), the
/// individual fields (year/doy/h/m/s), DUT1, flags (leap/DST), the validity
/// gate, and the sync confidence. `decode.crc_ok` is the validity gate.
pub fn to_message(
    f: &TimeFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let mut details = serde_json::Map::new();
    details.insert("station".into(), f.station.clone().into());
    details.insert("decoder".into(), match f.decoder {
        TimeDecoder::Chu => "chu",
        TimeDecoder::Wwv => "wwv",
    }
    .into());
    if let Some(utc) = f.utc {
        details.insert("utc".into(), utc.to_rfc3339().into());
    }
    if let Some(y) = f.year {
        details.insert("year".into(), y.into());
    }
    if let Some(d) = f.day_of_year {
        details.insert("day_of_year".into(), d.into());
    }
    if let Some(h) = f.hour {
        details.insert("hour".into(), h.into());
    }
    if let Some(m) = f.minute {
        details.insert("minute".into(), m.into());
    }
    if let Some(s) = f.second {
        details.insert("second".into(), s.into());
    }
    if let Some(dut1) = f.dut1_s {
        details.insert("dut1_s".into(), serde_json::json!(dut1));
    }
    details.insert("leap_pending".into(), f.leap_pending.into());
    if let Some(dst) = f.dst {
        details.insert("dst".into(), dst.into());
    }
    details.insert("valid".into(), f.valid.into());
    details.insert("sync_confidence".into(), serde_json::json!(f.sync_confidence));

    Message {
        mode: Mode::Time,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: f.valid,
            fec_corrected: None,
            errors: None,
        },
        body: MessageBody::Time {
            station: f.station.clone(),
            details: serde_json::Value::Object(details),
        },
        raw: None,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_rate_is_integer_bit_multiple() {
        let spb = CHANNEL_RATE / chu::BAUD;
        assert_eq!(spb.fract(), 0.0, "{spb} samples/bit");
        // Output rate must carry the two-sided passband (Nyquist).
        let min_rate = 2.0 * CHANNEL_PASSBAND_HZ;
        assert!(CHANNEL_RATE >= min_rate, "{CHANNEL_RATE} < {min_rate}");
    }

    #[test]
    fn carrier_selects_decoder() {
        assert_eq!(decoder_for_carrier(7_850_000), Some(TimeDecoder::Chu));
        assert_eq!(decoder_for_carrier(3_330_000), Some(TimeDecoder::Chu));
        assert_eq!(decoder_for_carrier(10_000_000), Some(TimeDecoder::Wwv)); // WWV
        assert_eq!(decoder_for_carrier(15_000_000), Some(TimeDecoder::Wwv));
        // RWM 9996 kHz is carrier-tone only and 4 kHz from WWV 10 MHz — the
        // ±2 kHz tolerance must NOT snap it to WWV.
        assert_eq!(decoder_for_carrier(9_996_000), None); // RWM
        assert_eq!(decoder_for_carrier(146_000_000), None); // nothing here
    }

    #[test]
    fn utc_from_doy_builds_date() {
        // 2026 is not a leap year: doy 159 = June 8.
        let dt = utc_from_doy(2026, 159, 12, 34, 0).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-06-08T12:34:00+00:00");
    }
}
