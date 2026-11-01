//! Local streaming transcription through sherpa-onnx (feature `sherpa`). One recognizer
//! per model directory is built on first use and cached for the process (construction is
//! ~100 ms; decoding a sentence is faster than real time on one CPU thread).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};

use crate::wav::Pcm;
use crate::SttFiles;

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Arc<OnlineRecognizer>>>> =
    LazyLock::new(Default::default);

fn recognizer(files: &SttFiles) -> Result<Arc<OnlineRecognizer>, String> {
    let key = files.encoder.clone();
    if let Some(r) = CACHE.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(r);
    }
    let mut cfg = OnlineRecognizerConfig::default();
    let p = |pb: &PathBuf| Some(pb.to_string_lossy().into_owned());
    cfg.model_config.transducer.encoder = p(&files.encoder);
    cfg.model_config.transducer.decoder = p(&files.decoder);
    cfg.model_config.transducer.joiner = p(&files.joiner);
    cfg.model_config.tokens = p(&files.tokens);
    cfg.model_config.num_threads = 1;
    cfg.model_config.provider = Some("cpu".into());
    cfg.decoding_method = Some("greedy_search".into());
    cfg.feat_config.sample_rate = crate::STT_SAMPLE_RATE as i32;
    cfg.feat_config.feature_dim = 80;
    let r = Arc::new(
        OnlineRecognizer::create(&cfg)
            .ok_or_else(|| "sherpa_recognizer_create_failed".to_string())?,
    );
    if let Ok(mut c) = CACHE.lock() {
        c.insert(key, r.clone());
    }
    Ok(r)
}

/// Decode one utterance (16 kHz mono). A short silent tail is appended so the final frames
/// are emitted, as Mingbird's `transcribe_local` relies on the recognizer draining.
pub fn transcribe(files: &SttFiles, pcm: &Pcm) -> Result<String, String> {
    let r = recognizer(files)?;
    let stream = r.create_stream();
    stream.accept_waveform(pcm.sample_rate as i32, &pcm.samples);
    let tail = vec![0.0f32; (pcm.sample_rate as usize) / 2];
    stream.accept_waveform(pcm.sample_rate as i32, &tail);
    stream.input_finished();
    while r.is_ready(&stream) {
        r.decode(&stream);
    }
    Ok(r.get_result(&stream)
        .map(|res| res.text.trim().to_string())
        .unwrap_or_default())
}
