//! Minimal RIFF/WAVE handling: PCM 8/16/24/32-bit and IEEE float, any channel count
//! (down-mixed to mono), plus a linear resampler to the recognizer's 16 kHz and a 16-bit
//! mono encoder for the Ollama fallback. No external codec; the browser sends plain PCM.

/// Decoded audio: mono samples in `[-1, 1]` and their sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Pcm {
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples.len() as f32 / self.sample_rate as f32
        }
    }

    /// Root-mean-square level, the same energy measure the page's VAD uses.
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.samples.iter().map(|s| s * s).sum();
        (sum / self.samples.len() as f32).sqrt()
    }

    /// Resample to `rate` (linear interpolation; adequate for speech into a 16 kHz model).
    pub fn resampled(&self, rate: u32) -> Pcm {
        if rate == self.sample_rate || self.samples.is_empty() || rate == 0 {
            return Pcm {
                samples: self.samples.clone(),
                sample_rate: rate.max(1),
            };
        }
        let ratio = self.sample_rate as f64 / rate as f64;
        let n = ((self.samples.len() as f64) / ratio).floor() as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let pos = i as f64 * ratio;
            let i0 = pos.floor() as usize;
            let frac = (pos - i0 as f64) as f32;
            let a = self.samples[i0.min(self.samples.len() - 1)];
            let b = self.samples[(i0 + 1).min(self.samples.len() - 1)];
            out.push(a + (b - a) * frac);
        }
        Pcm {
            samples: out,
            sample_rate: rate,
        }
    }
}

fn le_u16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn le_u32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(i)?,
        *b.get(i + 1)?,
        *b.get(i + 2)?,
        *b.get(i + 3)?,
    ]))
}

/// Parse a WAV file. Errors are short English tokens the API returns verbatim.
pub fn parse_wav(bytes: &[u8]) -> Result<Pcm, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not_a_wav".into());
    }
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (format, channels, rate, bits)
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = le_u32(bytes, pos + 4).ok_or("truncated")? as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match id {
            b"fmt " => {
                let format = le_u16(body, 0).ok_or("bad_fmt")?;
                let channels = le_u16(body, 2).ok_or("bad_fmt")?;
                let rate = le_u32(body, 4).ok_or("bad_fmt")?;
                let bits = le_u16(body, 14).ok_or("bad_fmt")?;
                // WAVE_FORMAT_EXTENSIBLE: the real format is in the sub-format GUID's first u16.
                let format = if format == 0xFFFE {
                    le_u16(body, 24).unwrap_or(format)
                } else {
                    format
                };
                fmt = Some((format, channels, rate, bits));
            }
            b"data" => {
                data = Some(body);
            }
            _ => {}
        }
        pos = body_start + size + (size & 1); // chunks are word-aligned
    }
    let (format, channels, rate, bits) = fmt.ok_or("no_fmt_chunk")?;
    let data = data.ok_or("no_data_chunk")?;
    if channels == 0 || rate == 0 {
        return Err("bad_fmt".into());
    }
    let ch = channels as usize;
    let frames: Vec<f32> = match (format, bits) {
        (1, 8) => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
            .collect(),
        (1, 24) => data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_608.0)
            .collect(),
        (1, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c) as f32 / 2_147_483_648.0)
            .collect(),
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
        _ => return Err(format!("unsupported_format({format},{bits}bit)")),
    };
    let samples: Vec<f32> = if ch == 1 {
        frames
    } else {
        frames
            .chunks_exact(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    Ok(Pcm {
        samples: samples.iter().map(|s| s.clamp(-1.0, 1.0)).collect(),
        sample_rate: rate,
    })
}

/// Encode mono 16-bit PCM WAV (what the Ollama audio fallback and Mingbird's recorder write).
pub fn encode_wav16(pcm: &Pcm) -> Vec<u8> {
    let n = pcm.samples.len() as u32;
    let data_len = n * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&pcm.sample_rate.to_le_bytes());
    out.extend_from_slice(&(pcm.sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &pcm.samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}
