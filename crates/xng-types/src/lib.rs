//! Core types shared across xng: the normalized message model that every
//! decode core produces and every output consumes.
//!
//! This model is the in-process representation of what will become the
//! `asf-2.0` wire format (see `docs/ARCHITECTURE.md` §4.3). Raw payloads are
//! always preserved alongside decoded fields so downstream consumers can
//! re-decode with newer logic.

pub mod message;
pub mod mode;
pub mod source;

pub use message::{AcarsCore, DecodeQuality, Message, MessageBody, SignalQuality};
pub use mode::Mode;
pub use source::{AppInfo, ChannelInfo, Provenance, SdrInfo, StationIdentity};
