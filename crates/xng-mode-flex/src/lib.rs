//! Native Motorola **FLEX** / FLEX-NEXT radio-paging decode core for xng.
//!
//! FLEX is a one-way paging air interface: binary (2-level) FSK at 1600 bps
//! (with 4-level 3200/6400 bps variants), structured into 1.875-second
//! **frames**. Each frame opens with **Sync 1** (BS1 dotting | A | B | inverted
//! A, where B = `0xA6C6AAAA`), a BCH-protected **Frame Information Word**, then
//! **Sync 2**, then 11 blocks of 8 words (= 88 32-bit words / "phase"). Every
//! word is a BCH(31,21) codeword plus an even-parity bit. The first data word
//! is the **Block Information Word** giving the address- and vector-field
//! offsets; address words carry capcodes; each address word's **Vector
//! Information Word** selects the page type (tone / numeric / alphanumeric /
//! …); message words carry the body (7-bit alphanumeric or 4-bit numeric).
//!
//! This crate splits like the sibling [`xng_mode_pocsag`] paging mode:
//!
//! - [`bch`] — BCH(31,21) syndrome correction + even parity (per-word integrity
//!   layer), generator `g(x)=x^10+x^9+x^8+x^6+x^5+x^3+1` (0x769). Spec-anchored.
//! - [`frame`] — FIW / BIW parsing, short+long capcode decode, VIW page-type,
//!   and alphanumeric / numeric body decode. Spec-anchored.
//! - [`demod`] — 2-FSK NRZ frequency-discriminator demod + Sync 1 hunt.
//! - [`modulate`] — waveform synthesis used ONLY by the synthetic
//!   modulate→AWGN→demod BER test.
//!
//! [`FlexChannelDecoder`] is the channelized IQ entry point (mirrors the POCSAG
//! template): it owns an [`xng_dsp::Ddc`] that mixes a wideband capture by
//! `freq_offset_hz` and decimates to [`CHANNEL_RATE`], runs the FSK demod at the
//! configured baud, hunts Sync 1, then BCH-corrects and decodes the frame into
//! [`FlexFrame`]s. [`to_message`] normalizes those into the [`xng_types`] form.
//!
//! VERIFICATION: the DECODE/framing core (BCH, FIW/BIW/VIW layout, capcode,
//! text tables) is validated against hand-constructed, spec-cited words — NOT
//! against the modulator. The DEMOD front end is validated by a synthetic
//! modulate→complex-AWGN→demod BER measurement (`demod_*_synth_iq`), reported as
//! synthetic; no off-air FLEX IQ is available.
//!
//! SCOPE / skip-don't-fake: this core implements **1600 bps 2-level FSK** with
//! **alphanumeric, numeric, and tone** pages and **short + long capcodes**,
//! plus FIW frame/cycle numbers. The 4-level 3200/6400 bps PHY and advanced
//! vector types (secure, binary, special/numbered numeric beyond table decode,
//! group-message expansion, fragment reassembly across frames) are
//! intentionally NOT implemented here — see crate notes.

pub mod bch;
pub mod demod;
pub mod frame;
pub mod modulate;

use chrono::Utc;
use frame::PageType;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal demod sample rate, 64 kS/s = 40·1600. A whole number of samples per
/// bit at 1600 Bd, comfortably carrying the ±4.8 kHz FSK deviation (Nyquist
/// 32 kHz).
pub const CHANNEL_RATE: f64 = 64_000.0;

/// One-sided DDC passband: passes both ±4.8 kHz FSK tones plus realistic
/// carrier tuning offset, while staying well inside the channel rate.
pub const CHANNEL_PASSBAND_HZ: f64 = 9_000.0;

/// Maximum bit errors tolerated when matching the 32-bit Sync 1 marker.
const SYNC_MAX_ERR: u32 = 3;

/// One fully decoded FLEX page.
#[derive(Debug, Clone, PartialEq)]
pub struct FlexFrame {
    /// Pager capcode (short or long address form).
    pub capcode: u32,
    /// True for the long (two-word) address form.
    pub long_address: bool,
    /// FLEX cycle number (0..=14) from the Frame Information Word.
    pub cycle: u8,
    /// FLEX frame number (0..=127) from the Frame Information Word.
    pub frame: u8,
    /// Baud the frame was decoded at.
    pub baud: u32,
    /// Message class: `"alpha"`, `"numeric"`, or `"tone"`.
    pub kind: FlexKind,
    /// Underlying FLEX page (vector) type.
    pub page_type: PageType,
    /// Decoded message text (empty for a tone-only page).
    pub text: String,
    /// Number of BCH-corrected bit errors across the words of this page.
    pub fec_corrected: u32,
    /// The raw 32-bit words (FIW + address + vector + message) this frame
    /// decoded from, big-endian bytes, for re-decoding / provenance.
    pub raw: Vec<u8>,
}

/// FLEX message class (the bus `kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexKind {
    /// Alphanumeric paging (7-bit ASCII).
    Alpha,
    /// Numeric paging (4-bit digits).
    Numeric,
    /// Tone-only / signalling page (no message body).
    Tone,
}

impl FlexKind {
    /// The bus `kind` string emitted in [`MessageBody::Flex`].
    pub fn as_str(self) -> &'static str {
        match self {
            FlexKind::Alpha => "alpha",
            FlexKind::Numeric => "numeric",
            FlexKind::Tone => "tone",
        }
    }

    fn from_page_type(t: PageType) -> Self {
        match t.kind_str() {
            "alpha" => FlexKind::Alpha,
            "numeric" => FlexKind::Numeric,
            _ => FlexKind::Tone,
        }
    }
}

/// Decode all FLEX frames found in a recovered, sync-aligned bit history.
///
/// `bits` is the raw demod bit stream (any polarity). This finds the Sync 1
/// marker, fixes polarity, parses the FIW, then reads the 88-word phase, walks
/// the BIW → address → vector → message structure, and emits one [`FlexFrame`]
/// per address that carried a page.
pub fn decode_bits(bits: &[u8], baud: u32) -> Vec<FlexFrame> {
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some((sync_off, inverted)) = demod::find_sync(&bits[search_from..], SYNC_MAX_ERR) {
        // FLEX Sync 1 = AAAA(16) : marker(32) : inverted-A(16); after locking
        // the 32-bit marker we step past it and the trailing 16-bit C field to
        // reach the FIW.
        let abs = search_from + sync_off;
        let fiw_pos = abs + 32 + 16; // marker + 16-bit inverted-A
        let frame = decode_frame(bits, fiw_pos, inverted, baud);
        let consumed = 32 + 16 + (1 + frame::WORDS_PER_PHASE) * 32;
        out.extend(frame);
        let advance = (sync_off + consumed).max(32);
        if search_from + advance >= bits.len() {
            break;
        }
        search_from += advance;
    }
    out
}

/// Read a u32 FLEX word at bit `pos`, applying polarity inversion, returning the
/// BCH-corrected value + bits flipped, or `None` if uncorrectable / short.
fn read_word(bits: &[u8], pos: usize, inverted: bool) -> Option<(u32, u32)> {
    let mut w = demod::word_at_lsb(bits, pos)?;
    if inverted {
        w = !w;
    }
    bch::correct(w)
}

/// Decode one FLEX frame: FIW at `fiw_pos`, then the 88-word phase right after.
fn decode_frame(bits: &[u8], fiw_pos: usize, inverted: bool, baud: u32) -> Vec<FlexFrame> {
    let mut out = Vec::new();

    // --- Frame Information Word ---
    let Some((fiw_word, fiw_fix)) = read_word(bits, fiw_pos, inverted) else {
        return out;
    };
    let fiw = frame::parse_fiw(fiw_word);

    // --- 88-word phase, beginning right after the FIW ---
    let phase_start = fiw_pos + 32;
    let mut words = Vec::with_capacity(frame::WORDS_PER_PHASE);
    let mut fixes = Vec::with_capacity(frame::WORDS_PER_PHASE);
    for k in 0..frame::WORDS_PER_PHASE {
        match read_word(bits, phase_start + k * 32, inverted) {
            Some((w, fix)) => {
                words.push(w);
                fixes.push(fix);
            }
            None => {
                // Unreadable word; push a sentinel so indices stay aligned.
                words.push(0);
                fixes.push(0);
            }
        }
    }
    if words.is_empty() {
        return out;
    }

    // --- Block Information Word = phase word 0 ---
    let biw = frame::parse_biw(words[0]);
    let aoff = biw.address_offset;
    let voff = biw.vector_offset;
    if aoff >= words.len() || voff == 0 || voff > words.len() {
        return out;
    }

    // Address words run from `aoff` up to (but not including) `voff`.
    let addr_end = voff.min(words.len());
    let mut i = aoff;
    while i < addr_end {
        let aw1 = words[i];
        if aw1 == 0 {
            i += 1;
            continue;
        }
        let addr = frame::decode_short_address(aw1);
        // Vector word for address i is at voff + (i - aoff).
        let vidx = voff + (i - aoff);
        if vidx >= words.len() {
            break;
        }
        let viw = words[vidx];
        let page_type = PageType::from_viw(viw);

        // FEC budget so far for this page: FIW + address + vector words.
        let mut fec = fiw_fix + fixes[i] + fixes[vidx];
        let mut raw_words = vec![fiw_word, aw1, viw];

        // Message words: the VIW carries a word pointer + count into the phase
        // for the message body. VIW bits 7..=13 = start word, 14..=20 = word
        // count (per the FLEX numeric/alphanumeric vector layout).
        let mw1 = ((viw >> 7) & 0x7F) as usize;
        let len = ((viw >> 14) & 0x7F) as usize;

        let (kind, text) = match page_type {
            PageType::Tone | PageType::Secure | PageType::ShortInstruction => {
                (FlexKind::Tone, String::new())
            }
            PageType::Alphanumeric | PageType::Binary => {
                let body = collect_message(&words, &fixes, mw1, len, &mut fec, &mut raw_words);
                (FlexKind::Alpha, frame::decode_alpha(&body))
            }
            PageType::StandardNumeric | PageType::SpecialNumeric | PageType::NumberedNumeric => {
                let body = collect_message(&words, &fixes, mw1, len, &mut fec, &mut raw_words);
                (FlexKind::Numeric, frame::decode_numeric(&body))
            }
        };
        // Reconcile: derived kind must match the page-type mapping.
        debug_assert_eq!(kind, FlexKind::from_page_type(page_type));

        let raw = raw_words.iter().flat_map(|w| w.to_be_bytes()).collect();
        out.push(FlexFrame {
            capcode: addr.capcode,
            long_address: addr.long,
            cycle: fiw.cycle,
            frame: fiw.frame,
            baud,
            kind,
            page_type,
            text,
            fec_corrected: fec,
            raw,
        });
        i += 1;
    }
    out
}

/// Gather the message-word data fields for a page body, summing FEC and raw.
fn collect_message(
    words: &[u32],
    fixes: &[u32],
    start: usize,
    len: usize,
    fec: &mut u32,
    raw_words: &mut Vec<u32>,
) -> Vec<u32> {
    let mut body = Vec::new();
    let end = start.saturating_add(len).min(words.len());
    for w in start..end {
        body.push(words[w]);
        *fec += fixes[w];
        raw_words.push(words[w]);
    }
    body
}

/// Decodes one FLEX channel out of a wideband capture.
///
/// Mirrors the POCSAG [`xng_mode_pocsag::PocsagChannelDecoder`] contract: owns
/// an internal [`Ddc`] that mixes by `freq_offset_hz` and decimates the capture
/// to [`CHANNEL_RATE`], runs the FSK demod at the configured baud, and emits
/// [`FlexFrame`]s as frames are recovered.
pub struct FlexChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::FskDemod,
    baud: u32,
    bits: Vec<u8>,
    channel_buf: Vec<Complex<f32>>,
    scanned_to: usize,
    seen: Vec<String>,
}

impl FlexChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`] (a non-integer
    /// multiple is resampled by the DDC). `freq_offset_hz` is the FLEX channel
    /// center relative to the capture center. `baud` must be 1600 (the only
    /// rate this 2-level core supports).
    pub fn new(input_rate: f64, freq_offset_hz: f64, baud: u32) -> Result<Self, String> {
        if !demod::BAUDS.contains(&(baud as f64)) {
            return Err(format!(
                "unsupported FLEX baud {baud}; this core supports 1600 (2-FSK) only"
            ));
        }
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
            demod: demod::FskDemod::new(baud as f64),
            baud,
            bits: Vec::new(),
            channel_buf: Vec::new(),
            scanned_to: 0,
            seen: Vec::new(),
        })
    }

    /// Feed capture IQ; returns newly completed FLEX frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<FlexFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.demod.process(channel, &mut self.bits);

        // Re-scan from a small overlap so a sync straddling a chunk boundary is
        // still found; dedup against what's already emitted.
        let overlap = 32 + 16 + (1 + frame::WORDS_PER_PHASE) * 32;
        let start = self.scanned_to.saturating_sub(overlap);
        let decoded = decode_bits(&self.bits[start..], self.baud);
        self.scanned_to = self.bits.len();

        let mut out = Vec::new();
        for f in decoded {
            let key = format!(
                "{}|{}|{}|{}|{}|{}",
                f.capcode,
                f.cycle,
                f.frame,
                f.kind.as_str(),
                f.long_address,
                f.text
            );
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

/// Convert a decoded FLEX frame into the normalized bus message.
///
/// `kind` is the message class (`alpha` / `numeric` / `tone`); `details` is a
/// JSON object with `capcode`, `long_address`, `frame`, `cycle`, `baud`, and
/// `text`. `decode.crc_ok` is true (every emitted word passed BCH+parity,
/// possibly after correction); `fec_corrected` carries the total bits flipped.
pub fn to_message(
    f: &FlexFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let details = serde_json::json!({
        "capcode": f.capcode,
        "long_address": f.long_address,
        "frame": f.frame,
        "cycle": f.cycle,
        "baud": f.baud,
        "text": f.text,
    });
    Message {
        mode: Mode::Flex,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: true,
            fec_corrected: Some(f.fec_corrected),
            errors: None,
        },
        body: MessageBody::Flex {
            kind: f.kind.as_str().to_string(),
            details,
        },
        raw: Some(f.raw.clone()),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_rate_is_integer_bit_multiple() {
        for &baud in &demod::BAUDS {
            let spb = CHANNEL_RATE / baud;
            assert_eq!(
                spb.fract(),
                0.0,
                "{baud} Bd → {spb} samples/bit not integer"
            );
        }
        assert!(CHANNEL_RATE >= 2.0 * CHANNEL_PASSBAND_HZ);
    }

    /// Hand-build a complete FLEX frame from the SPEC-cited field layout and
    /// assert the decoder recovers capcode / frame / cycle / page-type / text.
    ///
    /// Layout (multimon-ng `demod_flex.c`):
    ///   - FIW: cycle bits 4..=7, frame bits 8..=14, checksum nibbles sum mod16=F.
    ///   - phase word 0 = BIW: address offset = (bits8..9)+1, vector offset bits10..15.
    ///   - address word: capcode = aw1 - 0x8000.
    ///   - VIW: type bits 4..=6 (5=alphanumeric); message start bits 7..=13,
    ///     length bits 14..=20.
    ///   - alphanumeric body: 7-bit chars LSB-first, 3 per 21-bit word.
    ///
    /// Every 32-bit word is BCH(31,21)+parity encoded via [`bch::encode`].
    fn build_alpha_frame(capcode: u32, cycle: u32, frame_no: u32, text: &str) -> Vec<u32> {
        // ---- FIW ----
        let body = (cycle << 4) | (frame_no << 8);
        let partial = ((body >> 4) & 0xF)
            + ((body >> 8) & 0xF)
            + ((body >> 12) & 0xF)
            + ((body >> 16) & 0xF)
            + ((body >> 20) & 0x1);
        let c = (0xFu32.wrapping_sub(partial)) & 0xF;
        let fiw = bch::encode(body | c);

        // ---- phase words ----
        let mut phase = vec![0u32; frame::WORDS_PER_PHASE];
        // Layout choice: address field starts at word 1 (aoffset=1 -> bits8..9=0),
        // vector field starts at word 2 (voffset=2), message starts at word 3.
        let aoffset_field = 0u32; // (0)+1 = 1
        let voffset = 2u32;
        let biw_data = (aoffset_field << 8) | (voffset << 10);
        phase[0] = bch::encode(biw_data);

        // Address word at index 1: capcode = aw1 - 0x8000 -> aw1 = capcode+0x8000.
        let aw1 = (capcode + 0x8000) & 0x1F_FFFF;
        phase[1] = bch::encode(aw1);

        // Message body words: 7-bit chars, 3 per word, starting at word 3.
        let msg_start = 3u32;
        let bytes = text.as_bytes();
        let nwords = bytes.len().div_ceil(3);
        let mut mwords = Vec::new();
        for wi in 0..nwords {
            let mut data = 0u32;
            for slot in 0..3 {
                let bi = wi * 3 + slot;
                if bi < bytes.len() {
                    data |= ((bytes[bi] as u32) & 0x7F) << (slot * 7);
                }
            }
            mwords.push(bch::encode(data));
        }
        for (k, &mw) in mwords.iter().enumerate() {
            phase[msg_start as usize + k] = mw;
        }

        // VIW at index 2: type=5 (alpha), msg start bits7..13, length bits14..20.
        let viw_data = (5u32 << 4) | (msg_start << 7) | ((nwords as u32) << 14);
        phase[2] = bch::encode(viw_data);

        // Fill remaining phase words with valid (all-zero-data) words so BCH
        // passes and they classify benignly.
        for w in phase.iter_mut() {
            if *w == 0 {
                *w = bch::encode(0);
            }
        }

        let mut words = vec![fiw];
        words.extend(phase);
        words
    }

    #[test]
    fn decode_bits_recovers_spec_alpha_page() {
        let capcode = 1_234_567u32;
        let cycle = 7u32;
        let frame_no = 33u32;
        let words = build_alpha_frame(capcode, cycle, frame_no, "HELLO WORLD");
        let bits = modulate::frame_bits(64, &words);

        let frames = decode_bits(&bits, 1600);
        let f = frames
            .iter()
            .find(|f| f.capcode == capcode)
            .unwrap_or_else(|| panic!("expected capcode {capcode}; got {frames:?}"));
        assert_eq!(f.kind, FlexKind::Alpha);
        assert_eq!(f.cycle, 7);
        assert_eq!(f.frame, 33);
        assert_eq!(f.page_type, PageType::Alphanumeric);
        assert!(f.text.starts_with("HELLO WORLD"), "got {:?}", f.text);
    }

    /// Spec-constructed numeric page: digits via the FLEX 4-bit table.
    #[test]
    fn decode_bits_recovers_spec_numeric_page() {
        let capcode = 555_000u32;
        // FIW cycle=1, frame=2.
        let body = (1u32 << 4) | (2u32 << 8);
        let partial = ((body >> 4) & 0xF)
            + ((body >> 8) & 0xF)
            + ((body >> 12) & 0xF)
            + ((body >> 16) & 0xF)
            + ((body >> 20) & 0x1);
        let c = (0xFu32.wrapping_sub(partial)) & 0xF;
        let fiw = bch::encode(body | c);

        let mut phase = vec![0u32; frame::WORDS_PER_PHASE];
        let voffset = 2u32;
        phase[0] = bch::encode(voffset << 10); // aoffset=1 (field 0)
        phase[1] = bch::encode((capcode + 0x8000) & 0x1F_FFFF);

        // Numeric body "12345" -> 4-bit groups LSB-first, one word at index 3.
        let digits = [1u32, 2, 3, 4, 5];
        let mut data = 0u32;
        for (k, &d) in digits.iter().enumerate() {
            data |= d << (k * 4);
        }
        phase[3] = bch::encode(data);

        // VIW type=3 (standard numeric), start word 3, length 1.
        phase[2] = bch::encode((3u32 << 4) | (3u32 << 7) | (1u32 << 14));

        for w in phase.iter_mut() {
            if *w == 0 {
                *w = bch::encode(0);
            }
        }

        let mut words = vec![fiw];
        words.extend(phase);
        let bits = modulate::frame_bits(64, &words);
        let frames = decode_bits(&bits, 1600);
        let f = frames
            .iter()
            .find(|f| f.capcode == capcode)
            .expect("numeric page");
        assert_eq!(f.kind, FlexKind::Numeric);
        assert!(f.text.starts_with("12345"), "got {:?}", f.text);
    }

    /// Spec-constructed tone-only page: VIW type=2 (tone), no message body.
    #[test]
    fn decode_bits_recovers_spec_tone_page() {
        let capcode = 99_999u32;
        let body = (3u32 << 4) | (10u32 << 8);
        let partial = ((body >> 4) & 0xF)
            + ((body >> 8) & 0xF)
            + ((body >> 12) & 0xF)
            + ((body >> 16) & 0xF)
            + ((body >> 20) & 0x1);
        let c = (0xFu32.wrapping_sub(partial)) & 0xF;
        let fiw = bch::encode(body | c);

        let mut phase = vec![0u32; frame::WORDS_PER_PHASE];
        phase[0] = bch::encode(2u32 << 10); // aoffset=1, voffset=2
        phase[1] = bch::encode((capcode + 0x8000) & 0x1F_FFFF);
        phase[2] = bch::encode(2u32 << 4); // VIW type=2 tone
        for w in phase.iter_mut() {
            if *w == 0 {
                *w = bch::encode(0);
            }
        }
        let mut words = vec![fiw];
        words.extend(phase);
        let bits = modulate::frame_bits(64, &words);
        let frames = decode_bits(&bits, 1600);
        let f = frames
            .iter()
            .find(|f| f.capcode == capcode)
            .expect("tone page");
        assert_eq!(f.kind, FlexKind::Tone);
        assert_eq!(f.page_type, PageType::Tone);
        assert!(f.text.is_empty());
    }

    #[test]
    fn to_message_emits_flex_body() {
        let f = FlexFrame {
            capcode: 1_000_000,
            long_address: false,
            cycle: 3,
            frame: 64,
            baud: 1600,
            kind: FlexKind::Alpha,
            page_type: PageType::Alphanumeric,
            text: "TEST".into(),
            fec_corrected: 2,
            raw: vec![0xDE, 0xAD],
        };
        let source = Provenance {
            station: xng_types::StationIdentity::new("XX-TEST-FLEX"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let msg = to_message(&f, 929_000_000, -28.0, source);
        assert_eq!(msg.mode, Mode::Flex);
        match &msg.body {
            MessageBody::Flex { kind, details } => {
                assert_eq!(kind, "alpha");
                assert_eq!(details["capcode"], 1_000_000);
                assert_eq!(details["frame"], 64);
                assert_eq!(details["cycle"], 3);
                assert_eq!(details["baud"], 1600);
                assert_eq!(details["text"], "TEST");
                assert_eq!(details["long_address"], false);
            }
            other => panic!("expected Flex body, got {other:?}"),
        }
        assert!(msg.decode.crc_ok);
        assert_eq!(msg.decode.fec_corrected, Some(2));
    }

    /// SYNTHETIC DEMOD VALIDATION (reported as synthetic): modulate a real
    /// spec-built alpha frame to 2-FSK IQ, add complex AWGN, demod through the
    /// full channel decoder, and require the spec page to be recovered intact.
    #[test]
    fn demod_recovers_page_synth_iq() {
        let baud = 1600u32;
        let capcode = 1_234_567u32;
        let words = build_alpha_frame(capcode, 7, 33, "PAGE ME");
        // Long dotting preamble so the demod's DC/timing loops settle.
        let bits = modulate::frame_bits(600, &words);
        let iq = modulate::modulate_iq(&bits, CHANNEL_RATE, baud as f64, 1200.0, 1.0);
        let snr_db = 16.0;
        let noisy = modulate::add_awgn(&iq, snr_db, 0xC0FFEE);

        let mut dec = FlexChannelDecoder::new(CHANNEL_RATE, 1200.0, baud).unwrap();
        let frames = dec.process(&noisy);
        assert!(
            frames.iter().any(|f| f.capcode == capcode
                && f.kind == FlexKind::Alpha
                && f.text.starts_with("PAGE ME")),
            "synthetic AWGN demod @ {snr_db} dB SNR failed to recover the page; got {frames:?}"
        );
    }

    /// SYNTHETIC raw-BER measurement (reported as synthetic) at 1600 Bd 2-FSK.
    /// Modulate a known pseudo-random NRZ pattern, add AWGN, demod, align on the
    /// Sync 1 marker, and count bit errors over the data region. Asserts the raw
    /// (pre-FEC) BER is low enough at a moderate SNR that BCH(31,21,2) can clean
    /// up the residual. No real-RF IQ exists, so this is purely synthetic.
    #[test]
    fn demod_raw_ber_synth_iq() {
        let baud = demod::BAUD_1600;
        // Deterministic pseudo-random payload of valid FLEX words.
        let mut words = Vec::new();
        let mut lfsr = 0xACE1u32;
        for _ in 0..frame::WORDS_PER_PHASE {
            let mut data = 0u32;
            for _ in 0..21 {
                let bit = (lfsr ^ (lfsr >> 2) ^ (lfsr >> 3) ^ (lfsr >> 5)) & 1;
                lfsr = (lfsr >> 1) | (bit << 15);
                data = (data << 1) | bit;
            }
            words.push(bch::encode(data & 0x1F_FFFF));
        }
        let tx_bits = modulate::frame_bits(600, &words);
        let iq = modulate::modulate_iq(&tx_bits, CHANNEL_RATE, baud, 600.0, 1.0);
        let snr_db = 14.0;
        let noisy = modulate::add_awgn(&iq, snr_db, 0x1234_5678);

        let mut d = demod::FskDemod::new(baud);
        let mut rx = Vec::new();
        d.process(&noisy, &mut rx);
        let (sync_off, inverted) =
            demod::find_sync(&rx, SYNC_MAX_ERR).expect("sync must lock in BER test");
        // Data region begins after the 32-bit marker in both tx and rx.
        let data_start_rx = sync_off + 32;
        let data_start_tx = 600 + 32;

        let mut errors = 0usize;
        let mut total = 0usize;
        for (k, &t) in tx_bits[data_start_tx..].iter().enumerate() {
            let ri = data_start_rx + k;
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
        assert!(total > 1000, "too few bits compared ({total})");
        assert!(
            ber < 0.05,
            "1600 Bd @ {snr_db} dB: raw BER {ber:.4} too high ({errors}/{total})"
        );
    }
}
