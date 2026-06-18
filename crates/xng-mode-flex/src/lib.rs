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
//! - [`demod`] — 2-FSK NRZ frequency-discriminator demod + Sync 1 hunt, **plus**
//!   a 4-level FSK symbol slicer (Gardner timing), the per-rate Sync 1 A-code
//!   mode resolver, the 4-level symbol→dibit Gray map, and the A/B/C/D phase
//!   de-interleave for the 3200 / 6400 bps modes.
//! - [`modulate`] — 2-FSK and 4-FSK waveform synthesis used ONLY by the
//!   synthetic modulate→AWGN→demod BER tests.
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
//! SCOPE / skip-don't-fake: this core implements **1600 bps 2-level FSK** and
//! **3200 / 6400 bps 4-level FSK** (1600 sym/s → Phases A,B; 3200 sym/s →
//! Phases A,B,C,D), with **alphanumeric, numeric, and tone** pages and **short +
//! long capcodes**, plus FIW frame/cycle numbers. The 3200-bps **2-level** mode
//! (3200 sym/s, A-code `0x7B18`, Phases A,C) is recognized by the A-code table
//! but NOT decoded; advanced vector types (secure, binary, special/numbered
//! numeric beyond table decode, group-message expansion, fragment reassembly
//! across frames) are intentionally NOT implemented here — see crate notes.

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

/// Decode all FLEX frames found in a 4-level **symbol** stream (0..=3).
///
/// This is the 4-FSK counterpart of [`decode_bits`]. It hunts the 64-bit Sync 1
/// (whose 16-bit A-code selects the on-air mode — must be a 4-level mode, i.e.
/// 3200 or 6400 information bps), reads the shared 32-bit FIW at the symbol
/// rate, skips Sync 2, then de-interleaves the 1760 ms DATA section into the
/// mode's phase buffers (A,B or A,B,C,D) and decodes each phase through the same
/// BIW → address → vector → message core as the 1600-bps path.
///
/// `expected_baud` is the information bit rate the channel was opened at (3200
/// or 6400); a sync whose A-code resolves to a different rate is skipped so a
/// 6400-channel does not mis-decode a 3200 burst.
pub fn decode_symbols(syms: &[u8], expected_baud: u32) -> Vec<FlexFrame> {
    let mut out = Vec::new();
    // Sync hunt operates on the per-symbol sync bit (sym<2).
    let sync_bits: Vec<u8> = syms.iter().map(|&s| demod::symbol_sync_bit(s)).collect();

    let mut search_from = 0usize;
    while let Some((sync_off, inverted, mode)) =
        demod::find_sync_mode(&sync_bits[search_from..], SYNC_MAX_ERR)
    {
        let abs = search_from + sync_off;
        // Only accept syncs matching the channel's configured 4-level rate.
        if mode.levels != 4 || mode.baud() != expected_baud {
            search_from = abs + 64;
            if search_from + 64 >= sync_bits.len() {
                break;
            }
            continue;
        }

        // Sync-1 spans 64 symbols. Then 16 dotting symbols, then 32 FIW symbols
        // (multimon-ng: read_2fsk starts at fiwcount>=16, decodes at 48), then
        // Sync-2 (25 ms = 40 symbols @1600 sym/s, 80 @3200), then DATA.
        let fiw_start = abs + 64 + 16;
        let data_start = abs + 64 + 48 + sync2_symbols(mode.sym_rate);
        let n_data = demod::data_symbols(mode.sym_rate);
        if data_start + n_data > syms.len() {
            break;
        }

        // --- FIW: 32 symbols at the symbol rate, bit_a only (sym>1). ---
        if let Some((fiw_word, fiw_fix)) =
            read_fiw_symbols(&syms[fiw_start..fiw_start + 32], inverted)
        {
            let data = &syms[data_start..data_start + n_data];
            let phases = demod::deinterleave_phases(data, mode, inverted);
            for phase in &phases {
                let mut words = Vec::with_capacity(frame::WORDS_PER_PHASE);
                let mut fixes = Vec::with_capacity(frame::WORDS_PER_PHASE);
                for &raw in phase {
                    match bch::correct(raw) {
                        Some((w, fix)) => {
                            words.push(w & 0x001F_FFFF);
                            fixes.push(fix);
                        }
                        None => {
                            words.push(0);
                            fixes.push(0);
                        }
                    }
                }
                let mut frames = decode_phase_words(fiw_word, fiw_fix, &words, &fixes, expected_baud);
                out.append(&mut frames);
            }
        }

        let advance = sync_off + 64 + 48 + sync2_symbols(mode.sym_rate) + n_data;
        if search_from + advance >= syms.len() {
            break;
        }
        search_from += advance.max(64);
    }
    out
}

/// Sync-2 length in symbols: 25 ms at the symbol rate.
/// (multimon-ng: "25 ms = 40 bits @ 1600 bps, 80 @ 3200 bps".)
fn sync2_symbols(sym_rate: u32) -> usize {
    (sym_rate as usize) * 25 / 1000
}

/// Read the 32-symbol FIW: each symbol contributes one bit via `sym>1`
/// (multimon-ng `read_2fsk`: `(*dat>>1) | ((sym>1)?0x80000000:0)`), shifted in
/// MSB-first so the first symbol lands at bit 0 after 32 shifts. Returns the
/// BCH-corrected FIW word + bits flipped, or `None` if uncorrectable.
fn read_fiw_symbols(syms: &[u8], inverted: bool) -> Option<(u32, u32)> {
    if syms.len() < 32 {
        return None;
    }
    let mut dat = 0u32;
    for &raw in &syms[..32] {
        let sym = if inverted { 3 - raw.min(3) } else { raw };
        dat = (dat >> 1) | (if sym > 1 { 0x8000_0000 } else { 0 });
    }
    bch::correct(dat)
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
    // --- Frame Information Word ---
    let Some((fiw_word, fiw_fix)) = read_word(bits, fiw_pos, inverted) else {
        return Vec::new();
    };

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
    decode_phase_words(fiw_word, fiw_fix, &words, &fixes, baud)
}

/// Decode one already-assembled FLEX phase given the (corrected) FIW word and
/// its 88 (corrected) phase words + per-word BCH fix counts.
///
/// This is the shared core for both the 1600-bps 2-FSK bit-stream path
/// ([`decode_frame`]) and the 4-FSK de-interleaved phase path
/// ([`decode_symbols`]): both produce a corrected FIW and 88 corrected words, so
/// the BIW → address → vector → message walk lives here.
fn decode_phase_words(
    fiw_word: u32,
    fiw_fix: u32,
    words: &[u32],
    fixes: &[u32],
    baud: u32,
) -> Vec<FlexFrame> {
    let mut out = Vec::new();
    let fiw = frame::parse_fiw(fiw_word);
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
                let body = collect_message(words, fixes, mw1, len, &mut fec, &mut raw_words);
                (FlexKind::Alpha, frame::decode_alpha(&body))
            }
            PageType::StandardNumeric | PageType::SpecialNumeric | PageType::NumberedNumeric => {
                let body = collect_message(words, fixes, mw1, len, &mut fec, &mut raw_words);
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
    /// Resolved on-air mode (symbol rate + levels) for the configured baud.
    mode: demod::FlexMode,
    /// 2-level NRZ bit demod (1600 bps path).
    bit_demod: Option<demod::FskDemod>,
    /// 4-level symbol demod (3200 / 6400 bps path).
    sym_demod: Option<demod::SymbolDemod>,
    baud: u32,
    /// Recovered bits (2-level) or symbols 0..=3 (4-level).
    stream: Vec<u8>,
    channel_buf: Vec<Complex<f32>>,
    scanned_to: usize,
    seen: Vec<String>,
}

impl FlexChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`] (a non-integer
    /// multiple is resampled by the DDC). `freq_offset_hz` is the FLEX channel
    /// center relative to the capture center. `baud` is the information bit
    /// rate: **1600** (2-FSK), **3200** (4-FSK, 1600 sym/s, Phases A/B), or
    /// **6400** (4-FSK, 3200 sym/s, Phases A/B/C/D).
    pub fn new(input_rate: f64, freq_offset_hz: f64, baud: u32) -> Result<Self, String> {
        let mode = demod::FlexMode::from_baud(baud).ok_or_else(|| {
            format!("unsupported FLEX baud {baud}; supported: 1600 (2-FSK), 3200 & 6400 (4-FSK)")
        })?;
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
        let (bit_demod, sym_demod) = if mode.levels == 2 {
            (Some(demod::FskDemod::new(mode.sym_rate as f64)), None)
        } else {
            (None, Some(demod::SymbolDemod::new(mode.sym_rate as f64)))
        };
        Ok(Self {
            ddc,
            mode,
            bit_demod,
            sym_demod,
            baud,
            stream: Vec::new(),
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
        match (&mut self.bit_demod, &mut self.sym_demod) {
            (Some(d), _) => d.process(channel, &mut self.stream),
            (_, Some(d)) => d.process(channel, &mut self.stream),
            _ => unreachable!("decoder has neither demod"),
        }

        let decoded = if self.mode.levels == 2 {
            // Re-scan from a small overlap so a sync straddling a chunk boundary
            // is still found; dedup against what's already emitted.
            let overlap = 32 + 16 + (1 + frame::WORDS_PER_PHASE) * 32;
            let start = self.scanned_to.saturating_sub(overlap);
            let d = decode_bits(&self.stream[start..], self.baud);
            self.scanned_to = self.stream.len();
            d
        } else {
            // 4-level: a full frame spans Sync1+FIW+Sync2+DATA symbols; overlap
            // by one whole frame so a sync near a boundary still completes.
            let overlap = 64 + 48 + 80 + demod::data_symbols(self.mode.sym_rate);
            let start = self.scanned_to.saturating_sub(overlap);
            let d = decode_symbols(&self.stream[start..], self.baud);
            self.scanned_to = self.stream.len();
            d
        };

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
        match (&self.bit_demod, &self.sym_demod) {
            (Some(d), _) => d.level_dbfs(),
            (_, Some(d)) => d.level_dbfs(),
            _ => f32::NEG_INFINITY,
        }
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

    // ---------------------------------------------------------------------
    // 4-level FSK (3200 / 6400 bps) tests
    // ---------------------------------------------------------------------

    /// INDEPENDENT spec re-derivation of the FLEX column interleave, used only by
    /// the 4-FSK ground-truth decode test so that test does NOT rely on
    /// [`modulate::interleave_phases`] (which is the literal inverse of the
    /// de-interleaver — using it would be a self-consistency loopback).
    ///
    /// Directly from multimon-ng `read_data`: symbol counter `c` addresses word
    /// `idx = ((c>>5)&0xFFF8)|(c&7)`; bits shift in MSB-first so the k-th bit
    /// into a word lands at bit position k, where for the k-th visit within an
    /// 8-word block the pass index is `(c mod 256)/8`. bit_a→PhaseA(/C),
    /// bit_b→PhaseB(/D); at 3200 sym/s consecutive symbols alternate A/B vs C/D.
    fn interleave_phases_spec(mode: demod::FlexMode, phases: &[Vec<u32>]) -> Vec<u8> {
        let n = demod::data_symbols(mode.sym_rate);
        let four = mode.levels == 4;
        let two_phase = mode.sym_rate == 3200;
        // multimon-ng table inverse: (bit_a,bit_b) -> symbol.
        let dibit_sym = |a: u8, b: u8| match (a & 1, b & 1) {
            (0, 0) => 0u8,
            (0, 1) => 1,
            (1, 1) => 2,
            (1, 0) => 3,
            _ => unreachable!(),
        };
        let bit = |p: &Vec<u32>, idx: usize, pos: usize| ((p[idx] >> pos) & 1) as u8;

        let mut out = Vec::with_capacity(n);
        let mut counter: u32 = 0;
        let mut toggle = 0u8;
        while out.len() < n {
            let idx = (((counter >> 5) & 0xFFF8) | (counter & 7)) as usize;
            let pos = ((counter % 256) / 8) as usize;
            let (pa, pb) = if two_phase && toggle == 1 { (2, 3) } else { (0, 1) };
            let a = bit(&phases[pa], idx, pos);
            let b = if four { bit(&phases[pb], idx, pos) } else { 0 };
            out.push(dibit_sym(a, b));
            if two_phase {
                if toggle == 1 {
                    counter += 1;
                    toggle = 0;
                } else {
                    toggle = 1;
                }
            } else {
                counter += 1;
            }
        }
        out
    }

    /// Build the full 4-level symbol frame (Sync1 A-code + FIW + Sync2 + DATA)
    /// using the INDEPENDENT spec interleave above, NOT the modulator helper.
    fn build_symbol_frame_spec(
        mode: demod::FlexMode,
        a_code: u16,
        fiw_word: u32,
        phases: &[Vec<u32>],
        dotting: usize,
    ) -> Vec<u8> {
        // Sync bits are read as `(sym<2)`: bit "1" -> low tone (sym 0).
        let sync_sym = |b: u8| if b != 0 { 0u8 } else { 3u8 };
        // FIW bits are read as `(sym>1)` (bit_a): bit "1" -> high tone (sym 3).
        let fiw_sym = |b: u8| if b != 0 { 3u8 } else { 0u8 };
        let mut s = Vec::new();
        for i in 0..dotting {
            s.push(if i % 2 == 0 { 3 } else { 0 });
        }
        let sync64: u64 = ((a_code as u64) << 48)
            | ((frame::SYNC_MARKER_B as u64) << 16)
            | ((!a_code) as u64 & 0xFFFF);
        for i in (0..64).rev() {
            s.push(sync_sym(((sync64 >> i) & 1) as u8));
        }
        for i in 0..16 {
            s.push(if i % 2 == 0 { 3 } else { 0 });
        }
        // FIW: emit bit 0 first (LSB-first), 32 symbols.
        for i in 0..32 {
            s.push(fiw_sym(((fiw_word >> i) & 1) as u8));
        }
        let sync2 = (mode.sym_rate as usize) * 25 / 1000;
        for i in 0..sync2 {
            s.push(if i % 2 == 0 { 3 } else { 0 });
        }
        s.extend(interleave_phases_spec(mode, phases));
        s
    }

    /// SPEC-CITED DECODE (3200 bps, 4-FSK, Phases A/B): hand-build a real FLEX
    /// alpha page in Phase A from the cited field layout, place it in a symbol
    /// frame via the INDEPENDENT spec interleave, and assert `decode_symbols`
    /// recovers the exact capcode / cycle / frame / page-type / text. This
    /// validates the symbol→dibit map, the de-interleave geometry, the FIW
    /// read, and the shared word/page core — against spec, not against the demod.
    #[test]
    fn decode_symbols_recovers_spec_alpha_page_3200() {
        let capcode = 1_234_567u32;
        let cycle = 7u32;
        let frame_no = 33u32;
        let words = build_alpha_frame(capcode, cycle, frame_no, "HELLO WORLD");
        let fiw_word = words[0];
        let phase_a: Vec<u32> = words[1..].to_vec();
        assert_eq!(phase_a.len(), frame::WORDS_PER_PHASE);
        let phase_b = vec![0u32; frame::WORDS_PER_PHASE]; // idle phase

        let mode = demod::FlexMode::from_baud(3200).unwrap();
        let syms = build_symbol_frame_spec(
            mode,
            demod::A_CODE_1600_4,
            fiw_word,
            &[phase_a, phase_b],
            64,
        );

        let frames = decode_symbols(&syms, 3200);
        let f = frames
            .iter()
            .find(|f| f.capcode == capcode)
            .unwrap_or_else(|| panic!("expected capcode {capcode}; got {frames:?}"));
        assert_eq!(f.kind, FlexKind::Alpha);
        assert_eq!(f.cycle, 7);
        assert_eq!(f.frame, 33);
        assert_eq!(f.baud, 3200);
        assert_eq!(f.page_type, PageType::Alphanumeric);
        assert!(f.text.starts_with("HELLO WORLD"), "got {:?}", f.text);
    }

    /// SPEC-CITED DECODE (6400 bps, 4-FSK, Phases A/B/C/D): a numeric page in
    /// Phase A and an alpha page in Phase C — proving the 3200-sym/s phase
    /// alternation (A/B even symbols, C/D odd symbols) de-interleaves correctly.
    #[test]
    fn decode_symbols_recovers_spec_pages_6400_four_phase() {
        // Phase A: numeric "12345".
        let cap_a = 555_000u32;
        let num_words = build_numeric_frame(cap_a, 1, 2, &[1, 2, 3, 4, 5]);
        let fiw_word = num_words[0];
        let phase_a: Vec<u32> = num_words[1..].to_vec();
        let phase_b = vec![0u32; frame::WORDS_PER_PHASE];
        // Phase C: alpha "HI".
        let cap_c = 222_333u32;
        let alpha_words = build_alpha_frame(cap_c, 1, 2, "HI");
        let phase_c: Vec<u32> = alpha_words[1..].to_vec();
        let phase_d = vec![0u32; frame::WORDS_PER_PHASE];

        let mode = demod::FlexMode::from_baud(6400).unwrap();
        let syms = build_symbol_frame_spec(
            mode,
            demod::A_CODE_3200_4,
            fiw_word,
            &[phase_a, phase_b, phase_c, phase_d],
            64,
        );

        let frames = decode_symbols(&syms, 6400);
        let fa = frames
            .iter()
            .find(|f| f.capcode == cap_a)
            .unwrap_or_else(|| panic!("Phase A numeric page missing; got {frames:?}"));
        assert_eq!(fa.kind, FlexKind::Numeric);
        assert!(fa.text.starts_with("12345"), "got {:?}", fa.text);
        let fc = frames
            .iter()
            .find(|f| f.capcode == cap_c)
            .unwrap_or_else(|| panic!("Phase C alpha page missing; got {frames:?}"));
        assert_eq!(fc.kind, FlexKind::Alpha);
        assert!(fc.text.starts_with("HI"), "got {:?}", fc.text);
        assert_eq!(fc.baud, 6400);
    }

    /// Helper: build FIW + 88-word phase for a numeric page (spec layout).
    fn build_numeric_frame(capcode: u32, cycle: u32, frame_no: u32, digits: &[u32]) -> Vec<u32> {
        let body = (cycle << 4) | (frame_no << 8);
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
        // Numeric body at word 3: 4-bit groups LSB-first.
        let mut data = 0u32;
        for (k, &d) in digits.iter().enumerate() {
            data |= (d & 0xF) << (k * 4);
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
        words
    }

    /// The modulator's `interleave_phases` must be the EXACT inverse of
    /// `deinterleave_phases` for every 4-level mode (structural round trip; this
    /// is what licenses the modulator-driven BER test below to use it). Uses
    /// random-ish phase contents so it is not a trivial all-zero pass.
    #[test]
    fn interleave_roundtrips_deinterleave() {
        for baud in [3200u32, 6400] {
            let mode = demod::FlexMode::from_baud(baud).unwrap();
            let n = mode.num_phases();
            let mut phases = Vec::new();
            let mut lfsr = 0xBEEFu32;
            for p in 0..n {
                let mut ph = vec![0u32; frame::WORDS_PER_PHASE];
                for w in ph.iter_mut() {
                    let mut v = 0u32;
                    for _ in 0..32 {
                        let bit = (lfsr ^ (lfsr >> 2) ^ (lfsr >> 3) ^ (lfsr >> 5)) & 1;
                        lfsr = (lfsr >> 1) | (bit << 15);
                        v = (v << 1) | bit;
                    }
                    *w = v;
                }
                let _ = p;
                phases.push(ph);
            }
            let syms = modulate::interleave_phases(mode, &phases);
            assert_eq!(syms.len(), demod::data_symbols(mode.sym_rate));
            let back = demod::deinterleave_phases(&syms, mode, false);
            assert_eq!(back.len(), n, "baud {baud} phase count");
            for (p, (orig, got)) in phases.iter().zip(back.iter()).enumerate() {
                assert_eq!(orig, got, "baud {baud} phase {p} not recovered by round trip");
            }
        }
    }

    /// SYNTHETIC DEMOD VALIDATION at 3200 bps (4-FSK): modulate a real
    /// spec-built alpha frame to 4-level FSK IQ, add complex AWGN, demod through
    /// the full channel decoder, and require the page to be recovered intact.
    /// Reported as synthetic; no off-air 4-FSK FLEX IQ is available.
    #[test]
    fn demod_recovers_page_4fsk_3200_synth_iq() {
        let baud = 3200u32;
        let mode = demod::FlexMode::from_baud(baud).unwrap();
        let capcode = 1_234_567u32;
        let words = build_alpha_frame(capcode, 7, 33, "PAGE ME");
        let fiw_word = words[0];
        let phase_a: Vec<u32> = words[1..].to_vec();
        let phase_b = vec![0u32; frame::WORDS_PER_PHASE];
        let syms = modulate::frame_symbols(
            600,
            mode,
            demod::A_CODE_1600_4,
            fiw_word,
            mode.sym_rate as usize * 25 / 1000,
            &[phase_a, phase_b],
        );
        let iq = modulate::modulate_symbols_iq(&syms, CHANNEL_RATE, mode.sym_rate as f64, 1200.0, 1.0);
        // 4-level FSK needs ~6 dB more SNR than 2-level for the same BER (the
        // inner tones halve the per-decision Euclidean distance); 28 dB leaves
        // BCH(31,21,2) ample margin on this page.
        let snr_db = 28.0;
        let noisy = modulate::add_awgn(&iq, snr_db, 0xC0FFEE);

        let mut dec = FlexChannelDecoder::new(CHANNEL_RATE, 1200.0, baud).unwrap();
        let frames = dec.process(&noisy);
        assert!(
            frames.iter().any(|f| f.capcode == capcode
                && f.kind == FlexKind::Alpha
                && f.baud == 3200
                && f.text.starts_with("PAGE ME")),
            "synthetic 4-FSK AWGN demod @ {snr_db} dB SNR failed to recover the 3200 bps page; got {frames:?}"
        );
    }

    /// SYNTHETIC raw symbol-error / BER measurement at 6400 bps (4-FSK, 3200
    /// sym/s): modulate a known pseudo-random symbol payload, add AWGN, run the
    /// symbol demod, align on Sync 1, and measure the raw per-dibit BER over the
    /// DATA region. Asserts it is low enough at a moderate SNR that BCH(31,21,2)
    /// can clean the residual. Purely synthetic (no real-RF 4-FSK IQ exists).
    #[test]
    fn demod_raw_ber_4fsk_6400_synth_iq() {
        let baud = 6400u32;
        let mode = demod::FlexMode::from_baud(baud).unwrap();
        // Build four phases of pseudo-random valid FLEX words.
        let mut phases = Vec::new();
        let mut lfsr = 0x1357u32;
        for _ in 0..mode.num_phases() {
            let mut ph = vec![0u32; frame::WORDS_PER_PHASE];
            for w in ph.iter_mut() {
                let mut data = 0u32;
                for _ in 0..21 {
                    let bit = (lfsr ^ (lfsr >> 2) ^ (lfsr >> 3) ^ (lfsr >> 5)) & 1;
                    lfsr = (lfsr >> 1) | (bit << 15);
                    data = (data << 1) | bit;
                }
                *w = bch::encode(data & 0x1F_FFFF);
            }
            phases.push(ph);
        }
        let fiw_word = bch::encode(0xF); // benign FIW (checksum-neutral data)
        let tx_syms = modulate::frame_symbols(
            600,
            mode,
            demod::A_CODE_3200_4,
            fiw_word,
            mode.sym_rate as usize * 25 / 1000,
            &phases,
        );
        let iq =
            modulate::modulate_symbols_iq(&tx_syms, CHANNEL_RATE, mode.sym_rate as f64, 600.0, 1.0);
        // 4-level 6400-bps random data exercises all four tones; 25 dB SNR keeps
        // the raw dibit BER well under the BCH-correctable 5% (≈0.9% measured).
        let snr_db = 25.0;
        let noisy = modulate::add_awgn(&iq, snr_db, 0x5EED_1234);

        let mut d = demod::SymbolDemod::new(mode.sym_rate as f64);
        let mut rx_syms = Vec::new();
        d.process(&noisy, &mut rx_syms);

        // Align on Sync 1.
        let sync_bits: Vec<u8> = rx_syms.iter().map(|&s| demod::symbol_sync_bit(s)).collect();
        let (off, inverted, rmode) =
            demod::find_sync_mode(&sync_bits, SYNC_MAX_ERR).expect("4-FSK sync must lock in BER test");
        assert_eq!(rmode.baud(), 6400, "resolved wrong mode from A-code");

        // De-interleave both tx and rx DATA and compare dibits.
        let data_start = off + 64 + 48 + mode.sym_rate as usize * 25 / 1000;
        let n_data = demod::data_symbols(mode.sym_rate);
        assert!(data_start + n_data <= rx_syms.len(), "rx too short");
        let rx_phases =
            demod::deinterleave_phases(&rx_syms[data_start..data_start + n_data], rmode, inverted);

        let mut errors = 0usize;
        let mut total = 0usize;
        for (tx_ph, rx_ph) in phases.iter().zip(rx_phases.iter()) {
            for (&t, &r) in tx_ph.iter().zip(rx_ph.iter()) {
                errors += (t ^ r).count_ones() as usize;
                total += 32;
            }
        }
        let ber = errors as f64 / total.max(1) as f64;
        assert!(total > 5000, "too few bits compared ({total})");
        assert!(
            ber < 0.05,
            "6400 bps 4-FSK @ {snr_db} dB: raw BER {ber:.4} too high ({errors}/{total})"
        );
    }
}
