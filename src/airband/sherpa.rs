//! sherpa-onnx speech engines (`sherpa` build feature): Moonshine,
//! NVIDIA Parakeet and SenseVoice through ONNX Runtime. They need far less
//! CPU than Whisper for the same audio because they do not pad every
//! transmission to a 30 s window: a 4 s call costs 4 s of compute, not 30.
//!
//! Models are named from [`MODELS`] and fetched once (from the sherpa-onnx
//! releases on GitHub, or a pinned Hugging Face commit), or given as a
//! directory holding an exported model.

/// A model xng knows by name.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    /// Name used in `--transcribe-model` and reported to Airframes ATC.
    pub name: &'static str,
    /// Directory name in the model cache. For release models it is also
    /// the archive name in the sherpa-onnx `asr-models` release (without
    /// .tar.bz2).
    pub archive: &'static str,
    /// Where the files come from.
    pub from: Source,
    /// Download size, MB (for the log line).
    pub mb: u32,
    pub note: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// `<archive>.tar.bz2` in the sherpa-onnx `asr-models` GitHub release.
    Release,
    /// Single files in a Hugging Face model repo, pinned to a commit and
    /// checked against their LFS sha256 (None for small files kept in git,
    /// which are checked by size only).
    HuggingFace {
        repo: &'static str,
        rev: &'static str,
        files: &'static [HfFile],
    },
}

#[derive(Clone, Copy, Debug)]
pub struct HfFile {
    pub name: &'static str,
    pub size: u64,
    pub sha256: Option<&'static str>,
}

/// Default sherpa-onnx model: the ATC-tuned Parakeet, by far the most
/// accurate on real ATC audio per CPU second (docs/notes/AIRBAND.md).
pub const DEFAULT: &str = "skycontrol-atc-110m";

pub const MODELS: &[Known] = &[
    Known {
        name: "skycontrol-atc-110m",
        archive: "skycontrol-atc-asr-110m-afb8cb1",
        from: Source::HuggingFace {
            repo: "srinath-exe/skycontrol-atc-asr-110m",
            rev: "afb8cb1b9285df0936228d59b38251ad4391b5f6",
            files: &[
                HfFile {
                    name: "encoder.int8.onnx",
                    size: 131_112_998,
                    sha256: Some(
                        "8bb2341a4b42383f1d0ca9cb3f736da5cff6be35b71f1a2c37e365ea9a8329ab",
                    ),
                },
                HfFile {
                    name: "decoder.int8.onnx",
                    size: 3_955_863,
                    sha256: Some(
                        "7b2efacf70318c51e0024a6a229a63d5b901ee6c8643794d158109122d10ae9e",
                    ),
                },
                HfFile {
                    name: "joiner.int8.onnx",
                    size: 1_411_400,
                    sha256: Some(
                        "fce43322868e78bbebb1db0ebbe6617e0dbdc907bcba4af357edf0a625ee12d6",
                    ),
                },
                // tokens.txt last: its presence marks a complete download.
                HfFile {
                    name: "tokens.txt",
                    size: 9_953,
                    sha256: None,
                },
            ],
        },
        mb: 137,
        note: "English ATC (Parakeet-110m fine-tuned on ATC speech, CC-BY-4.0; default)",
    },
    Known {
        name: "moonshine-tiny",
        archive: "sherpa-onnx-moonshine-tiny-en-int8",
        from: Source::Release,
        mb: 108,
        note: "English, lowest CPU (Raspberry Pi 3/4)",
    },
    Known {
        name: "moonshine-base",
        archive: "sherpa-onnx-moonshine-base-en-int8",
        from: Source::Release,
        mb: 250,
        note: "English, low CPU",
    },
    Known {
        name: "moonshine-tiny-v2",
        archive: "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27",
        from: Source::Release,
        mb: 40,
        note: "English, Moonshine v2 (smaller download)",
    },
    Known {
        name: "moonshine-base-v2",
        archive: "sherpa-onnx-moonshine-base-en-quantized-2026-02-27",
        from: Source::Release,
        mb: 125,
        note: "English, Moonshine v2 (smaller download)",
    },
    Known {
        name: "parakeet-110m",
        archive: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8",
        from: Source::Release,
        mb: 108,
        note: "English, general purpose, same CPU as skycontrol-atc-110m",
    },
    Known {
        name: "parakeet-tdt-0.6b-v2",
        archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        from: Source::Release,
        mb: 600,
        note: "English, highest accuracy, about 4x the CPU of parakeet-110m",
    },
    Known {
        name: "parakeet-tdt-0.6b-v3",
        archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        from: Source::Release,
        mb: 640,
        note: "25 European languages",
    },
    Known {
        name: "sense-voice",
        archive: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17",
        from: Source::Release,
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
            let rec = OfflineRecognizer::create(&cfg).ok_or_else(|| {
                anyhow::anyhow!("sherpa-onnx could not load the model in {}", dir.display())
            })?;
            Ok(Self {
                rec,
                info: super::super::transcribe::planned(s),
            })
        }
    }

    /// Moonshine and Parakeet run best on few threads: the clips are short
    /// and the demodulator needs the rest.
    fn threads(s: &TranscribeSettings) -> i32 {
        if s.threads > 0 {
            return s.threads as i32;
        }
        std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(1, 4))
            .unwrap_or(1) as i32
    }

    /// Fill in the recognizer for whatever model family `dir` holds.
    pub(super) fn configure(
        cfg: &mut OfflineRecognizerConfig,
        dir: &Path,
        s: &TranscribeSettings,
    ) -> anyhow::Result<()> {
        let f = |names: &[&str]| -> Option<String> {
            names
                .iter()
                .map(|n| dir.join(n))
                .find(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned())
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
        if let (Some(enc), Some(dec)) =
            (f(&["encoder_model.ort"]), f(&["decoder_model_merged.ort"]))
        {
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
            m.sense_voice.language = Some(if s.language == "auto" {
                "auto".into()
            } else {
                s.language.clone()
            });
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
                super::MODELS
                    .iter()
                    .map(|m| m.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let root = model_dir().join("sherpa");
        let dir = root.join(k.archive);
        if dir.join("tokens.txt").is_file() {
            return Ok(dir);
        }
        std::fs::create_dir_all(&root)?;
        tracing::info!(
            "downloading speech model {} (~{} MB) to {} (once)",
            k.name,
            k.mb,
            dir.display()
        );
        // Fetch into a temporary directory next to the target, then move it
        // into place, so a cut download never looks like a model.
        let tmp = root.join(format!(".{}.part{}", k.archive, std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(20))
            .build();
        let r = match k.from {
            super::Source::Release => fetch_release(&agent, k, &tmp),
            super::Source::HuggingFace { repo, rev, files } => {
                fetch_hf(&agent, repo, rev, files, &tmp)
            }
        }
        .and_then(|inner| {
            anyhow::ensure!(
                inner.join("tokens.txt").is_file(),
                "download has no tokens.txt"
            );
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::rename(&inner, &dir)?;
            Ok(())
        });
        let _ = std::fs::remove_dir_all(&tmp);
        r.map_err(|e| e.context(format!("downloading speech model {}", k.name)))?;
        tracing::info!("speech model {} ready", k.name);
        Ok(dir)
    }

    /// A sherpa-onnx release archive, unpacked into `tmp`. Returns the
    /// unpacked model directory.
    fn fetch_release(agent: &ureq::Agent, k: &super::Known, tmp: &Path) -> anyhow::Result<PathBuf> {
        // XNG_SHERPA_MODELS_URL points at a mirror of the release assets.
        let base = std::env::var("XNG_SHERPA_MODELS_URL").unwrap_or_else(|_| {
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models".into()
        });
        let url = format!("{}/{}.tar.bz2", base.trim_end_matches('/'), k.archive);
        let resp = agent
            .get(&url)
            .call()
            .map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
        let mut ar = tar::Archive::new(bzip2::read::BzDecoder::new(resp.into_reader()));
        // unpack() refuses entries that would land outside `tmp`.
        ar.unpack(tmp).map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
        Ok(tmp.join(k.archive))
    }

    /// Files from a Hugging Face repo at a pinned commit, each checked by
    /// size and (for LFS files) sha256. Returns `tmp`.
    pub(super) fn fetch_hf(
        agent: &ureq::Agent,
        repo: &str,
        rev: &str,
        files: &[super::HfFile],
        tmp: &Path,
    ) -> anyhow::Result<PathBuf> {
        use sha2::Digest;
        use std::io::{Read, Write};
        // XNG_HF_URL points at a mirror laid out like huggingface.co.
        let base = std::env::var("XNG_HF_URL").unwrap_or_else(|_| "https://huggingface.co".into());
        for f in files {
            let url = format!(
                "{}/{repo}/resolve/{rev}/{}",
                base.trim_end_matches('/'),
                f.name
            );
            let resp = agent
                .get(&url)
                .call()
                .map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
            let mut rd = resp.into_reader().take(f.size + 1);
            let mut out = std::io::BufWriter::new(std::fs::File::create(tmp.join(f.name))?);
            let mut hash = sha2::Sha256::new();
            let mut buf = vec![0u8; 1 << 16];
            let mut n = 0u64;
            loop {
                let got = rd
                    .read(&mut buf)
                    .map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
                if got == 0 {
                    break;
                }
                hash.update(&buf[..got]);
                out.write_all(&buf[..got])?;
                n += got as u64;
            }
            out.flush()?;
            anyhow::ensure!(n == f.size, "{url}: got {n} bytes, expected {}", f.size);
            if let Some(want) = f.sha256 {
                let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
                anyhow::ensure!(got == want, "{url}: sha256 {got}, expected {want}");
            }
        }
        Ok(tmp.to_path_buf())
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
            Ok(vec![Segment {
                text,
                no_speech: None,
                prob: None,
            }])
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
        let d = dir_with(
            "pk",
            &[
                "encoder.int8.onnx",
                "decoder.int8.onnx",
                "joiner.int8.onnx",
                "tokens.txt",
            ],
        );
        let mut c = OfflineRecognizerConfig::default();
        super::engine::configure(&mut c, &d, &s).unwrap();
        assert!(c
            .model_config
            .transducer
            .joiner
            .as_deref()
            .unwrap()
            .ends_with("joiner.int8.onnx"));
        assert_eq!(
            c.model_config.model_type.as_deref(),
            Some("nemo_transducer")
        );

        let d = dir_with(
            "m1",
            &[
                "preprocess.onnx",
                "encode.int8.onnx",
                "cached_decode.int8.onnx",
                "uncached_decode.int8.onnx",
                "tokens.txt",
            ],
        );
        let mut c = OfflineRecognizerConfig::default();
        super::engine::configure(&mut c, &d, &s).unwrap();
        assert!(c
            .model_config
            .moonshine
            .cached_decoder
            .as_deref()
            .unwrap()
            .ends_with("/cached_decode.int8.onnx"));
        assert!(c
            .model_config
            .moonshine
            .uncached_decoder
            .as_deref()
            .unwrap()
            .ends_with("/uncached_decode.int8.onnx"));

        let d = dir_with(
            "m2",
            &[
                "encoder_model.ort",
                "decoder_model_merged.ort",
                "tokens.txt",
            ],
        );
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

    /// Hugging Face files are fetched from a pinned commit and checked by
    /// size and sha256.
    #[test]
    fn hf_download_checks() {
        use super::HfFile;
        use std::io::{Read, Write};
        let body = b"tokens 0\n".to_vec();
        // Serve whatever is asked with `body`, counting the requests.
        let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = srv.local_addr().unwrap();
        let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = paths.clone();
        let b = body.clone();
        std::thread::spawn(move || {
            for c in srv.incoming() {
                let mut c = c.unwrap();
                let mut req = [0u8; 1024];
                let n = c.read(&mut req).unwrap();
                let line = String::from_utf8_lossy(&req[..n])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                seen.lock()
                    .unwrap()
                    .push(line.split(' ').nth(1).unwrap_or("").to_string());
                write!(
                    c,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    b.len()
                )
                .unwrap();
                c.write_all(&b).unwrap();
            }
        });
        std::env::set_var("XNG_HF_URL", format!("http://{addr}"));
        let agent = ureq::AgentBuilder::new().build();
        let sha = "7f8b1fd6e3fa4e0d8e8ea2ebb5bcb4a0b0f3b0bc0bd3b7c06a9a1f8f0d2b3b1e";
        let tmp = std::env::temp_dir().join(format!("xng-hf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Right size, no hash: accepted, fetched from the pinned commit.
        let ok = [HfFile {
            name: "tokens.txt",
            size: body.len() as u64,
            sha256: None,
        }];
        let d = super::engine::fetch_hf(&agent, "org/m", "abc123", &ok, &tmp).unwrap();
        assert_eq!(std::fs::read(d.join("tokens.txt")).unwrap(), body);
        assert_eq!(paths.lock().unwrap()[0], "/org/m/resolve/abc123/tokens.txt");

        // Wrong size or wrong hash: refused.
        let short = [HfFile {
            name: "tokens.txt",
            size: 3,
            sha256: None,
        }];
        assert!(super::engine::fetch_hf(&agent, "org/m", "abc123", &short, &tmp).is_err());
        let bad = [HfFile {
            name: "tokens.txt",
            size: body.len() as u64,
            sha256: Some(sha),
        }];
        let e = super::engine::fetch_hf(&agent, "org/m", "abc123", &bad, &tmp).unwrap_err();
        assert!(e.to_string().contains("sha256"), "{e}");
        std::env::remove_var("XNG_HF_URL");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The default is a known model and every name is unique.
    #[test]
    fn catalog() {
        assert!(super::find(super::DEFAULT).is_some());
        let mut names: Vec<_> = super::MODELS.iter().map(|m| m.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), super::MODELS.len());
    }
}
