//! Text helpers shared by the executor and guards: lenient file decoding (UTF-8, then
//! GBK, then Latin-1, as `read_text` in Mingbird), console decoding, and char-based
//! slicing that matches Python string semantics.

use std::path::Path;

use crate::{Error, Result};

/// Read a text file trying UTF-8, GBK, then Latin-1 (never fails on encoding).
pub fn read_text(path: &Path) -> Result<String> {
    let raw = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    Ok(decode_lenient(&raw))
}

pub fn decode_lenient(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    let (s, _, had_errors) = encoding_rs::GBK.decode(raw);
    if !had_errors {
        return s.into_owned();
    }
    raw.iter().map(|&b| b as char).collect()
}

/// Subprocess output: UTF-8, then GBK, then cp1252, then lossy UTF-8. Never fails.
pub fn decode_console(raw: &[u8]) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    for enc in [encoding_rs::GBK, encoding_rs::WINDOWS_1252] {
        let (s, _, had_errors) = enc.decode(raw);
        if !had_errors {
            return s.into_owned();
        }
    }
    String::from_utf8_lossy(raw).into_owned()
}

/// First `n` chars (Python `s[:n]`).
pub fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// One log line out of any text: line breaks become ` ⏎ `, tabs a space (bead boubou-h5d:
/// a multi-line tool result split the ⚙ line and its status tag landed on the next one,
/// which the bench reads as no tag).
pub fn one_line(s: &str) -> String {
    s.replace("\r\n", " ⏎ ")
        .replace(['\n', '\r'], " ⏎ ")
        .replace('\t', " ")
}

/// Last `n` chars (Python `s[-n:]`).
pub fn tail(s: &str, n: usize) -> &str {
    let len = s.chars().count();
    if len <= n {
        return s;
    }
    let skip = len - n;
    match s.char_indices().nth(skip) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// Char count (Python `len`).
pub fn clen(s: &str) -> usize {
    s.chars().count()
}

/// Standard base64 (no external dependency needed for image attachment).
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Python `:g` formatting for the values we print (no exponent forms needed).
pub fn fmt_g(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}
