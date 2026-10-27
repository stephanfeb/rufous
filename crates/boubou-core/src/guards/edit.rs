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
                "⚠️ 你已对 {p} 连续成功编辑 {n} 次,但目标仍未达成。停止继续猜测性微调:先完整阅读 run_bash 的失败输出定位根因(必要时临时加 print 打印中间值);若确认当前思路不通,可用 .bak 备份恢复,或用 create_file 重写该文件的这一部分。"
            )
        } else {
            format!(
                "⚠️ 仍是 {p} 的同点反复修改(第 {n} 次成功编辑)。规则:这次编辑前必须先说出根因假设,并用一次 run_bash 验证假设;禁止在没读新报错前再次 edit_file。若连续两个假设都不成立,恢复 .bak 备份并换实现思路。"
            )
        })
    }
}
