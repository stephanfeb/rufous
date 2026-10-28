//! `_EditConvergenceGuard` (Task #72): the same file edited "successfully" over and over
//! while the goal never converges. Never disables or throttles; injects a strategy hint,
//! at most two per file, the second one stricter.

use std::collections::HashMap;

/// `_norm_p`: same-file identity for paths the model spells differently
/// (`./m08.py`, `M08.PY`, `sub\a.py`). Case-insensitive on every platform: the guard is
/// about intent, and the primary target (Windows) is case-insensitive anyway.
pub fn norm_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = s.trim_start_matches(['.', '/']);
    let mut parts: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            x => parts.push(x),
        }
    }
    parts.join("/").to_lowercase()
}

#[derive(Debug, Clone)]
pub struct EditConvergenceGuard {
    threshold: usize,
    max_hints: usize,
    streak: usize,
    last_path: Option<String>,
    hints: HashMap<String, usize>,
}

impl Default for EditConvergenceGuard {
    fn default() -> Self {
        Self::new(4, 2)
    }
}

impl EditConvergenceGuard {
    pub fn new(threshold: usize, max_hints: usize) -> Self {
        Self {
            threshold,
            max_hints,
            streak: 0,
            last_path: None,
            hints: HashMap::new(),
        }
    }

    /// Record one successful edit of `path` (callers only pass edits whose receipt starts
    /// with `[edited`). Returns the hint to inject, if any.
    pub fn observe(&mut self, path: &str) -> Option<String> {
        let p = norm_path(path);
        if p.is_empty() {
            self.streak = 0;
            self.last_path = None;
            return None;
        }
        if self.last_path.as_deref() == Some(p.as_str()) {
            self.streak += 1;
        } else {
            self.streak = 1;
            self.last_path = Some(p.clone());
        }
        if self.streak < self.threshold {
            return None;
        }
        let count = self.hints.entry(p.clone()).or_insert(0);
        if *count >= self.max_hints {
            return None;
        }
        *count += 1;
        let n = self.streak;
        Some(if *count == 1 {
            format!(
                "⚠️ You have edited {p} successfully {n} times in a row and the goal is still not met. Stop speculative tweaking: read the full run_bash failure output to find the root cause (add temporary prints for intermediate values if needed). If the current approach is a dead end, restore the .bak backup or rewrite that part of the file with create_file."
            )
        } else {
            format!(
                "⚠️ Still editing the same spot in {p} (successful edit number {n}). Rule: before this edit, state your root-cause hypothesis and verify it with one run_bash; no further edit_file until you have read a new error. If two hypotheses in a row fail, restore the .bak backup and change approach."
            )
        })
    }
}
