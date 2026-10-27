//! The Ollama side of voice input: which installed models accept audio, and
//! `transcribe_ollama` (the WAV goes as a base64 attachment with a transcription prompt).

use std::time::Duration;

use boubou_core::ollama_proc::block_on_fresh;
use serde_json::{json, Value};

/// Standard base64 (the core crate's encoder, shared with image attachments).
pub fn base64(bytes: &[u8]) -> String {
    boubou_core::text::base64_encode(bytes)
}

async fn post_json(url: String, body: Value, timeout: Duration) -> Result<Value, String> {
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let r = http
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status();
    if !status.is_success() {
        let t = r.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {}: {}",
            status.as_u16(),
            t.chars().take(200).collect::<String>()
        ));
    }
    r.json::<Value>().await.map_err(|e| e.to_string())
}

/// `model_audio_capable`: `/api/show` lists `audio` among the capabilities.
pub fn model_audio_capable(host: &str, model: &str) -> bool {
    let url = format!("{}/api/show", host.trim_end_matches('/'));
    let m = model.to_string();
    block_on_fresh(async move {
        post_json(url, json!({"model": m}), Duration::from_secs(10))
            .await
            .ok()
            .and_then(|v| {
                v.get("capabilities")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().any(|c| c.as_str() == Some("audio")))
            })
            .unwrap_or(false)
    })
}

/// `list_audio_models`: installed models that take audio, or whose name says whisper.
pub fn list_audio_models(host: &str) -> Vec<String> {
    let base = host.trim_end_matches('/').to_string();
    let names: Vec<String> = block_on_fresh(async move {
        let Ok(http) = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        else {
            return Vec::new();
        };
        let Ok(r) = http.get(format!("{base}/api/tags")).send().await else {
            return Vec::new();
        };
        let Ok(v) = r.json::<Value>().await else {
            return Vec::new();
        };
        v.get("models")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    });
    names
        .into_iter()
        .filter(|n| n.to_lowercase().contains("whisper") || model_audio_capable(host, n))
        .collect()
}

/// `pick_stt_model`: a whisper model when one is installed, else `preferred`.
pub fn pick_stt_model(host: &str, preferred: &str) -> String {
    list_audio_models(host)
        .into_iter()
        .find(|m| m.to_lowercase().contains("whisper"))
        .unwrap_or_else(|| preferred.to_string())
}

/// Leading markers some models print before the transcript; the first line is dropped.
pub const STRIP_MARKERS: &[&str] = &["Thinking Process", "Analysis", "转录结果", "以下为转录"];

/// `transcribe_ollama`'s post-processing: content, else thinking; a marker heading dropped.
pub fn extract_transcript(message: &Value) -> String {
    let s = |k: &str| {
        message
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let mut content = s("content");
    if content.is_empty() {
        content = s("thinking");
    }
    for marker in STRIP_MARKERS {
        if content.starts_with(marker) {
            content = match content.split_once('\n') {
                Some((_, rest)) => rest.to_string(),
                None => String::new(),
            };
            break;
        }
    }
    content.chars().take(500).collect()
}

/// `transcribe_ollama`: hand the WAV to an audio-capable model; `thinking` is forced off.
pub fn transcribe_ollama(host: &str, model: &str, wav: &[u8]) -> Result<String, String> {
    let url = format!("{}/api/chat", host.trim_end_matches('/'));
    let payload = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": "请把这段语音转录成文字,直接输出转录结果,不要解释。",
            "images": [base64(wav)],
        }],
        "stream": false,
        "think": false,
        "options": {"num_predict": 800},
    });
    let v = block_on_fresh(async move { post_json(url, payload, Duration::from_secs(600)).await })?;
    Ok(extract_transcript(v.get("message").unwrap_or(&Value::Null)))
}
