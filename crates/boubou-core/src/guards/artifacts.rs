//! Finish gates on deliverables: files the summary or the plan names must exist.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::todo::load_todo;

pub const ARTIFACT_EXTS: [&str; 18] = [
    ".md", ".txt", ".png", ".jpg", ".jpeg", ".csv", ".py", ".json", ".docx", ".xlsx", ".pdf",
    ".html", ".yaml", ".yml", ".svg", ".tex", ".tsv", ".log",
];

static FILE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9_\-\x{4e00}-\x{9fff}][A-Za-z0-9_\-\x{4e00}-\x{9fff}]*\.[A-Za-z0-9]{2,5}")
        .expect("regex")
});

fn all_basenames_lower(dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            all_basenames_lower(&p, out);
        } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            out.push(n.to_lowercase());
        }
    }
}

/// `_missing_files_in_text`: artifact-looking tokens in `text` that do not exist under
/// `workdir`. A token counts as present if the relative path exists, a file with that
/// basename exists anywhere below, or an existing basename is a suffix of the token
/// (Chinese verb prefixes glue onto filenames: `生成了温度分布图.png`).
pub fn missing_files_in_text(text: &str, workdir: &Path) -> Vec<String> {
    let mut toks: Vec<String> = FILE_TOKEN
        .find_iter(text)
        .map(|m| m.as_str().trim().to_string())
        .filter(|t| {
            let low = t.to_lowercase();
            ARTIFACT_EXTS.iter().any(|e| low.ends_with(e))
        })
        .collect();
    toks.sort();
    toks.dedup();
    if toks.is_empty() {
        return Vec::new();
    }
    let mut all_bn = Vec::new();
    all_basenames_lower(workdir, &mut all_bn);
    let mut missing = Vec::new();
    for tok in toks {
        let rel = tok.replace('\\', "/");
        let rel = rel.trim_start_matches(['.', '/']);
        if workdir.join(rel).is_file() {
            continue;
        }
        let base = rel.rsplit('/').next().unwrap_or(rel).to_lowercase();
        if all_bn
            .iter()
            .any(|bn| *bn == base || base.ends_with(bn.as_str()))
        {
            continue;
        }
        missing.push(tok);
    }
    missing
}

/// `_claimed_missing_files`: files the finish summary claims that are not there.
pub fn claimed_missing_files(summary: &str, workdir: &Path) -> Vec<String> {
    missing_files_in_text(summary, workdir)
}

/// `_plan_named_missing`: files the model's own plan names that are not there.
/// Checkboxes are not trusted; only the filesystem is.
pub fn plan_named_missing(workdir: &Path) -> Vec<String> {
    let t = load_todo(workdir);
    if t.is_empty() {
        return Vec::new();
    }
    let text = t
        .iter()
        .map(|x| x.item.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    missing_files_in_text(&text, workdir)
}
