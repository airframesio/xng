//! sherpa-onnx speech engines (`sherpa` build feature): Moonshine,
//! NVIDIA Parakeet and SenseVoice through ONNX Runtime. They need far less
//! CPU than Whisper for the same audio because they do not pad every
//! transmission to a 30 s window: a 4 s call costs 4 s of compute, not 30.
//!
//! Models are named from [`MODELS`] and fetched once from the sherpa-onnx
//! releases on GitHub, or given as a directory holding an exported model.

/// A model xng knows by name.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    /// Name used in `--transcribe-model` and reported to Airframes ATC.
    pub name: &'static str,
    /// Archive in the sherpa-onnx `asr-models` release (without .tar.bz2).
    pub archive: &'static str,
    /// Download size, MB (for the log line).
    pub mb: u32,
    pub note: &'static str,
}

/// Default sherpa-onnx model: the best English accuracy per CPU second in
/// our ATC tests (docs/notes/AIRBAND.md).
pub const DEFAULT: &str = "parakeet-110m";

pub const MODELS: &[Known] = &[
    Known {
        name: "moonshine-tiny",
        archive: "sherpa-onnx-moonshine-tiny-en-int8",
        mb: 108,
        note: "English, lowest CPU (Raspberry Pi 3/4)",
    },
    Known {
        name: "moonshine-base",
        archive: "sherpa-onnx-moonshine-base-en-int8",
        mb: 250,
        note: "English, low CPU",
    },
    Known {
        name: "moonshine-tiny-v2",
        archive: "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27",
        mb: 40,
        note: "English, Moonshine v2 (smaller download)",
    },
    Known {
        name: "moonshine-base-v2",
        archive: "sherpa-onnx-moonshine-base-en-quantized-2026-02-27",
        mb: 125,
        note: "English, Moonshine v2 (smaller download)",
    },
    Known {
        name: "parakeet-110m",
        archive: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
        mb: 108,
        note: "English, best accuracy per CPU second (default)",
    },
    Known {
        name: "parakeet-tdt-0.6b-v2",
        archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        mb: 600,
        note: "English, highest accuracy, about 4x the CPU of parakeet-110m",
    },
    Known {
        name: "parakeet-tdt-0.6b-v3",
        archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        mb: 640,
        note: "25 European languages",
    },
    Known {
        name: "sense-voice",
        archive: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17",
        mb: 160,
        note: "Chinese, English, Japanese, Korean, Cantonese",
    },
];

pub fn find(name: &str) -> Option<&'static Known> {
    MODELS.iter().find(|m| m.name == name)
}

#[cfg(feature = "sherpa")]
pub use engine::SherpaEngine;

#[cfg(feature = "sherpa")]
pub(super) mod engine {
    use super::super::config::TranscribeSettings;
    use super::super::transcribe::{model_dir, Engine, EngineInfo, Segment};
    use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};
    use std::path::{Path, PathBuf};

    pub struct SherpaEngine {
        rec: OfflineRecognizer,
        info: EngineInfo,
    }

    // OfflineRecognizer is Send + Sync in sherpa-onnx; the engine lives on
    // the worker thread only.

    impl SherpaEngine {
        pub fn new(s: &TranscribeSettings) -> anyhow::Result<Self> {
            let want = s.model.clone().unwrap_or_else(|| super::DEFAULT.into());
            let dir = model_files(&want)?;
            let mut cfg = OfflineRecognizerConfig::default();
            configure(&mut cfg, &dir, s)?;
            cfg.model_config.num_threads = threads(s);
            cfg.model_config.provider = Some("cpu".into());
            let rec = OfflineRecognizer::create(&cfg)
                .ok_or_else(|| anyhow::anyhow!("sherpa-onnx could not load the model in {}", dir.display()))?;
            Ok(Self { rec, info: super::super::transcribe::planned(s) })
        }
    }

    /// Moonshine and Parakeet run best on few threads: the clips are short
    /// and the demodulator needs the rest.
    fn threads(s: &TranscribeSettings) -> i32 {
        if s.threads > 0 {
            return s.threads as i32;
        }
        std::thread::available_parallelism().map(|n| (n.get() / 2).clamp(1, 4)).unwrap_or(1) as i32
    }

    /// Fill in the recognizer for whatever model family `dir` holds.
    pub(super) fn configure(cfg: &mut OfflineRecognizerConfig, dir: &Path, s: &TranscribeSettings) -> anyhow::Result<()> {
        let f = |names: &[&str]| -> Option<String> {
            names.iter().map(|n| dir.join(n)).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
        };
        let any = |pred: &dyn Fn(&str) -> bool| -> Option<String> {
            let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
                .ok()?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(pred))
                .collect();
            // Prefer the int8 export when both are there.
            v.sort_by_key(|p| !p.to_string_lossy().contains("int8"));
            v.first().map(|p| p.to_string_lossy().into_owned())
        };
        let m = &mut cfg.model_config;
        m.tokens = f(&["tokens.txt"]).or_else(|| any(&|n| n.ends_with("tokens.txt")));
        anyhow::ensure!(m.tokens.is_some(), "no tokens.txt in {}", dir.display());
        if let (Some(enc), Some(dec)) = (f(&["encoder_model.ort"]), f(&["decoder_model_merged.ort"])) {
            // Moonshine v2.
            m.moonshine.encoder = Some(enc);
            m.moonshine.merged_decoder = Some(dec);
        } else if let Some(pre) = f(&["preprocess.onnx"]) {
            // Moonshine v1.
            m.moonshine.preprocessor = Some(pre);
            m.moonshine.encoder = any(&|n| n.starts_with("encode."));
            m.moonshine.uncached_decoder = any(&|n| n.starts_with("uncached_decode."));
            m.moonshine.cached_decoder = any(&|n| n.starts_with("cached_decode."));
        } else if let Some(joiner) = any(&|n| n.starts_with("joiner") && n.ends_with(".onnx")) {
            // Parakeet (NeMo) and other transducers.
            m.transducer.encoder = any(&|n| n.starts_with("encoder") && n.ends_with(".onnx"));
            m.transducer.decoder = any(&|n| n.starts_with("decoder") && n.ends_with(".onnx"));
            m.transducer.joiner = Some(joiner);
            m.model_type = Some("nemo_transducer".into());
            // Parakeet emits mixed case and punctuation.
        } else if let Some(model) = any(&|n| n.starts_with("model") && n.ends_with(".onnx")) {
            // SenseVoice.
            m.sense_voice.model = Some(model);
            m.sense_voice.language = Some(if s.language == "auto" { "auto".into() } else { s.language.clone() });
            m.sense_voice.use_itn = true;
        } else {
            anyhow::bail!(
                "{} holds no model sherpa-onnx support in xng recognises (Moonshine, Parakeet/transducer, SenseVoice)",
                dir.display()
            );
        }
        Ok(())
    }

    /// The directory for `name`: an existing directory as is, a known name
    /// fetched once into the model cache.
    fn model_files(name: &str) -> anyhow::Result<PathBuf> {
        let p = Path::new(name);
        if p.is_dir() {
            return Ok(p.to_path_buf());
        }
        let k = super::find(name).ok_or_else(|| {
            anyhow::anyhow!(
                "sherpa-onnx model {name:?} is neither a directory nor a known name ({})",
                super::MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
            )
        })?;
        let root = model_dir().join("sherpa");
        let dir = root.join(k.archive);
        if dir.join("tokens.txt").is_file() {
            return Ok(dir);
        }
        std::fs::create_dir_all(&root)?;
        // XNG_SHERPA_MODELS_URL points at a mirror of the release assets.
        let base = std::env::var("XNG_SHERPA_MODELS_URL")
            .unwrap_or_else(|_| "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models".into());
        let url = format!("{}/{}.tar.bz2", base.trim_end_matches('/'), k.archive);
        tracing::info!("downloading speech model {} (~{} MB) to {} (once)", k.name, k.mb, dir.display());
        let resp = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(20))
            .build()
            .get(&url)
            .call()
            .map_err(|e| anyhow::anyhow!("downloading {url}: {e}"))?;
        // Unpack next to the target, then move into place, so a cut
        // download never looks like a model.
        let tmp = root.join(format!(".{}.part{}", k.archive, std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let r = (|| -> anyhow::Result<()> {
            let mut ar = tar::Archive::new(bzip2::read::BzDecoder::new(resp.into_reader()));
            // unpack() refuses entries that would land outside `tmp`.
            ar.unpack(&tmp)?;
            let inner = tmp.join(k.archive);
            anyhow::ensure!(inner.join("tokens.txt").is_file(), "archive has no {}/tokens.txt", k.archive);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::rename(&inner, &dir)?;
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        r.map_err(|e| e.context(format!("downloading {url}")))?;
        tracing::info!("speech model {} ready", k.name);
        Ok(dir)
    }

    impl Engine for SherpaEngine {
        fn info(&self) -> EngineInfo {
            self.info.clone()
        }

        fn transcribe(&mut self, samples: &[f32], _prompt: &str) -> anyhow::Result<Vec<Segment>> {
            // These models take no text prompt. Give them a little silence
            // either side; squelch-gated clips start and end abruptly.
            let mut audio = Vec::with_capacity(samples.len() + 4_800);
            audio.extend(std::iter::repeat_n(0.0, 1_600));
            audio.extend_from_slice(samples);
            audio.extend(std::iter::repeat_n(0.0, 3_200));
            let stream = self.rec.create_stream();
            stream.accept_waveform(super::super::hub::AUDIO_RATE as i32, &audio);
            self.rec.decode(&stream);
            let text = stream.get_result().map(|r| r.text).unwrap_or_default();
            Ok(vec![Segment { text, no_speech: None, prob: None }])
        }
    }
}

#[cfg(all(test, feature = "sherpa"))]
mod tests {
    use super::super::config::TranscribeSettings;
    use sherpa_onnx::OfflineRecognizerConfig;

    fn dir_with(name: &str, files: &[&str]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("xng-sherpa-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for f in files {
            std::fs::write(d.join(f), b"").unwrap();
        }
        d
    }

    /// Each release layout maps to the right sherpa-onnx model family.
    #[test]
    fn model_families() {
        let s = TranscribeSettings::default();
        let d = dir_with("pk", &["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"]);
        let mut c = OfflineRecognizerConfig::default();
        super::engine::configure(&mut c, &d, &s).unwrap();
        assert!(c.model_config.transducer.joiner.as_deref().unwrap().ends_with("joiner.int8.onnx"));
        assert_eq!(c.model_config.model_type.as_deref(), Some("nemo_transducer"));

        let d = dir_with(
            "m1",
            &["preprocess.onnx", "encode.int8.onnx", "cached_decode.int8.onnx", "uncached_decode.int8.onnx", "tokens.txt"],
        );
        let mut c = OfflineRecognizerConfig::default();
        super::engine::configure(&mut c, &d, &s).unwrap();
        assert!(c.model_config.moonshine.cached_decoder.as_deref().unwrap().ends_with("/cached_decode.int8.onnx"));
        assert!(c.model_config.moonshine.uncached_decoder.as_deref().unwrap().ends_with("/uncached_decode.int8.onnx"));

        let d = dir_with("m2", &["encoder_model.ort", "decoder_model_merged.ort", "tokens.txt"]);
        let mut c = OfflineRecognizerConfig::default();
        super::engine::configure(&mut c, &d, &s).unwrap();
        assert!(c.model_config.moonshine.merged_decoder.is_some());

        let d = dir_with("sv", &["model.int8.onnx", "tokens.txt"]);
        let mut c = OfflineRecognizerConfig::default();
        super::engine::configure(&mut c, &d, &s).unwrap();
        assert_eq!(c.model_config.sense_voice.language.as_deref(), Some("en"));

        let d = dir_with("none", &["tokens.txt"]);
        assert!(super::engine::configure(&mut OfflineRecognizerConfig::default(), &d, &s).is_err());
    }
}
