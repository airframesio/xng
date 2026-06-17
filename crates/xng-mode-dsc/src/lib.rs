//! Native Digital Selective Calling (DSC) decode core for xng.
//!
//! DSC (ITU-R M.493 / M.541, built on the CCIR 493 alphabet) is the calling
//! and distress-alerting layer of the GMDSS, carried by FSK on MF/HF
//! (170 Hz shift, 100 Bd) and VHF (1300/2100 Hz, 1200 Bd) channels.
//!
//! This commit lands the **symbol level** ([`symbol`]): the FSK bit stream is
//! sliced into 10-bit CCIR 493 symbols (7 information bits + a 3-bit count of
//! the zero information bits, giving each symbol its own integrity check), and
//! the DX/RX time-diversity streams are de-interleaved into one symbol
//! sequence, recovering symbols erased in one stream from the other.
//!
//! The message/frame layer and IQ front end land in subsequent commits. See
//! PROVENANCE.md for the external reference vectors this layer is pinned to.

pub mod symbol;

pub use symbol::{decode_bitstream, decode_symbol, deinterleave_dx_rx, ERASURE};
