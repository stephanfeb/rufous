//! Voice input without a microphone: WAV parsing/encoding, resampling, model-directory
//! resolution (the int8 discipline from `test_v191_universal.py::TestBilingualSTT`), the
//! silence gate, the Ollama transcript post-processing, and — when a bundled model is
//! present and the `sherpa` feature is on — a real transcription of its sample WAV.

use std::path::{Path, PathBuf};

use boubou_voice::ollama::extract_transcript;
use boubou_voice::{
    encode_wav16, install_hint, installed_langs, local_stt_available, parse_wav, stt_files,
    stt_model_dir, transcribe, Pcm, Request, VoiceError, STT_MODEL_DIRS,
};
use serde_json::json;

fn tone(rate: u32, secs: f32, amp: f32) -> Pcm {
    let n = (rate as f32 * secs) as usize;
    let samples = (0..n)
        .map(|i| amp * (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin())
        .collect();
    Pcm {
        samples,
        sample_rate: rate,
    }
}

#[test]
fn wav16_round_trip_and_duration() {
    let p = tone(16_000, 0.5, 0.5);
    let bytes = encode_wav16(&p);
    assert_eq!(&bytes[0..4], b"RIFF");
    let back = parse_wav(&bytes).expect("parse");
    assert_eq!(back.sample_rate, 16_000);
    assert_eq!(back.samples.len(), p.samples.len());
    assert!((back.duration_secs() - 0.5).abs() < 0.01);
    let err: f32 = back
        .samples
        .iter()
        .zip(&p.samples)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(err < 1e-3, "{err}");
}

#[test]
fn parse_handles_stereo_float_and_odd_chunks() {
    // Hand-built 48 kHz stereo IEEE-float WAV with a LIST chunk before data.
    let rate = 48_000u32;
    let frames: Vec<f32> = (0..480)
        .flat_map(|i| [i as f32 / 480.0, -(i as f32) / 480.0])
        .collect();
    let mut data = Vec::new();
    for f in &frames {
        data.extend_from_slice(&f.to_le_bytes());
    }
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&0u32.to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 8).to_le_bytes());
    w.extend_from_slice(&8u16.to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"LIST");
    w.extend_from_slice(&3u32.to_le_bytes());
    w.extend_from_slice(b"abc\0"); // odd size → pad byte
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    let p = parse_wav(&w).expect("parse");
    assert_eq!(p.sample_rate, 48_000);
    assert_eq!(p.samples.len(), 480);
    assert!(
        p.samples.iter().all(|s| s.abs() < 1e-6),
        "L+R cancel to mono silence"
    );
    let r = p.resampled(16_000);
    assert_eq!(r.sample_rate, 16_000);
    assert_eq!(r.samples.len(), 160);
    assert_eq!(parse_wav(b"not a wav").expect_err("garbage"), "not_a_wav");
}

#[test]
fn resample_preserves_duration() {
    let p = tone(44_100, 1.0, 0.3);
    let r = p.resampled(16_000);
    assert!((r.duration_secs() - 1.0).abs() < 0.01);
    assert!((r.rms() - p.rms()).abs() < 0.02);
}

fn fake_model_dir(root: &Path, lang_dir: &str, int8: bool) -> PathBuf {
    let d = root.join("stt").join(lang_dir);
    std::fs::create_dir_all(&d).expect("mkdir");
    let suffix = if int8 { ".int8.onnx" } else { ".onnx" };
    for part in ["encoder", "decoder", "joiner"] {
        std::fs::write(d.join(format!("{part}-epoch-99-avg-1{suffix}")), "x").expect("write");
    }
    std::fs::write(d.join("tokens.txt"), "<blk>\n").expect("write");
    d
}

#[test]
fn model_dir_resolution_and_int8_discipline() {
    let root = tempfile::tempdir().expect("tmp");
    let bases = vec![root.path().to_path_buf()];
    assert!(stt_model_dir("en", &bases).is_none());
    assert!(installed_langs(&bases).is_empty());
    let en = fake_model_dir(root.path(), STT_MODEL_DIRS[0].1, true);
    assert_eq!(stt_model_dir("en", &bases), Some(en.clone()));
    assert_eq!(
        stt_model_dir("xx", &bases),
        Some(en.clone()),
        "unknown language → English"
    );
    let f = stt_files(&en).expect("files");
    for p in [&f.encoder, &f.decoder, &f.joiner] {
        assert!(p.to_string_lossy().contains("int8"));
    }
    assert!(f.tokens.is_file());
    assert_eq!(installed_langs(&bases), ["en"]);
    // float-only weights do not count as an installed model
    let zh = fake_model_dir(root.path(), STT_MODEL_DIRS[1].1, false);
    assert!(stt_files(&zh).is_none());
    assert_eq!(installed_langs(&bases), ["en"]);
    // A base may name the model directory itself (BOUBOU_STT_DIR pointing straight at it).
    assert_eq!(stt_model_dir("en", std::slice::from_ref(&en)), Some(en));
    assert!(install_hint("zh", root.path()).contains(STT_MODEL_DIRS[1].1));
}

#[test]
fn availability_follows_feature_and_files() {
    let root = tempfile::tempdir().expect("tmp");
    let bases = vec![root.path().to_path_buf()];
    assert!(!local_stt_available("zh", &bases));
    fake_model_dir(root.path(), STT_MODEL_DIRS[0].1, true);
    assert_eq!(local_stt_available("zh", &bases), cfg!(feature = "sherpa"));
}

#[test]
fn silence_is_no_voice_and_no_engine_is_unavailable() {
    let root = tempfile::tempdir().expect("tmp");
    let bases = vec![root.path().to_path_buf()];
    let silent = encode_wav16(&tone(16_000, 0.5, 0.0));
    let req = Request {
        wav: &silent,
        lang: "zh",
        bases: &bases,
        ollama_host: "http://127.0.0.1:9",
        fallback_model: None,
    };
    assert_eq!(transcribe(&req), Err(VoiceError::NoVoice));
    let loud = encode_wav16(&tone(16_000, 0.5, 0.5));
    let req = Request { wav: &loud, ..req };
    match transcribe(&req) {
        Err(VoiceError::Unavailable(_)) => {}
        other => panic!("{other:?}"),
    }
    let req = Request {
        wav: b"garbage",
        ..req
    };
    assert!(matches!(transcribe(&req), Err(VoiceError::BadAudio(_))));
}

#[test]
fn ollama_transcript_post_processing() {
    assert_eq!(extract_transcript(&json!({"content": " hello "})), "hello");
    assert_eq!(
        extract_transcript(&json!({"content": "", "thinking": "from thinking"})),
        "from thinking"
    );
    assert_eq!(
        extract_transcript(&json!({"content": "Transcript\nthe actual words"})),
        "the actual words"
    );
    assert_eq!(extract_transcript(&json!({"content": "Analysis"})), "");
    let long = "x".repeat(600);
    assert_eq!(
        extract_transcript(&json!({"content": long}))
            .chars()
            .count(),
        500
    );
}

/// Real transcription of the model's own sample (`test_wavs/0.wav`). Skipped unless the
/// Chinese model is installed (gitignored distribution asset) and the feature is on.
#[test]
fn real_zh_transcription_when_model_present() {
    let install = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let bases = boubou_voice::stt_bases(&install, &boubou_core::config::agent_home());
    let Some(dir) = stt_model_dir("zh", &bases).filter(|d| stt_files(d).is_some()) else {
        eprintln!("skip: zh STT model not installed");
        return;
    };
    if !cfg!(feature = "sherpa") {
        eprintln!("skip: built without the sherpa feature");
        return;
    }
    let wav_path = dir.join("test_wavs").join("0.wav");
    let Ok(wav) = std::fs::read(&wav_path) else {
        eprintln!("skip: {} missing", wav_path.display());
        return;
    };
    let req = Request {
        wav: &wav,
        lang: "zh",
        bases: &bases,
        ollama_host: "http://127.0.0.1:9",
        fallback_model: None,
    };
    let t = transcribe(&req).expect("transcribe");
    assert_eq!(t.engine.as_str(), "sherpa");
    assert!(t.text.chars().count() > 4, "{}", t.text);
    assert!(
        t.text
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{}",
        t.text
    );
}
