//! `gui_prefs.json`: the UI's sampling / context / system-prompt preferences, shared with
//! the desktop shell. Same keys and the same `prefver` migration as Mingbird.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

pub const PREFS_FILE: &str = "gui_prefs.json";
/// Keys the status endpoint exposes and `POST /api/prefs` accepts.
pub const KEYS: [&str; 9] = [
    "stt_lang",
    "ctx",
    "temp",
    "num_predict",
    "think",
    "sys_enable",
    "sys_text",
    "ui_mode",
    "batch",
];

pub fn prefs_path(home: &Path) -> PathBuf {
    home.join(PREFS_FILE)
}

fn defaults() -> Map<String, Value> {
    let v = json!({
        "ui_mode": "auto", "ctx": 131072, "temp": null, "num_predict": 2048,
        "think": null, "sys_enable": false, "sys_text": "", "batch": false, "prefver": 2
    });
    v.as_object().cloned().unwrap_or_default()
}

/// `_prefs_load`: defaults overlaid with the file. A pre-`prefver` file's `temp: 0` and
/// `think: true` were hidden defaults, not choices, so they migrate to "unset".
pub fn load(home: &Path) -> Map<String, Value> {
    let mut d = defaults();
    let Some(file) = std::fs::read_to_string(prefs_path(home))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
    else {
        return d;
    };
    let legacy = !file.contains_key("prefver");
    for (k, v) in file {
        d.insert(k, v);
    }
    if legacy {
        if d.get("temp").and_then(Value::as_f64) == Some(0.0) {
            d.insert("temp".into(), Value::Null);
        }
        if d.get("think") == Some(&Value::Bool(true)) {
            d.insert("think".into(), Value::Null);
        }
    }
    d
}

/// `_prefs_save`: atomic write with the `prefver` marker.
pub fn save(home: &Path, prefs: &Map<String, Value>) -> std::io::Result<()> {
    let mut p = prefs.clone();
    p.entry("prefver").or_insert(json!(2));
    std::fs::create_dir_all(home)?;
    let path = prefs_path(home);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&p)?)?;
    std::fs::rename(tmp, path)
}

/// The subset the frontend reads from `/api/status`.
pub fn public(prefs: &Map<String, Value>) -> Map<String, Value> {
    KEYS.iter()
        .map(|k| (k.to_string(), prefs.get(*k).cloned().unwrap_or(Value::Null)))
        .collect()
}
