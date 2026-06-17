//! Native Digital Selective Calling (DSC) decode core for xng.
//!
//! DSC (ITU-R M.493 / M.541, built on the CCIR 493 alphabet) is the calling
//! and distress-alerting layer of the GMDSS, carried by FSK on MF/HF
//! (170 Hz shift, 100 Bd) and VHF (1300/2100 Hz, 1200 Bd) channels.
//!
//! Pipeline:
//!
//! 1. **Symbol level** ([`symbol`]) — the FSK bit stream is sliced into 10-bit
//!    CCIR 493 symbols (7 information bits + a 3-bit count of the zero
//!    information bits, giving each symbol its own integrity check), and the
//!    DX/RX time-diversity streams are de-interleaved into one symbol
//!    sequence, recovering symbols erased in one stream from the other.
//! 2. **Message level** ([`message`]) — the symbol sequence is parsed by
//!    format specifier into a structured [`message::DscMessage`]: addressed
//!    and self-identification MMSIs, category, telecommands, distress
//!    nature/position/time, frequency or working channel, end-of-sequence,
//!    and the recomputed error-check character (ECC) status. The message
//!    serializes to JSON via [`message::DscMessage::to_json`].
//!
//! The bit→symbol→message layers are pinned to an external reference decoder's
//! published vectors (see PROVENANCE.md). The IQ→bits front end (FSK demod and
//! bit/symbol synchronisation) is a documented TODO in [`demod`].

pub mod demod;
pub mod message;
pub mod symbol;

pub use message::{
    decode, Category, DscMessage, EndOfSequence, FirstCommand, Format, NatureOfDistress,
    SecondCommand,
};
pub use symbol::{decode_bitstream, decode_symbol, deinterleave_dx_rx, ERASURE};

/// Decodes a full bit stream (10 bits/symbol) into a [`DscMessage`], applying
/// DX/RX time-diversity de-interleaving with the standard geometry (6 leading
/// DX phasing characters; RX repeat trailing by 2). This is the convenience
/// path once a demod has produced a synchronised bit stream.
pub fn decode_from_bits(bits: &[u8]) -> DscMessage {
    let chars = symbol::decode_bitstream(bits);
    let symbols = symbol::deinterleave_dx_rx(&chars, 6, 2);
    message::decode(&symbols)
}
