//! Voice input (`voice_input.py`): a recorded utterance → text.
//!
//! The page records 16 kHz mono PCM with an energy VAD (auto-stop after ~1.2 s of silence,
//! 60 s cap) and posts a WAV. Transcription prefers the bundled sherpa-onnx streaming
//! zipformer for the chosen language (CPU, faster than real time), falls back to the other
//! bundled language, then to an Ollama audio-capable model, and reports `Unavailable` when
//! none exists. The sherpa path is behind the `sherpa` cargo feature (native libraries);
//! model files are the same ones Mingbird ships, in `stt/<model dir>` under the install
//! directory or the agent home (`BOUBOU_STT_DIR` names an extra location first).

pub mod ollama;
#[cfg(feature = "sherpa")]
pub mod sherpa;
pub mod wav;

use std::path::{Path, PathBuf};

pub use wav::{encode_wav16, parse_wav, Pcm};

/// Bundled streaming zipformer directories per language (int8 weights only).
pub const STT_MODEL_DIRS: &[(&str, &str)] = &[
    ("zh", "sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23"),
    ("en", "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17"),
];

/// Where the models are fetched from (the k2-fsa release that Mingbird's installer bundles).
pub const STT_MODEL_URL_BASE: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/";

/// The recognizer's input rate.
pub const STT_SAMPLE_RATE: u32 = 16_000;

/// Transcripts are capped like Mingbird's (`[:500]`).
pub const MAX_TEXT_CHARS: usize = 500;

/// `_stt_bases`: candidate roots, each holding `stt/<model dir>`. `BOUBOU_STT_DIR` (a root
/// or a model directory itself) comes first, then the install dir, then the agent home.
pub fn stt_bases(install_dir: &Path, agent_home: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(e) = std::env::var_os("BOUBOU_STT_DIR").filter(|s| !s.is_empty()) {
        v.push(PathBuf::from(e));
    }
    v.push(install_dir.to_path_buf());
    v.push(agent_home.to_path_buf());
    v
}

/// `_stt_model_dir`: the model directory for `lang` (unknown language → Chinese), or `None`.
/// A base may be a root (`<base>/stt/<dir>`), a `stt` folder (`<base>/<dir>`), or the model
/// directory itself.
pub fn stt_model_dir(lang: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    let name = STT_MODEL_DIRS
        .iter()
        .find(|(l, _)| *l == lang)
        .or_else(|| STT_MODEL_DIRS.first())
        .map(|(_, d)| *d)?;
    for base in bases {
        for cand in [base.join("stt").join(name), base.join(name), base.clone()] {
            if cand.file_name().and_then(|n| n.to_str()) == Some(name) && cand.is_dir() {
                return Some(cand);
            }
        }
    }
    None
}

/// The int8 transducer triple plus the token table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttFiles {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

/// `_stt_files`: find the int8 encoder/decoder/joiner (robust to epoch naming) and
/// `tokens.txt`; `None` when the set is incomplete.
pub fn stt_files(dir: &Path) -> Option<SttFiles> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let pick = |part: &str| -> Option<PathBuf> {
        names
            .iter()
            .find(|n| n.ends_with(".onnx") && n.contains("int8") && n.contains(part))
            .map(|n| dir.join(n))
    };
    let tokens = dir.join("tokens.txt");
    if !tokens.is_file() {
        return None;
    }
    Some(SttFiles {
        encoder: pick("encoder")?,
        decoder: pick("decoder")?,
        joiner: pick("joiner")?,
        tokens,
    })
}

/// Languages whose bundled model is present (regardless of the `sherpa` feature).
pub fn installed_langs(bases: &[PathBuf]) -> Vec<&'static str> {
    STT_MODEL_DIRS
        .iter()
        .filter(|(l, _)| stt_model_dir(l, bases).is_some_and(|d| stt_files(&d).is_some()))
        .map(|(l, _)| *l)
        .collect()
}

/// Whether local transcription can run here: the native engine is compiled in *and* the
/// model for `lang` (or any language) is installed.
pub fn local_stt_available(lang: &str, bases: &[PathBuf]) -> bool {
    cfg!(feature = "sherpa")
        && (stt_model_dir(lang, bases).is_some_and(|d| stt_files(&d).is_some())
            || !installed_langs(bases).is_empty())
}

/// Human-readable hint for a missing model (the CLI and the page show it).
pub fn install_hint(lang: &str, agent_home: &Path) -> String {
    let dir = STT_MODEL_DIRS
        .iter()
        .find(|(l, _)| *l == lang)
        .or_else(|| STT_MODEL_DIRS.first())
        .map(|(_, d)| *d)
        .unwrap_or_default();
    format!(
        "语音模型未安装:下载 {STT_MODEL_URL_BASE}{dir}.tar.bz2 并解压到 {}/stt/{dir}/(只需 *int8.onnx 与 tokens.txt)",
        agent_home.display()
    )
}

/// Which engine produced the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    /// Local sherpa-onnx, with the language whose model was used.
    Sherpa(String),
    /// An Ollama audio-capable model.
    Ollama(String),
}

impl Engine {
    pub fn as_str(&self) -> &str {
        match self {
            Engine::Sherpa(_) => "sherpa",
            Engine::Ollama(_) => "ollama",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub engine: Engine,
    pub duration_secs: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoiceError {
    #[error("bad_audio: {0}")]
    BadAudio(String),
    /// Below the energy floor: nothing was said (`未捕捉到声音,请重试`).
    #[error("no_voice")]
    NoVoice,
    /// No local model and no audio-capable Ollama model.
    #[error("stt_unavailable: {0}")]
    Unavailable(String),
    #[error("transcribe_failed: {0}")]
    Failed(String),
}

/// One transcription request.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub wav: &'a [u8],
    /// `zh` | `en` (the prefs' `stt_lang`).
    pub lang: &'a str,
    pub bases: &'a [PathBuf],
    /// Ollama host for the fallback.
    pub ollama_host: &'a str,
    /// A model to try when no local engine applies (`pick_stt_model`); `None` = no fallback.
    pub fallback_model: Option<&'a str>,
}

/// RMS below this is treated as silence (the recorder's VAD uses 0.008 for "speech").
pub const SILENCE_RMS: f32 = 0.002;

/// `transcribe`: local sherpa (chosen language, then Chinese) → Ollama audio model → error.
pub fn transcribe(req: &Request<'_>) -> Result<Transcript, VoiceError> {
    let pcm = parse_wav(req.wav).map_err(VoiceError::BadAudio)?;
    if pcm.samples.is_empty() || pcm.rms() < SILENCE_RMS {
        return Err(VoiceError::NoVoice);
    }
    let duration = pcm.duration_secs();
    let pcm16k = pcm.resampled(STT_SAMPLE_RATE);

    #[cfg(feature = "sherpa")]
    {
        let mut langs: Vec<&str> = vec![req.lang];
        if req.lang != "zh" {
            langs.push("zh");
        }
        for lang in langs {
            if let Some(dir) = stt_model_dir(lang, req.bases) {
                if let Some(files) = stt_files(&dir) {
                    match sherpa::transcribe(&files, &pcm16k) {
                        Ok(text) if !text.trim().is_empty() => {
                            return Ok(Transcript {
                                text: cap(&text),
                                engine: Engine::Sherpa(lang.to_string()),
                                duration_secs: duration,
                            });
                        }
                        Ok(_) => return Err(VoiceError::NoVoice),
                        Err(e) => return Err(VoiceError::Failed(e)),
                    }
                }
            }
        }
    }

    match req.fallback_model {
        Some(model) if !model.is_empty() => {
            let wav16 = encode_wav16(&pcm16k);
            let text = ollama::transcribe_ollama(req.ollama_host, model, &wav16)
                .map_err(VoiceError::Failed)?;
            if text.trim().is_empty() {
                return Err(VoiceError::NoVoice);
            }
            Ok(Transcript {
                text: cap(&text),
                engine: Engine::Ollama(model.to_string()),
                duration_secs: duration,
            })
        }
        _ => {
            let why = if cfg!(feature = "sherpa") {
                "no_stt_model_installed"
            } else {
                "built_without_voice_feature"
            };
            Err(VoiceError::Unavailable(why.to_string()))
        }
    }
}

fn cap(s: &str) -> String {
    s.trim().chars().take(MAX_TEXT_CHARS).collect()
}
