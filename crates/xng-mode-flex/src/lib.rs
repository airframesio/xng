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
        // A-code GATE: the 16 bits before the marker are the per-rate A-code.
        // The 1600-bps 2-level path must only decode a Sync 1 whose A-code is a
        // **1600 sym/s 2-level** mode — otherwise a 4-level (3200/6400) burst,
        // whose Sync 1 is still 2-level @ 1600 and so matches the marker here,
        // would be mis-decoded as 1600 (the historical garbage bug). This makes
        // the auto-detect lanes self-gating on the A-code.
        if abs >= 16 && !a_code_is_1600_2level(bits, abs, inverted) {
            let advance = (sync_off + 32).max(32);
            if search_from + advance >= bits.len() {
                break;
            }
            search_from += advance;
            continue;
        }
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

/// Read the 16-bit Sync-1 A-code (the 16 bits ending right before the 32-bit
/// marker at `marker_pos`, MSB-first) and test whether it resolves to a 1600
/// sym/s **2-level** mode. `inverted` flips polarity to match the sync lock.
fn a_code_is_1600_2level(bits: &[u8], marker_pos: usize, inverted: bool) -> bool {
    let mut a = 0u16;
    for &b in &bits[marker_pos - 16..marker_pos] {
        let bit = if inverted { b ^ 1 } else { b };
        a = (a << 1) | (bit as u16 & 1);
    }
    matches!(
        demod::FlexMode::from_a_code(a, SYNC_MAX_ERR),
        Some(m) if m.sym_rate == 1600 && m.levels == 2
    )
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

/// Decode the off-air 4-level recovered phases produced by
/// [`demod::recover_4level_frames`] into [`FlexFrame`]s.
///
/// `phases` holds the de-interleaved A/B/C/D phase word buffers (raw 32-bit
/// codewords). Each phase is BCH-corrected then walked through the BIW → address
/// → vector → message structure (the off-air alpha body honors the FLEX
/// per-message header word + fragment-flag char that real transmissions carry,
/// which the synthetic spec frames omit — see [`decode_alpha_offair`]).
fn decode_recovered_frames(
    mode: demod::FlexMode,
    fiw: Option<(u32, u32)>,
    phases: &[Vec<u32>],
    baud: u32,
) -> Vec<FlexFrame> {
    let Some((fiw_word, fiw_fix)) = fiw else {
        return Vec::new();
    };
    let _ = mode;
    let mut out = Vec::new();
    for phase in phases {
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
        out.extend(decode_phase_words_offair(fiw_word, fiw_fix, &words, &fixes, baud));
    }
    out
}

/// Like [`decode_phase_words`] but using the real-FLEX alpha body layout (header
/// word + fragment-flag char skip). Used only by the off-air 4-level path.
fn decode_phase_words_offair(
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
    let biw = frame::parse_biw(words[0]);
    let aoff = biw.address_offset;
    let voff = biw.vector_offset;
    if aoff >= words.len() || voff == 0 || voff > words.len() {
        return out;
    }
    let addr_end = voff.min(words.len());
    let mut i = aoff;
    while i < addr_end {
        let aw1 = words[i];
        if aw1 == 0 {
            i += 1;
            continue;
        }
        let addr = frame::decode_short_address(aw1);
        let vidx = voff + (i - aoff);
        if vidx >= words.len() {
            break;
        }
        let viw = words[vidx];
        let page_type = PageType::from_viw(viw);
        let mut fec = fiw_fix + fixes[i] + fixes[vidx];
        let mut raw_words = vec![fiw_word, aw1, viw];
        let mw1 = ((viw >> 7) & 0x7F) as usize;
        let len = ((viw >> 14) & 0x7F) as usize;

        let (kind, text) = match page_type {
            PageType::Tone | PageType::Secure | PageType::ShortInstruction => {
                (FlexKind::Tone, String::new())
            }
            PageType::Alphanumeric | PageType::Binary => {
                let body = collect_message(words, fixes, mw1, len, &mut fec, &mut raw_words);
                (FlexKind::Alpha, decode_alpha_offair(&body))
            }
            PageType::StandardNumeric | PageType::SpecialNumeric | PageType::NumberedNumeric => {
                let body = collect_message(words, fixes, mw1, len, &mut fec, &mut raw_words);
                (FlexKind::Numeric, frame::decode_numeric(&body))
            }
        };
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

/// Decode a real-FLEX alphanumeric body. The FIRST word of the body
/// (`words[0]`) is a per-message **header** carrying fragment (bits 11..=12) and
/// continuation (bit 10) flags — NOT text. Text starts at `words[1]`; the very
/// first text character is a fragment-check byte that is dropped when the
/// fragment field == 0x03. (multimon-ng `parse_alphanumeric`.) Remaining 7-bit
/// chars decode as usual (0x03 ETX separators dropped, trailing control trimmed).
fn decode_alpha_offair(words: &[u32]) -> String {
    if words.len() < 2 {
        return String::new();
    }
    let frag = (words[0] >> 11) & 0x03;
    let mut out = String::new();
    for (wi, &w) in words[1..].iter().enumerate() {
        let data = w & 0x001F_FFFF;
        for c in 0..3 {
            if wi == 0 && c == 0 && frag == 0x03 {
                continue;
            }
            let ch = ((data >> (c * 7)) & 0x7F) as u8;
            if ch != 0x03 {
                out.push(ch as char);
            }
        }
    }
    while matches!(out.chars().last(), Some(c) if (c as u32) < 0x20) {
        out.pop();
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

/// One full demod+decode lane for a single FLEX information rate.
///
/// A lane owns its own demod (2-level [`demod::FskDemod`] for 1600 bps, or
/// 4-level [`demod::SymbolDemod`] for 3200 / 6400 bps), the recovered
/// bit/symbol stream, and its incremental scan cursor. `decode_new` re-scans
/// only the newly demodulated tail (plus one frame of overlap) and returns the
/// frames found there. Sync 1 + the FIW are always 1600 sym/s 2-level on air,
/// but for the 4-level lanes the lane's [`demod::SymbolDemod`] reads them as the
/// outer symbols {0,3}; the lane's `decode_symbols` only accepts a Sync 1 whose
/// **A-code resolves to this lane's rate**, so a 3200-sym/s burst never decodes
/// in the 1600-sym/s lane and vice-versa.
struct Lane {
    /// Resolved on-air mode (symbol rate + levels) for this lane's baud.
    mode: demod::FlexMode,
    /// 2-level NRZ bit demod (1600 bps path).
    bit_demod: Option<demod::FskDemod>,
    /// 4-level single-rate symbol demod (used by FIXED 3200/6400 constructors and
    /// the synthetic modulate→demod tests, where the whole frame is one rate).
    sym_demod: Option<demod::SymbolDemod>,
    baud: u32,
    /// Recovered bits (2-level) or symbols 0..=3 (single-rate 4-level).
    stream: Vec<u8>,
    scanned_to: usize,
    /// Off-air two-clock recovery for 4-level (auto path only): real FLEX sends
    /// Sync 1 + FIW at 1600 sym/s and DATA at the mode rate, a transition no
    /// single demod handles — so buffer channel IQ and recover per frame.
    offair_4level: bool,
    iq: Vec<Complex<f32>>,
    iq_scanned_to: usize,
    level: f32,
}

impl Lane {
    /// A fixed-rate lane: 2-level NRZ bits, or single-rate 4-level symbols.
    fn new(baud: u32) -> Self {
        Self::build(baud, false)
    }

    /// An auto-path lane: 4-level rates use the off-air two-clock recovery.
    fn new_auto(baud: u32) -> Self {
        let mode = demod::FlexMode::from_baud(baud).expect("validated baud");
        Self::build(baud, mode.levels == 4)
    }

    fn build(baud: u32, offair_4level: bool) -> Self {
        // `baud` is pre-validated by the caller (FlexMode::from_baud).
        let mode = demod::FlexMode::from_baud(baud).expect("validated baud");
        let (bit_demod, sym_demod) = if mode.levels == 2 {
            (Some(demod::FskDemod::new(mode.sym_rate as f64)), None)
        } else if offair_4level {
            (None, None)
        } else {
            (None, Some(demod::SymbolDemod::new(mode.sym_rate as f64)))
        };
        Self {
            mode,
            bit_demod,
            sym_demod,
            baud,
            stream: Vec::new(),
            scanned_to: 0,
            offair_4level,
            iq: Vec::new(),
            iq_scanned_to: 0,
            level: 0.0,
        }
    }

    /// Feed a channel-rate IQ chunk. 2-level / single-rate 4-level lanes demod to
    /// their bit/symbol stream; the off-air 4-level lane buffers the IQ.
    fn feed(&mut self, channel: &[Complex<f32>]) {
        if self.offair_4level {
            for &x in channel {
                self.level += 0.002 * (x.norm_sqr() - self.level);
            }
            self.iq.extend_from_slice(channel);
            return;
        }
        match (&mut self.bit_demod, &mut self.sym_demod) {
            (Some(d), _) => d.process(channel, &mut self.stream),
            (_, Some(d)) => d.process(channel, &mut self.stream),
            _ => unreachable!("lane has neither demod"),
        }
    }

    /// Decode newly available data (with one frame of overlap so a frame
    /// straddling a chunk boundary still completes).
    fn decode_new(&mut self) -> Vec<FlexFrame> {
        if self.offair_4level {
            // Off-air two-clock recovery over the new IQ tail + one frame overlap.
            let spb = CHANNEL_RATE / self.mode.sym_rate as f64;
            let frame_samples =
                ((64.0 + 48.0 + 80.0) + demod::data_symbols(self.mode.sym_rate) as f64) * spb;
            let start = self.iq_scanned_to.saturating_sub(frame_samples as usize);
            let mut out = Vec::new();
            for (mode, fiw, phases) in demod::recover_4level_frames(&self.iq[start..], self.baud) {
                out.extend(decode_recovered_frames(mode, fiw, &phases, self.baud));
            }
            self.iq_scanned_to = self.iq.len();
            return out;
        }
        if self.mode.levels == 2 {
            let overlap = 32 + 16 + (1 + frame::WORDS_PER_PHASE) * 32;
            let start = self.scanned_to.saturating_sub(overlap);
            let d = decode_bits(&self.stream[start..], self.baud);
            self.scanned_to = self.stream.len();
            d
        } else {
            let overlap = 64 + 48 + 80 + demod::data_symbols(self.mode.sym_rate);
            let start = self.scanned_to.saturating_sub(overlap);
            let d = decode_symbols(&self.stream[start..], self.baud);
            self.scanned_to = self.stream.len();
            d
        }
    }

    /// Smoothed channel power level in dBFS.
    fn level_dbfs(&self) -> f32 {
        if self.offair_4level {
            return 10.0 * self.level.max(1e-12).log10();
        }
        match (&self.bit_demod, &self.sym_demod) {
            (Some(d), _) => d.level_dbfs(),
            (_, Some(d)) => d.level_dbfs(),
            _ => f32::NEG_INFINITY,
        }
    }
}

/// Lane configuration for a [`FlexChannelDecoder`]: a single fixed-rate lane, or
/// the auto-detect race across all three FLEX rates.
enum LaneMode {
    /// One lane at the explicitly requested baud.
    Fixed(Lane),
    /// One lane per FLEX rate, racing off the same channel IQ until one of them
    /// decodes a frame (the Sync 1 A-code makes the race self-gating: a burst
    /// only ever decodes in the lane whose rate its A-code resolves to). Once a
    /// lane produces frames it is locked and the others are dropped.
    Auto {
        lanes: Vec<Lane>,
        locked: Option<usize>,
    },
}

/// Decodes one FLEX channel out of a wideband capture.
///
/// Mirrors the POCSAG [`xng_mode_pocsag::PocsagChannelDecoder`] contract: owns
/// an internal [`Ddc`] that mixes by `freq_offset_hz` and decimates the capture
/// to [`CHANNEL_RATE`], runs the FSK demod at the configured baud, and emits
/// [`FlexFrame`]s as frames are recovered.
///
/// Two modes:
/// - **Fixed baud** ([`FlexChannelDecoder::new`] with `baud` ∈ {1600, 3200,
///   6400}): one demod at the given rate.
/// - **Auto rate** ([`FlexChannelDecoder::new_auto`], or `new(.., 0)`): all
///   three rates are demodulated in parallel off the *same* channel IQ (the
///   shared [`CHANNEL_RATE`] is an integer-bit multiple of every rate, so one
///   DDC serves all). The FLEX Sync 1 A-code already encodes the on-air rate, so
///   each lane's decode is self-gating — a 4-level 6400 burst never decodes in
///   the 1600 lane. Whichever lane first decodes a real frame is locked for the
///   rest of the session, so a single decoder handles any FLEX rate on air
///   without being told it in advance.
pub struct FlexChannelDecoder {
    ddc: Option<Ddc>,
    mode: LaneMode,
    channel_buf: Vec<Complex<f32>>,
    seen: Vec<String>,
}

impl FlexChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`] (a non-integer
    /// multiple is resampled by the DDC). `freq_offset_hz` is the FLEX channel
    /// center relative to the capture center. `baud` is the information bit
    /// rate: **1600** (2-FSK), **3200** (4-FSK, 1600 sym/s, Phases A/B),
    /// **6400** (4-FSK, 3200 sym/s, Phases A/B/C/D), **or `0` to auto-detect the
    /// rate from the Sync 1 A-code** (equivalent to [`new_auto`](Self::new_auto)).
    pub fn new(input_rate: f64, freq_offset_hz: f64, baud: u32) -> Result<Self, String> {
        if baud == 0 {
            return Self::new_auto(input_rate, freq_offset_hz);
        }
        demod::FlexMode::from_baud(baud).ok_or_else(|| {
            format!(
                "unsupported FLEX baud {baud}; use 1600 (2-FSK), 3200 / 6400 (4-FSK), or 0 for auto"
            )
        })?;
        Ok(Self {
            ddc: Self::make_ddc(input_rate, freq_offset_hz)?,
            mode: LaneMode::Fixed(Lane::new(baud)),
            channel_buf: Vec::new(),
            seen: Vec::new(),
        })
    }

    /// Auto-detect the FLEX rate (1600 / 3200 / 6400 bps) from the on-air
    /// signal. Every rate is demodulated in parallel off the same channel IQ;
    /// the Sync 1 A-code (read at the always-1600 sym/s sync) selects the data
    /// rate, so each lane's `decode_symbols`/`decode_bits` only commits a burst
    /// whose A-code resolves to that lane's rate. The first lane to decode a
    /// frame is locked for the session. Use this when the channel's rate is
    /// unknown — real US paging is commonly 4-level (3200 / 6400 bps), not the
    /// 1600-bps base rate.
    pub fn new_auto(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let lanes = vec![
            Lane::new_auto(1600),
            Lane::new_auto(3200),
            Lane::new_auto(6400),
        ];
        Ok(Self {
            ddc: Self::make_ddc(input_rate, freq_offset_hz)?,
            mode: LaneMode::Auto { lanes, locked: None },
            channel_buf: Vec::new(),
            seen: Vec::new(),
        })
    }

    fn make_ddc(input_rate: f64, freq_offset_hz: f64) -> Result<Option<Ddc>, String> {
        if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            Ok(None)
        } else {
            Ok(Some(Ddc::new(
                input_rate,
                CHANNEL_RATE,
                freq_offset_hz,
                CHANNEL_PASSBAND_HZ,
            )?))
        }
    }

    /// The information bit rate currently in use: the fixed rate, or the locked
    /// auto-detected rate, or `None` if auto-detect has not yet committed.
    pub fn baud(&self) -> Option<u32> {
        match &self.mode {
            LaneMode::Fixed(lane) => Some(lane.baud),
            LaneMode::Auto { lanes, locked: Some(i) } => Some(lanes[*i].baud),
            LaneMode::Auto { .. } => None,
        }
    }

    /// Feed capture IQ; returns newly completed FLEX frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<FlexFrame> {
        // DDC once; every lane shares the same channel IQ (CHANNEL_RATE is an
        // integer-bit multiple of all three rates).
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };

        let decoded = match &mut self.mode {
            LaneMode::Fixed(lane) => {
                lane.feed(channel);
                lane.decode_new()
            }
            LaneMode::Auto { lanes, locked } => {
                if let Some(i) = locked {
                    let lane = &mut lanes[*i];
                    lane.feed(channel);
                    lane.decode_new()
                } else {
                    // Race: feed every lane, decode each, and lock the first lane
                    // that produces a frame. The A-code gate inside each lane's
                    // decode means only the true-rate lane can ever decode, so
                    // the lock is correct as soon as any frame appears.
                    let mut decoded = Vec::new();
                    let mut winner = None;
                    for (i, lane) in lanes.iter_mut().enumerate() {
                        lane.feed(channel);
                        let frames = lane.decode_new();
                        if !frames.is_empty() && winner.is_none() {
                            winner = Some(i);
                            decoded = frames;
                        }
                    }
                    if let Some(i) = winner {
                        *locked = Some(i);
                    }
                    decoded
                }
            }
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

    /// Smoothed channel power level in dBFS. During auto-detect (before lock)
    /// this reflects the first lane's estimate; once a rate is fixed or locked
    /// it is that lane's level.
    pub fn level_dbfs(&self) -> f32 {
        match &self.mode {
            LaneMode::Fixed(lane) => lane.level_dbfs(),
            LaneMode::Auto { lanes, locked: Some(i) } => lanes[*i].level_dbfs(),
            LaneMode::Auto { lanes, .. } => {
                lanes.first().map(|l| l.level_dbfs()).unwrap_or(f32::NEG_INFINITY)
            }
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

    /// `new(.., 0)` is the documented alias for `new_auto`: it routes to the
    /// auto path (no committed baud until a signal locks one).
    #[test]
    fn new_with_zero_baud_selects_auto() {
        let dec = FlexChannelDecoder::new(CHANNEL_RATE, 0.0, 0).unwrap();
        assert_eq!(dec.baud(), None, "baud 0 must select auto (no fixed rate yet)");
        let dec_auto = FlexChannelDecoder::new_auto(CHANNEL_RATE, 0.0).unwrap();
        assert_eq!(dec_auto.baud(), None);
        // Fixed constructors report their rate immediately.
        assert_eq!(
            FlexChannelDecoder::new(CHANNEL_RATE, 0.0, 6400).unwrap().baud(),
            Some(6400)
        );
        // Unsupported baud is rejected.
        assert!(FlexChannelDecoder::new(CHANNEL_RATE, 0.0, 2400).is_err());
    }

    /// A-code GATE: a Sync 1 whose A-code is a 4-level mode (here the 6400-bps
    /// `0xDEA0`) must NOT decode in the 1600-bps 2-level path — otherwise the
    /// 4-level data (whose Sync 1 is still 2-level @1600) would be mis-read as
    /// 1600. This is what makes the auto-detect lanes self-gating on the A-code.
    #[test]
    fn decode_bits_rejects_non_1600_a_code() {
        let words = build_alpha_frame(1_234_567, 7, 33, "HELLO WORLD");
        // Build a Sync 1 with the 6400-bps 4-level A-code (0xDEA0) before the
        // marker: preamble | A | marker | ~A | data.
        let a = demod::A_CODE_3200_4;
        let mut bits = Vec::new();
        for i in 0..64 {
            bits.push((i % 2 == 0) as u8);
        }
        for i in (0..16).rev() {
            bits.push(((a >> i) & 1) as u8); // A-code, MSB-first
        }
        for i in (0..32).rev() {
            bits.push(((frame::SYNC_MARKER_B >> i) & 1) as u8); // marker
        }
        for i in (0..16).rev() {
            bits.push((((!a) >> i) & 1) as u8); // ~A field
        }
        for &w in &words {
            modulate::push_word_lsb(&mut bits, w);
        }
        // The 1600 path must reject this (wrong A-code) → no frames.
        let frames = decode_bits(&bits, 1600);
        assert!(
            frames.is_empty(),
            "1600 path must reject a 4-level A-code Sync 1; got {frames:?}"
        );
        // But a genuine 1600 A-code (via frame_bits) decodes fine.
        let ok = modulate::frame_bits(64, &build_alpha_frame(1_234_567, 7, 33, "HELLO WORLD"));
        assert!(decode_bits(&ok, 1600).iter().any(|f| f.capcode == 1_234_567));
    }

    /// Auto-detect must NOT false-lock on pure noise (no valid Sync 1 / A-code
    /// anywhere) — baud stays unset and no frames are emitted.
    #[test]
    fn auto_does_not_lock_on_noise() {
        let mut dec = FlexChannelDecoder::new_auto(CHANNEL_RATE, 0.0).unwrap();
        // Deterministic pseudo-random complex noise.
        let mut lfsr = 0x1357_9BDFu32;
        let noise: Vec<Complex<f32>> = (0..200_000)
            .map(|_| {
                lfsr ^= lfsr << 13;
                lfsr ^= lfsr >> 17;
                lfsr ^= lfsr << 5;
                let re = (lfsr & 0xFFFF) as f32 / 32768.0 - 1.0;
                lfsr ^= lfsr << 7;
                let im = (lfsr & 0xFFFF) as f32 / 32768.0 - 1.0;
                Complex::new(re, im)
            })
            .collect();
        let frames = dec.process(&noise);
        assert!(frames.is_empty(), "auto false-locked on noise: {frames:?}");
        assert_eq!(dec.baud(), None, "auto committed a baud on pure noise");
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
        // Data region begins after the 32-bit marker in both tx and rx. In tx
        // the marker follows preamble(600) + the 16-bit A-code, so it ends at
        // 600 + 16 + 32.
        let data_start_rx = sync_off + 32;
        let data_start_tx = 600 + 16 + 32;

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
