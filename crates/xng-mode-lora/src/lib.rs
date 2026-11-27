//! Native LoRa receiver with Meshtastic and MeshCore packet decoding.
//!
//! ```text
//! capture IQ → [Ddc] (mix + decimate to 2 × BW)
//!            → [demod::Demodulator] per spreading factor (preamble, sync
//!               word, carrier/timing estimate, FFT dechirp)
//!            → [coding] (Gray, deinterleave, Hamming, dewhiten, CRC)
//!            → LoRa payload → [meshtastic] / [meshcore] by sync word
//!            → xng_types::Message
//! ```
//!
//! LoRa itself is a Semtech physical layer; the coding steps follow the
//! published reverse engineering (see PROVENANCE.md). Meshtastic uses sync
//! word 0x2B, MeshCore the "private" 0x12; that picks the packet parser.

pub mod coding;
pub mod demod;
pub mod meshcore;
pub mod meshtastic;
mod meshtastic_tables;
pub mod modulate;
pub mod pb;

use chrono::Utc;
use num_complex::Complex;
use serde_json::json;
use std::sync::OnceLock;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Widest supported LoRa channel (500 kHz) at two samples per chip.
pub const CHANNEL_RATE: f64 = 1_000_000.0;
/// Widest one-sided passband (500 kHz BW).
pub const CHANNEL_PASSBAND_HZ: f64 = 250_000.0;

/// Radio settings for one LoRa channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    /// Spreading factors to listen for at once (each costs one FFT per symbol).
    pub sfs: Vec<u8>,
    pub bw_hz: f64,
}

/// Session-wide LoRa settings, set once from the command line.
#[derive(Debug, Clone, Default)]
pub struct LoraConfig {
    /// Preset name (Meshtastic `LongFast` ..., or `meshcore-us`, `meshcore-eu`,
    /// `meshcore-anz`), applied to every channel. `None`: by mode/frequency.
    pub preset: Option<String>,
    /// Spreading factor override(s).
    pub sfs: Vec<u8>,
    /// Bandwidth override, Hz.
    pub bw_hz: Option<f64>,
    /// Extra Meshtastic channels: (name, PSK bytes).
    pub meshtastic_channels: Vec<(String, Vec<u8>)>,
    /// Extra MeshCore channels.
    pub meshcore_channels: Vec<meshcore::Channel>,
}

static CONFIG: OnceLock<LoraConfig> = OnceLock::new();

/// Install the session config (first call wins).
pub fn set_config(c: LoraConfig) {
    let _ = CONFIG.set(c);
}

pub fn config() -> LoraConfig {
    CONFIG.get().cloned().unwrap_or_default()
}

/// Parse a `--meshtastic-key NAME=BASE64` value.
pub fn parse_meshtastic_key(s: &str) -> Result<(String, Vec<u8>), String> {
    use base64::Engine;
    let (name, key) = s.split_once('=').ok_or("expected NAME=BASE64KEY")?;
    let k = base64::engine::general_purpose::STANDARD
        .decode(key.trim())
        .map_err(|e| format!("bad base64 key: {e}"))?;
    meshtastic::Channel::new(name, &k).ok_or("key must be 1, 16 or 32 bytes")?;
    Ok((name.to_string(), k))
}

/// Parse a `--meshcore-channel` value: `#name` (hashtag) or `NAME=BASE64`.
pub fn parse_meshcore_channel(s: &str) -> Result<meshcore::Channel, String> {
    if let Some((name, key)) = s.split_once('=') {
        meshcore::Channel::from_base64(name, key)
            .ok_or_else(|| "key must be 16 or 32 bytes of base64".to_string())
    } else if s.starts_with('#') {
        Ok(meshcore::Channel::hashtag(s))
    } else {
        Err("expected #name or NAME=BASE64KEY".into())
    }
}

/// Look up a named preset.
pub fn named_preset(name: &str) -> Option<Preset> {
    let lower = name.to_ascii_lowercase();
    if let Some((n, sf, bw, _)) = meshtastic::PRESETS
        .iter()
        .find(|(n, ..)| n.to_ascii_lowercase() == lower)
    {
        return Some(Preset {
            name: n.to_string(),
            sfs: vec![*sf],
            bw_hz: *bw,
        });
    }
    let mc = |n: &str, sfs: &[u8]| {
        Some(Preset {
            name: n.into(),
            sfs: sfs.to_vec(),
            bw_hz: 62_500.0,
        })
    };
    match lower.as_str() {
        "meshcore-us" | "meshcore-usa" => mc("meshcore-us", &[7]),
        "meshcore-eu" | "meshcore-uk" => mc("meshcore-eu", &[7, 8]),
        "meshcore-anz" | "meshcore-au" | "meshcore-nz" => mc("meshcore-anz", &[7]),
        _ => None,
    }
}

/// Preset for a channel: the configured one, else the mode's default
/// (Meshtastic LongFast; MeshCore's narrow 62.5 kHz settings, listening on
/// SF7 and SF8 in the EU band where both are in use).
pub fn preset_for(mode: Mode, carrier_hz: f64, cfg: &LoraConfig) -> Preset {
    let mut p = cfg
        .preset
        .as_deref()
        .and_then(named_preset)
        .unwrap_or_else(|| match mode {
            Mode::MeshCore => {
                if (863e6..=870e6).contains(&carrier_hz) {
                    named_preset("meshcore-eu").unwrap()
                } else {
                    named_preset("meshcore-us").unwrap()
                }
            }
            _ => named_preset("LongFast").unwrap(),
        });
    if !cfg.sfs.is_empty() {
        p.sfs = cfg.sfs.clone();
    }
    if let Some(bw) = cfg.bw_hz {
        p.bw_hz = bw;
    }
    p
}

/// A frame with its decoded packet.
#[derive(Debug, Clone)]
pub struct MeshFrame {
    pub lora: demod::LoraFrame,
    pub mode: Mode,
    pub kind: String,
    pub details: serde_json::Value,
}

/// Decodes one LoRa channel out of a wideband capture.
pub struct LoraChannelDecoder {
    ddc: Option<Ddc>,
    demods: Vec<demod::Demodulator>,
    buf: Vec<Complex<f32>>,
    mode: Mode,
    preset: Preset,
    mt_channels: Vec<meshtastic::Channel>,
    mc_channels: Vec<meshcore::Channel>,
    /// Frames whose payload CRC failed (counted, not emitted).
    pub crc_failures: u64,
}

impl LoraChannelDecoder {
    pub fn new(
        mode: Mode,
        input_rate: f64,
        freq_offset_hz: f64,
        carrier_hz: f64,
    ) -> Result<Self, String> {
        let cfg = config();
        let preset = preset_for(mode, carrier_hz, &cfg);
        Self::with_preset(mode, input_rate, freq_offset_hz, carrier_hz, preset, &cfg)
    }

    pub fn with_preset(
        mode: Mode,
        input_rate: f64,
        freq_offset_hz: f64,
        carrier_hz: f64,
        preset: Preset,
        cfg: &LoraConfig,
    ) -> Result<Self, String> {
        let rate = preset.bw_hz * demod::OVERSAMPLE as f64;
        let ddc = if (input_rate - rate).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            // Pass the chirp band plus a small margin for carrier offset.
            Some(Ddc::new(
                input_rate,
                rate,
                freq_offset_hz,
                preset.bw_hz * 0.55,
            )?)
        };
        let demods = preset
            .sfs
            .iter()
            .map(|&sf| {
                if !(7..=12).contains(&sf) {
                    return Err(format!("spreading factor {sf} not supported (7..12)"));
                }
                Ok(demod::Demodulator::new(demod::RxParams {
                    sf,
                    bw_hz: preset.bw_hz,
                    carrier_hz,
                    ldro: None,
                }))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut mt_channels = meshtastic::default_channels();
        mt_channels.extend(
            cfg.meshtastic_channels
                .iter()
                .filter_map(|(n, k)| meshtastic::Channel::new(n, k)),
        );
        let mut mc_channels = vec![meshcore::Channel::public()];
        mc_channels.extend(cfg.meshcore_channels.iter().cloned());
        Ok(Self {
            ddc,
            demods,
            buf: Vec::new(),
            mode,
            preset,
            mt_channels,
            mc_channels,
            crc_failures: 0,
        })
    }

    pub fn preset(&self) -> &Preset {
        &self.preset
    }

    pub fn channel_rate(&self) -> f64 {
        self.preset.bw_hz * demod::OVERSAMPLE as f64
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demods
            .first()
            .map(|d| d.level_dbfs())
            .unwrap_or(-100.0)
    }

    /// Decode a LoRa payload by its sync word (falling back to the session mode).
    pub fn decode_payload(&self, f: demod::LoraFrame) -> Option<MeshFrame> {
        let mode = match f.sync_word {
            meshtastic::SYNC_WORD => Mode::Meshtastic,
            meshcore::SYNC_WORD => Mode::MeshCore,
            _ => self.mode,
        };
        let (kind, mut details) = match mode {
            Mode::MeshCore => {
                let d = meshcore::decode(&f.payload, &self.mc_channels)?;
                (d.kind, d.details)
            }
            _ => {
                let d = meshtastic::decode(&f.payload, &self.mt_channels)?;
                (d.kind, d.details)
            }
        };
        if let Some(o) = details.as_object_mut() {
            o.insert(
                "lora".into(),
                json!({
                    "sf": f.sf,
                    "bw_khz": f.bw_hz / 1e3,
                    "cr": format!("4/{}", 4 + f.cr),
                    "sync_word": format!("0x{:02x}", f.sync_word),
                    "preset": self.preset.name,
                    "snr_db": (f.snr_db as f64 * 10.0).round() / 10.0,
                    "cfo_hz": f.cfo_hz.round(),
                }),
            );
        }
        Some(MeshFrame {
            lora: f,
            mode,
            kind,
            details,
        })
    }

    /// Feed capture IQ; returns every frame whose CRC verifies.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<MeshFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.buf.clear();
                ddc.process(input, &mut self.buf);
                &self.buf
            }
            None => input,
        };
        let mut frames = Vec::new();
        for d in &mut self.demods {
            frames.extend(d.process(channel));
        }
        let mut out = Vec::new();
        for f in frames {
            if f.crc_ok == Some(false) {
                self.crc_failures += 1;
                continue;
            }
            if let Some(m) = self.decode_payload(f) {
                out.push(m);
            }
        }
        out
    }
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(
    f: &MeshFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let body = match f.mode {
        Mode::MeshCore => MessageBody::MeshCore {
            kind: f.kind.clone(),
            details: f.details.clone(),
        },
        _ => MessageBody::Meshtastic {
            kind: f.kind.clone(),
            details: f.details.clone(),
        },
    };
    Message {
        mode: f.mode,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            snr_db: Some(f.lora.snr_db),
            freq_skew_hz: Some(f.lora.cfo_hz),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: f.lora.crc_ok.unwrap_or(true),
            fec_corrected: Some(f.lora.corrected_bits),
            errors: None,
        },
        body,
        raw: Some(f.lora.payload.clone()),
        source,
    }
}
