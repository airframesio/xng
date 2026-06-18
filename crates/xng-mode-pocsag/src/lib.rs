//! Native POCSAG (CCIR Radiopaging Code No.1 / ITU-R M.584-2) decode core for
//! xng.
//!
//! POCSAG is the dominant one-way radio-paging protocol: binary FSK at
//! 512 / 1200 / 2400 baud with ~±4.5 kHz deviation, carrying the CCIR
//! Radiopaging Code No.1 (ITU-R Recommendation M.584-2, Annex 1). A
//! transmission is a long alternating preamble (≥576 bits), then one or more
//! **batches**; each batch is a 32-bit frame-sync codeword (`0x7CD215D8`)
//! followed by 8 frames of 2 codewords each. Every 32-bit codeword is a flag
//! bit + 20 information bits + BCH(31,21,2) check bits + an even-parity bit.
//!
//! This crate splits cleanly into:
//!
//! - [`bch`] — BCH(31,21,2) syndrome correction + even parity (the per-codeword
//!   integrity layer). Spec-anchored: generator polynomial and idle/sync
//!   constants cited to ITU-R M.584-2.
//! - [`frame`] — batch/codeword framing: address codewords (capcode =
//!   `(addr18 << 3) | frame_position`, 2 function bits), message codewords (20
//!   payload bits → numeric 4-bit-reversed or alphanumeric 7-bit-LSB-first
//!   text), idle handling. Spec-anchored.
//! - [`demod`] — 2-FSK NRZ frequency-discriminator demod + preamble/sync hunt.
//! - [`modulate`] — waveform synthesis used ONLY by the synthetic
//!   modulate→AWGN→demod BER test.
//!
//! [`PocsagChannelDecoder`] is the channelized IQ entry point (mirrors the
//! NAVTEX template): it owns an [`xng_dsp::Ddc`] that mixes a wideband capture
//! by `freq_offset_hz` and decimates to [`CHANNEL_RATE`], runs the FSK demod at
//! a configured baud, hunts the preamble + sync codeword, then BCH-corrects and
//! decodes each batch into [`PocsagFrame`]s. [`to_message`] normalizes those
//! into the [`xng_types`] bus form.
//!
//! VERIFICATION: the DECODE/framing core (BCH, codeword layout, text tables) is
//! validated against hand-constructed, spec-cited codewords — NOT against the
//! modulator. The DEMOD front end is validated by a synthetic
//! modulate→complex-AWGN→demod BER measurement (see `demod_ber_synth_iq`),
//! reported as synthetic; no off-air IQ is available.

pub mod bch;
pub mod demod;
pub mod frame;
pub mod modulate;

use chrono::Utc;
use frame::Codeword;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal demod sample rate, 38.4 kS/s. A common integer multiple of all
/// three POCSAG bauds — 38400 = 75·512 = 32·1200 = 16·2400 — so every baud has
/// a whole number of samples per bit, and it comfortably carries the ±4.5 kHz
/// FSK deviation (Nyquist 19.2 kHz).
pub const CHANNEL_RATE: f64 = 38_400.0;

/// One-sided DDC passband: passes both ±4.5 kHz FSK tones plus realistic
/// carrier tuning offset, while staying well inside the channel rate.
pub const CHANNEL_PASSBAND_HZ: f64 = 7_500.0;

/// Maximum bit errors tolerated when matching the 32-bit frame-sync codeword.
const SYNC_MAX_ERR: u32 = 2;

/// One fully decoded POCSAG message extracted from a batch.
#[derive(Debug, Clone, PartialEq)]
pub struct PocsagFrame {
    /// Pager capcode (full 21-bit address: `(addr18 << 3) | frame_position`).
    pub capcode: u32,
    /// 2-bit function code from the address codeword.
    pub function: u8,
    /// Baud the batch was decoded at.
    pub baud: u32,
    /// Message class: `"numeric"`, `"alpha"`, or `"tone"` (no message body).
    pub kind: PocsagKind,
    /// Decoded message text (empty for a tone-only page).
    pub text: String,
    /// Number of BCH-corrected bit errors across the codewords of this message.
    pub fec_corrected: u32,
    /// The raw 32-bit codewords (address + message) this frame decoded from,
    /// big-endian bytes, for re-decoding / provenance.
    pub raw: Vec<u8>,
}

/// POCSAG message class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PocsagKind {
    /// Numeric paging (4-bit BCD-style digits).
    Numeric,
    /// Alphanumeric paging (7-bit ASCII).
    Alpha,
    /// Tone-only page (address codeword with no following message codewords).
    Tone,
}

impl PocsagKind {
    /// The bus `kind` string emitted in [`MessageBody::Pocsag`].
    pub fn as_str(self) -> &'static str {
        match self {
            PocsagKind::Numeric => "numeric",
            PocsagKind::Alpha => "alpha",
            PocsagKind::Tone => "tone",
        }
    }
}

/// Decode all batches found in a recovered, sync-aligned bit history.
///
/// `bits` must be the raw demod bit stream (any polarity); this function finds
/// the sync codeword, fixes polarity, then reads consecutive batches. Each
/// address codeword opens a message; following message codewords (until the
/// next address/idle/sync or end of data) form its body, decoded as numeric
/// and alphanumeric — the [`PocsagKind`] is chosen by the function code (3 =
/// alphanumeric per common convention) with a numeric fallback.
///
/// Returns one [`PocsagFrame`] per address codeword that carried information.
pub fn decode_bits(bits: &[u8], baud: u32) -> Vec<PocsagFrame> {
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some((sync_off, inverted)) = demod::find_sync(&bits[search_from..], SYNC_MAX_ERR) {
        let batch_start = search_from + sync_off + 32; // first codeword after sync
        let frames = decode_batch(bits, batch_start, inverted, baud);
        out.extend(frames.frames);
        // Continue scanning after this batch's 16 codewords (or wherever data
        // ran out) for a subsequent batch.
        let advance = (sync_off + 32 + frame::CODEWORDS_PER_BATCH * 32).max(32);
        if search_from + advance >= bits.len() {
            break;
        }
        search_from += advance;
    }
    out
}

struct BatchResult {
    frames: Vec<PocsagFrame>,
}

/// Decode the (up to) 16 codewords of one batch starting at bit `start`.
fn decode_batch(bits: &[u8], start: usize, inverted: bool, baud: u32) -> BatchResult {
    let mut frames = Vec::new();
    // Accumulator for the message currently being built.
    let mut cur: Option<MsgBuilder> = None;

    for idx in 0..frame::CODEWORDS_PER_BATCH {
        let pos = start + idx * 32;
        let Some(mut w) = demod::word_at(bits, pos) else {
            break;
        };
        if inverted {
            w = !w;
        }
        // BCH-correct the codeword; skip ones we can't validate.
        let (cw, corrected) = match bch::correct(w) {
            Some(v) => v,
            None => continue,
        };
        let frame_position = (idx / 2) as u8; // 2 codewords per frame, 8 frames
        match frame::classify(cw, frame_position) {
            Codeword::Address { capcode, function } => {
                // Close out any in-progress message first.
                if let Some(b) = cur.take() {
                    frames.push(b.finish());
                }
                cur = Some(MsgBuilder::new(capcode, function, baud, corrected));
            }
            Codeword::Message { payload20 } => {
                if let Some(b) = cur.as_mut() {
                    b.push_payload(payload20, corrected);
                }
                // Message codewords with no preceding address are orphaned and
                // dropped (cannot attribute them to a capcode).
            }
            Codeword::Idle => {
                if let Some(b) = cur.take() {
                    frames.push(b.finish());
                }
            }
        }
    }
    if let Some(b) = cur.take() {
        frames.push(b.finish());
    }
    BatchResult { frames }
}

/// Accumulates one message: address fields + concatenated message payloads.
struct MsgBuilder {
    capcode: u32,
    function: u8,
    baud: u32,
    payloads: Vec<u32>,
    fec_corrected: u32,
    raw_words: Vec<u32>,
}

impl MsgBuilder {
    fn new(capcode: u32, function: u8, baud: u32, addr_corrected: u32) -> Self {
        let addr_word = bch::encode((((capcode >> 3) << 2) | function as u32) & 0x1F_FFFF);
        Self {
            capcode,
            function,
            baud,
            payloads: Vec::new(),
            fec_corrected: addr_corrected,
            raw_words: vec![addr_word],
        }
    }

    fn push_payload(&mut self, payload20: u32, corrected: u32) {
        // Reconstruct the canonical message codeword for raw output.
        let data21 = (1 << 20) | payload20; // flag=1
        self.raw_words.push(bch::encode(data21));
        self.payloads.push(payload20);
        self.fec_corrected += corrected;
    }

    fn finish(self) -> PocsagFrame {
        let raw = self.raw_words.iter().flat_map(|w| w.to_be_bytes()).collect();
        if self.payloads.is_empty() {
            return PocsagFrame {
                capcode: self.capcode,
                function: self.function,
                baud: self.baud,
                kind: PocsagKind::Tone,
                text: String::new(),
                fec_corrected: self.fec_corrected,
                raw,
            };
        }
        let bits = frame::message_bits(&self.payloads);
        // Function code 3 conventionally selects alphanumeric; others numeric.
        // We decode both and pick by function, which is the operator-signalled
        // class (the spec leaves the numeric/alpha choice to the function bits /
        // paging plan; 3 = alphanumeric is the de-facto convention).
        let (kind, text) = if self.function == 3 {
            (PocsagKind::Alpha, frame::decode_alpha(&bits))
        } else {
            (PocsagKind::Numeric, frame::decode_numeric(&bits))
        };
        PocsagFrame {
            capcode: self.capcode,
            function: self.function,
            baud: self.baud,
            kind,
            text,
            fec_corrected: self.fec_corrected,
            raw,
        }
    }
}

/// Decodes one POCSAG channel out of a wideband capture.
///
/// Mirrors the NAVTEX [`xng_mode_navtex::NavtexChannelDecoder`] contract: owns
/// an internal [`Ddc`] that mixes by `freq_offset_hz` and decimates the capture
/// to [`CHANNEL_RATE`], runs the FSK demod at the configured baud, and emits
/// [`PocsagFrame`]s as complete batches are recovered.
pub struct PocsagChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::FskDemod,
    baud: u32,
    bits: Vec<u8>,
    channel_buf: Vec<Complex<f32>>,
    /// Bit index already scanned for batches (so growing the buffer re-emits
    /// nothing already reported).
    scanned_to: usize,
    /// Dedup keys for messages already emitted.
    seen: Vec<String>,
}

impl PocsagChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`] (a non-integer
    /// multiple is resampled by the DDC). `freq_offset_hz` is the POCSAG
    /// channel center relative to the capture center. `baud` must be one of
    /// 512 / 1200 / 2400.
    pub fn new(input_rate: f64, freq_offset_hz: f64, baud: u32) -> Result<Self, String> {
        if !demod::BAUDS.contains(&(baud as f64)) {
            return Err(format!("unsupported POCSAG baud {baud}; use 512/1200/2400"));
        }
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::FskDemod::new(baud as f64),
            baud,
            bits: Vec::new(),
            channel_buf: Vec::new(),
            scanned_to: 0,
            seen: Vec::new(),
        })
    }

    /// Feed capture IQ; returns newly completed POCSAG frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<PocsagFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.demod.process(channel, &mut self.bits);

        // Re-scan from a small overlap before the last scan point (so a sync
        // straddling a chunk boundary is still found), decode any batches, and
        // dedup against what we've already emitted.
        let start = self.scanned_to.saturating_sub(32 + frame::CODEWORDS_PER_BATCH * 32);
        let decoded = decode_bits(&self.bits[start..], self.baud);
        self.scanned_to = self.bits.len();

        let mut out = Vec::new();
        for f in decoded {
            let key = format!("{}|{}|{}|{}", f.capcode, f.function, f.kind.as_str(), f.text);
            if !self.seen.contains(&key) {
                self.seen.push(key);
                out.push(f);
            }
        }
        out
    }

    /// Smoothed channel power level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Convert a decoded POCSAG frame into the normalized bus message.
///
/// `kind` is the message class (`numeric` / `alpha` / `tone`); `details` is a
/// JSON object with `capcode`, `function`, `baud`, and `text`. `decode.crc_ok`
/// is true (every emitted codeword passed BCH+parity, possibly after
/// correction); `fec_corrected` carries the total bits flipped by BCH.
pub fn to_message(
    f: &PocsagFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let details = serde_json::json!({
        "capcode": f.capcode,
        "function": f.function,
        "baud": f.baud,
        "text": f.text,
    });
    Message {
        mode: Mode::Pocsag,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality {
            crc_ok: true,
            fec_corrected: Some(f.fec_corrected),
            errors: None,
        },
        body: MessageBody::Pocsag { kind: f.kind.as_str().to_string(), details },
        raw: Some(f.raw.clone()),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_rate_is_integer_bit_multiple_for_all_bauds() {
        for &baud in &demod::BAUDS {
            let spb = CHANNEL_RATE / baud;
            assert_eq!(spb.fract(), 0.0, "{baud} Bd → {spb} samples/bit not integer");
        }
        // Output rate must carry the two-sided passband (Nyquist).
        assert!(CHANNEL_RATE >= 2.0 * CHANNEL_PASSBAND_HZ);
    }

    /// End-to-end DECODE test against a SPEC-CONSTRUCTED batch (no modulator):
    /// hand-build an address codeword + alphanumeric message codewords with
    /// correct BCH/parity per ITU-R M.584-2, run the bit-level decoder, and
    /// assert capcode/function/text. This is spec ground truth, not a modulator
    /// round-trip.
    #[test]
    fn decode_bits_recovers_spec_alpha_message() {
        let capcode = 1_234_568u32; // low 3 bits select frame position 0
        let function = 3u8; // alphanumeric
        let frame_position = (capcode & 0x7) as u8;
        assert_eq!(frame_position, 0, "test fixture: capcode must land in frame 0");
        let addr_data = ((capcode >> 3) << 2) | function as u32;
        let addr_cw = bch::encode(addr_data);

        // Encode "HI" as 7-bit LSB-first into 20-bit message payloads.
        let mut msg_bits = Vec::new();
        for &ch in b"HI" {
            for i in 0..7 {
                msg_bits.push((ch >> i) & 1);
            }
        }
        // Pad to a multiple of 20 with 0 (NUL padding, trimmed on decode).
        while msg_bits.len() % 20 != 0 {
            msg_bits.push(0);
        }
        let mut msg_cws = Vec::new();
        for chunk in msg_bits.chunks(20) {
            let mut payload = 0u32;
            for &b in chunk {
                payload = (payload << 1) | b as u32; // MSB-first
            }
            msg_cws.push(bch::encode((1 << 20) | payload));
        }

        // Lay out the batch: address in frame 0 / codeword 0, message after.
        let mut codewords = vec![addr_cw];
        codewords.extend(&msg_cws);
        while codewords.len() < frame::CODEWORDS_PER_BATCH {
            codewords.push(bch::IDLE_CODEWORD);
        }

        // Build the on-air bit stream (preamble + sync + codewords) WITHOUT a
        // modulator — just the bits.
        let bits = modulate::frame_bits(64, &codewords);
        let frames = decode_bits(&bits, 1200);
        assert_eq!(frames.len(), 1, "expected exactly one message");
        let f = &frames[0];
        assert_eq!(f.capcode, capcode);
        assert_eq!(f.function, function);
        assert_eq!(f.kind, PocsagKind::Alpha);
        assert_eq!(f.text, "HI");
    }

    /// Spec-constructed numeric page: digits "0123456789".
    #[test]
    fn decode_bits_recovers_spec_numeric_message() {
        let capcode = 8u32; // addr18=1, frame position 0
        let function = 0u8; // numeric
        let frame_position = (capcode & 0x7) as u8;
        assert_eq!(frame_position, 0);
        let addr_cw = bch::encode((((capcode >> 3) << 2) | function as u32) & 0x1F_FFFF);

        // Numeric: 4 bits per digit; encode so decode's bit-reverse yields the
        // digit value.
        let digits = "0123456789";
        let mut nbits = Vec::new();
        for d in digits.bytes() {
            let val = (d - b'0') as u8;
            for i in 0..4 {
                nbits.push((val >> (3 - i)) & 1);
            }
        }
        // POCSAG pads remaining numeric positions with the "spare"/space code;
        // pad to a 20-bit boundary with 0xC (space) so trailing junk is benign.
        while nbits.len() % 20 != 0 {
            // space code: index 12 = 0b1100 → emit so reverse gives 12.
            for i in 0..4 {
                nbits.push((12u8 >> (3 - i)) & 1);
            }
        }
        let mut msg_cws = Vec::new();
        for chunk in nbits.chunks(20) {
            let mut payload = 0u32;
            for &b in chunk {
                payload = (payload << 1) | b as u32;
            }
            msg_cws.push(bch::encode((1 << 20) | payload));
        }
        let mut codewords = vec![addr_cw];
        codewords.extend(&msg_cws);
        while codewords.len() < frame::CODEWORDS_PER_BATCH {
            codewords.push(bch::IDLE_CODEWORD);
        }
        let bits = modulate::frame_bits(72, &codewords);
        let frames = decode_bits(&bits, 512);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert_eq!(f.capcode, capcode);
        assert_eq!(f.kind, PocsagKind::Numeric);
        // Leading digits must be exactly the page; trailing spaces are padding.
        assert!(f.text.starts_with("0123456789"), "got {:?}", f.text);
    }

    /// Tone-only page: an address codeword with no message codewords.
    #[test]
    fn decode_bits_recovers_tone_page() {
        let capcode = 42u32;
        let function = 1u8;
        let addr_cw = bch::encode((((capcode >> 3) << 2) | function as u32) & 0x1F_FFFF);
        // Per ITU-R M.584-2 §2.2 the address codeword MUST be transmitted in
        // the frame whose number equals the capcode's low 3 bits (here 42 & 7 =
        // 2 → frame 2 → codeword index 4). Earlier slots are idle.
        let frame_position = (capcode & 0x7) as usize;
        let addr_index = frame_position * 2; // 2 codewords per frame, addr in first
        let mut codewords = vec![bch::IDLE_CODEWORD; addr_index];
        codewords.push(addr_cw);
        while codewords.len() < frame::CODEWORDS_PER_BATCH {
            codewords.push(bch::IDLE_CODEWORD);
        }
        let bits = modulate::frame_bits(64, &codewords);
        let frames = decode_bits(&bits, 2400);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, PocsagKind::Tone);
        assert_eq!(frames[0].capcode, capcode);
        assert!(frames[0].text.is_empty());
    }

    #[test]
    fn to_message_emits_pocsag_body() {
        let f = PocsagFrame {
            capcode: 100,
            function: 3,
            baud: 1200,
            kind: PocsagKind::Alpha,
            text: "TEST".into(),
            fec_corrected: 1,
            raw: vec![0xDE, 0xAD],
        };
        let source = Provenance {
            station: xng_types::StationIdentity::new("XX-TEST-POCSAG"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let msg = to_message(&f, 152_000_000, -30.0, source);
        assert_eq!(msg.mode, Mode::Pocsag);
        match &msg.body {
            MessageBody::Pocsag { kind, details } => {
                assert_eq!(kind, "alpha");
                assert_eq!(details["capcode"], 100);
                assert_eq!(details["function"], 3);
                assert_eq!(details["baud"], 1200);
                assert_eq!(details["text"], "TEST");
            }
            other => panic!("expected Pocsag body, got {other:?}"),
        }
        assert!(msg.decode.crc_ok);
        assert_eq!(msg.decode.fec_corrected, Some(1));
    }

    /// SYNTHETIC DEMOD VALIDATION (reported as synthetic): modulate a real
    /// batch to 2-FSK IQ, add complex AWGN at a controlled SNR, demod, and
    /// require the spec message to be recovered intact. Measures frame
    /// recovery + (implicitly) low BER through the full IQ→bits→BCH→text chain.
    #[test]
    fn demod_ber_synth_iq() {
        let baud = 1200u32;
        let capcode = 1_234_568u32;
        let function = 3u8;
        let addr_cw = bch::encode((((capcode >> 3) << 2) | function as u32) & 0x1F_FFFF);
        // "PAGE" alphanumeric.
        let mut msg_bits = Vec::new();
        for &ch in b"PAGE" {
            for i in 0..7 {
                msg_bits.push((ch >> i) & 1);
            }
        }
        while msg_bits.len() % 20 != 0 {
            msg_bits.push(0);
        }
        let mut msg_cws = Vec::new();
        for chunk in msg_bits.chunks(20) {
            let mut payload = 0u32;
            for &b in chunk {
                payload = (payload << 1) | b as u32;
            }
            msg_cws.push(bch::encode((1 << 20) | payload));
        }
        let mut codewords = vec![addr_cw];
        codewords.extend(&msg_cws);
        while codewords.len() < frame::CODEWORDS_PER_BATCH {
            codewords.push(bch::IDLE_CODEWORD);
        }
        // Full preamble (>=576 bits) so the demod's DC/timing loops settle.
        let bits = modulate::frame_bits(600, &codewords);
        let iq = modulate::modulate_iq(&bits, CHANNEL_RATE, baud as f64, 800.0, 1.0);

        // Add AWGN at a moderate SNR; BCH(31,21,2) + per-codeword correction
        // should still recover the spec message. (Synthetic; no real RF.)
        let snr_db = 14.0;
        let noisy = modulate::add_awgn(&iq, snr_db, 0xC0FFEE);

        let mut dec = PocsagChannelDecoder::new(CHANNEL_RATE, 800.0, baud).unwrap();
        let frames = dec.process(&noisy);
        assert!(
            frames.iter().any(|f| f.capcode == capcode
                && f.kind == PocsagKind::Alpha
                && f.text == "PAGE"),
            "synthetic AWGN demod @ {snr_db} dB SNR failed to recover the page; got {frames:?}"
        );
    }

    /// SYNTHETIC raw-BER measurement (reported as synthetic) across all three
    /// bauds. Modulate a known pseudo-random NRZ bit pattern, add complex AWGN
    /// at a controlled SNR, demod, align to the sync codeword, and count bit
    /// errors over the data region. Asserts the raw (pre-FEC) BER is low enough
    /// at a moderate SNR that BCH(31,21,2) can clean up the residual — this is
    /// the demod-layer validation, distinct from the spec-anchored framing
    /// tests. No real-RF IQ is available, so this is purely synthetic.
    #[test]
    fn demod_raw_ber_synth_iq_all_bauds() {
        for &baud in &demod::BAUDS {
            let baud_u = baud as u32;
            // A deterministic pseudo-random payload of codewords (valid BCH so
            // the bit pattern is realistic), preceded by the standard preamble.
            let mut codewords = Vec::new();
            let mut lfsr = 0xACE1u32;
            for _ in 0..frame::CODEWORDS_PER_BATCH {
                // 21 data bits from a tiny LFSR.
                let mut data = 0u32;
                for _ in 0..21 {
                    let bit = (lfsr ^ (lfsr >> 2) ^ (lfsr >> 3) ^ (lfsr >> 5)) & 1;
                    lfsr = (lfsr >> 1) | (bit << 15);
                    data = (data << 1) | bit;
                }
                codewords.push(bch::encode(data & 0x1F_FFFF));
            }
            let tx_bits = modulate::frame_bits(600, &codewords);
            let iq = modulate::modulate_iq(&tx_bits, CHANNEL_RATE, baud, 600.0, 1.0);
            let snr_db = 12.0;
            let noisy = modulate::add_awgn(&iq, snr_db, 0x1234_5678 ^ baud_u as u64);

            // Demod to bits and align on the sync codeword.
            let mut d = demod::FskDemod::new(baud);
            let mut rx = Vec::new();
            d.process(&noisy, &mut rx);
            let (sync_off, inverted) =
                demod::find_sync(&rx, SYNC_MAX_ERR).expect("sync must lock in BER test");
            let data_start = sync_off + 32;

            // Compare the demodulated codeword bits to the transmitted ones.
            let tx_data = &tx_bits[600 + 32..]; // skip preamble + sync
            let mut errors = 0usize;
            let mut total = 0usize;
            for (k, &t) in tx_data.iter().enumerate() {
                let ri = data_start + k;
                if ri >= rx.len() {
                    break;
                }
                let mut r = rx[ri];
                if inverted {
                    r ^= 1;
                }
                if r != t {
                    errors += 1;
                }
                total += 1;
            }
            let ber = errors as f64 / total.max(1) as f64;
            assert!(total > 400, "{baud_u} Bd: too few bits compared ({total})");
            // At 12 dB SNR the raw BER through this discriminator demod should be
            // small (well under 5%); BCH then corrects the rest. This is a
            // synthetic AWGN figure, not a real-RF claim.
            assert!(
                ber < 0.05,
                "{baud_u} Bd @ {snr_db} dB: raw BER {ber:.4} too high ({errors}/{total})"
            );
        }
    }
}
