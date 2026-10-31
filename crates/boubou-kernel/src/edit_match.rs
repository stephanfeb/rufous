//! `edit_file`'s matcher (bead boubou-bk9.3).
//!
//! The tool answers every call with the truth about the file as it is now, and the loop is
//! not consulted. Measured on gemma4:e2b (changelog 2026-10-31), the exact matcher's misses
//! were of two kinds it could not tell apart: a misquote (whitespace, quotes, escape
//! sequences) and a stale quote, where the model re-issues an edit against text its own
//! previous edit already changed; and a bare `old` of one word replaced the first of two
//! occurrences. So: [`find`] runs matching tiers in the order pi and opencode agree on, each
//! only when the previous found nothing, exact on the text as given first (the inherited
//! unconditional unescape turned a correct quote of a line containing `\n` into a miss on
//! every call of ED-06), then the escape sequences unescaped, then Unicode-normalised lines,
//! then trailing whitespace ignored, then typographic quotes and dashes normalised. A tier
//! applies only when it finds something; it never guesses below the tiers, because a silent
//! wrong edit costs more than a miss. Every tier counts occurrences, so the caller can refuse
//! an ambiguous `old` or replace them all. When nothing matches but the new text is already
//! in the file, the edit has already happened ([`Found::AlreadyApplied`]). When nothing
//! matches at all, the miss names the region of the file most like `old`, with line numbers,
//! so the model can re-quote without another read.
use unicode_normalization::UnicodeNormalization;

/// Which tier matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Exact,
    /// `\n`, `\t`, `\"` in `old` read as the characters (the caller does the same to `new`).
    Unescaped,
    /// Lines compared after NFKC normalisation.
    Nfkc,
    /// Lines compared after NFKC with trailing whitespace ignored.
    TrailingWs,
    /// As above, with typographic quotes, dashes and non-breaking spaces normalised.
    Typographic,
}

impl Tier {
    pub fn describe(self) -> &'static str {
        match self {
            Tier::Exact => "exactly",
            Tier::Unescaped => "with the escape sequences unescaped",
            Tier::Nfkc => "after Unicode normalisation",
            Tier::TrailingWs => "ignoring trailing whitespace",
            Tier::Typographic => "with typographic quotes and dashes normalised",
        }
    }
}

/// What [`find`] saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Every non-overlapping occurrence as a byte range of the file, in order, and its tier.
    Hits {
        tier: Tier,
        ranges: Vec<(usize, usize)>,
    },
    /// `old` is gone and `new` is already there: the edit has already happened. The lines
    /// of the file that hold the new text, 1-based inclusive, so the answer can show them
    /// (a small model re-issues the edit when told only to go and read).
    AlreadyApplied {
        first_line: usize,
        last_line: usize,
        region: String,
    },
    /// Nothing: the region most like `old`, 1-based inclusive line numbers and its text.
    Miss {
        first_line: usize,
        last_line: usize,
        region: String,
    },
}

/// The inherited rule: literal `\n`, `\t`, `\"` become the characters.
pub fn unescape(t: &str) -> String {
    t.replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
}

fn occurrences(file: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    file.match_indices(needle)
        .map(|(i, m)| (i, i + m.len()))
        .collect()
}

/// `(start, end_of_content, end_with_newline)` for every line of `file`.
fn line_spans(file: &str) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in file.char_indices() {
        if c == '\n' {
            out.push((start, i, i + 1));
            start = i + 1;
        }
    }
    if start < file.len() || file.is_empty() || file.ends_with('\n') && false {
        out.push((start, file.len(), file.len()));
    }
    out
}

fn nfkc(l: &str) -> String {
    l.nfkc().collect()
}

fn typographic(l: &str) -> String {
    l.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{2033}' => '"',
            '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            '\u{a0}' | '\u{2007}' | '\u{202f}' => ' ',
            c => c,
        })
        .collect()
}

fn normaliser(tier: Tier) -> fn(&str) -> String {
    match tier {
        Tier::Nfkc => nfkc,
        Tier::TrailingWs => |l| nfkc(l).trim_end().to_string(),
        Tier::Typographic => |l| typographic(&nfkc(l)).trim_end().to_string(),
        Tier::Exact | Tier::Unescaped => |l| l.to_string(),
    }
}

/// Windows of file lines equal to `old`'s lines under `norm`, as byte ranges. `old` ending
/// in a newline covers the last matched line's newline too, so `new` is used as given.
fn line_windows(file: &str, old: &str, norm: fn(&str) -> String) -> Vec<(usize, usize)> {
    let mut pieces: Vec<&str> = old.split('\n').collect();
    let trailing_nl = pieces.len() > 1 && pieces.last() == Some(&"");
    if trailing_nl {
        pieces.pop();
    }
    if pieces.is_empty() || pieces.iter().all(|p| p.trim().is_empty()) {
        return Vec::new();
    }
    let want: Vec<String> = pieces.iter().map(|p| norm(p)).collect();
    let spans = line_spans(file);
    let have: Vec<String> = spans.iter().map(|&(s, e, _)| norm(&file[s..e])).collect();
    let k = want.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + k <= have.len() {
        if have[i..i + k] == want[..] {
            let (s, _, _) = spans[i];
            let (_, e, e_nl) = spans[i + k - 1];
            out.push((s, if trailing_nl { e_nl } else { e }));
            i += k;
        } else {
            i += 1;
        }
    }
    out
}

fn bigrams(s: &str) -> Vec<(char, char)> {
    let cs: Vec<char> = s.trim().chars().collect();
    let mut v: Vec<(char, char)> = cs.windows(2).map(|w| (w[0], w[1])).collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn similarity(a: &[(char, char)], b: &[(char, char)]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut both) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    both as f64 / (a.len() + b.len() - both) as f64
}

/// The window of file lines most like `old`, by mean character-bigram similarity of the
/// aligned lines; the earliest on a tie.
fn nearest(file: &str, old: &str) -> (usize, usize, String) {
    let spans = line_spans(file);
    if spans.is_empty() {
        return (1, 1, String::new());
    }
    let mut old_lines: Vec<&str> = old.split('\n').collect();
    while old_lines.len() > 1 && old_lines.last().is_some_and(|l| l.trim().is_empty()) {
        old_lines.pop();
    }
    let want: Vec<Vec<(char, char)>> = old_lines.iter().map(|l| bigrams(l)).collect();
    let have: Vec<Vec<(char, char)>> = spans
        .iter()
        .map(|&(s, e, _)| bigrams(&file[s..e]))
        .collect();
    let k = want.len().clamp(1, have.len());
    let mut best = (0usize, -1.0f64);
    for i in 0..=have.len() - k {
        let score: f64 = (0..k)
            .map(|j| similarity(&have[i + j], want.get(j).unwrap_or(&Vec::new())))
            .sum::<f64>()
            / k as f64;
        if score > best.1 {
            best = (i, score);
        }
    }
    let (s, _, _) = spans[best.0];
    let (_, e, _) = spans[best.0 + k - 1];
    (best.0 + 1, best.0 + k, file[s..e].to_string())
}

/// Where `old` is in `file`, see the module doc.
pub fn find(file: &str, old: &str, new: &str) -> Found {
    let hits = occurrences(file, old);
    if !hits.is_empty() {
        return Found::Hits {
            tier: Tier::Exact,
            ranges: hits,
        };
    }
    if old.contains('\\') {
        let u = unescape(old);
        if u != old {
            let hits = occurrences(file, &u);
            if !hits.is_empty() {
                return Found::Hits {
                    tier: Tier::Unescaped,
                    ranges: hits,
                };
            }
        }
    }
    for tier in [Tier::Nfkc, Tier::TrailingWs, Tier::Typographic] {
        let hits = line_windows(file, old, normaliser(tier));
        if !hits.is_empty() {
            return Found::Hits { tier, ranges: hits };
        }
    }
    if !new.trim().is_empty() {
        let at = file.find(new).map(|i| (i, i + new.len())).or_else(|| {
            let u = unescape(new);
            file.find(&u).map(|i| (i, i + u.len()))
        });
        if let Some((s, e)) = at {
            let (first_line, last_line, region) = lines_around(file, s, e);
            return Found::AlreadyApplied {
                first_line,
                last_line,
                region,
            };
        }
    }
    let (first_line, last_line, region) = nearest(file, old);
    Found::Miss {
        first_line,
        last_line,
        region,
    }
}

/// The whole lines of `file` covering the byte range `s..e`, 1-based inclusive numbers.
fn lines_around(file: &str, s: usize, e: usize) -> (usize, usize, String) {
    let spans = line_spans(file);
    let first = spans.iter().position(|&(_, _, nl)| s < nl).unwrap_or(0);
    let last = spans
        .iter()
        .rposition(|&(a, _, _)| a < e.max(s + 1))
        .unwrap_or(first)
        .max(first);
    let (a, _, _) = spans[first];
    let (_, b, _) = spans[last];
    (first + 1, last + 1, file[a..b].to_string())
}

/// `file` with `new` in place of the first range, or of every range when `all`.
pub fn apply(file: &str, ranges: &[(usize, usize)], new: &str, all: bool) -> String {
    let take = if all {
        ranges.len()
    } else {
        ranges.len().min(1)
    };
    let mut out = String::with_capacity(file.len() + new.len());
    let mut at = 0;
    for &(s, e) in &ranges[..take] {
        out.push_str(&file[at..s]);
        out.push_str(new);
        at = e;
    }
    out.push_str(&file[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(f: Found) -> (Tier, Vec<(usize, usize)>) {
        match f {
            Found::Hits { tier, ranges } => (tier, ranges),
            other => panic!("expected hits, got {other:?}"),
        }
    }

    #[test]
    fn exact_wins_and_is_counted() {
        let (t, r) = hits(find("a b a\n", "a", "z"));
        assert_eq!((t, r), (Tier::Exact, vec![(0, 1), (4, 5)]));
        assert_eq!(apply("a b a\n", &[(0, 1), (4, 5)], "z", false), "z b a\n");
        assert_eq!(apply("a b a\n", &[(0, 1), (4, 5)], "z", true), "z b z\n");
    }

    #[test]
    fn a_source_escape_is_matched_as_written_before_any_unescaping() {
        let file = "return \",\".join(cells) + \"\\n\"\n"; // the file has backslash-n
        let (t, _) = hits(find(file, "return \",\".join(cells) + \"\\n\"", "x"));
        assert_eq!(t, Tier::Exact);
        // A real newline quoted as backslash-n is the unescape tier.
        let (t, r) = hits(find("a = 1\nb = 2\n", "a = 1\\nb = 2", "x"));
        assert_eq!((t, r), (Tier::Unescaped, vec![(0, 11)]));
    }

    #[test]
    fn line_tiers_cover_normalisation_trailing_whitespace_and_typography() {
        let (t, r) = hits(find("x = 1\n\u{fb01}le = 2\ny = 3\n", "file = 2", "n"));
        assert_eq!(t, Tier::Nfkc);
        assert_eq!(
            &"x = 1\n\u{fb01}le = 2\ny = 3\n"[r[0].0..r[0].1],
            "\u{fb01}le = 2"
        );
        let file = "def f():\n    x = 1   \n    return x\n";
        let (t, r) = hits(find(file, "    x = 1\n    return x\n", "    return 1\n"));
        assert_eq!(t, Tier::TrailingWs);
        assert_eq!(
            apply(file, &r, "    return 1\n", false),
            "def f():\n    return 1\n"
        );
        let (t, _) = hits(find("s = \u{201c}hi\u{201d}\n", "s = \"hi\"", "n"));
        assert_eq!(t, Tier::Typographic);
    }

    #[test]
    fn the_already_applied_edit_and_the_nearest_region() {
        assert_eq!(
            find(
                "import x\n\ndef fetch_all(c):\n    pass\n",
                "fetchAll",
                "fetch_all"
            ),
            Found::AlreadyApplied {
                first_line: 3,
                last_line: 3,
                region: "def fetch_all(c):".into()
            }
        );
        // A multi-line new text: every line it touches, whole.
        match find("a\nb = 1\nc = 2\nd\n", "zzz", "= 1\nc =") {
            Found::AlreadyApplied {
                first_line,
                last_line,
                region,
            } => assert_eq!(
                (first_line, last_line, region.as_str()),
                (2, 3, "b = 1\nc = 2")
            ),
            other => panic!("{other:?}"),
        }
        let file = "import os\n\ndef f(x):\n    retrun x + 1\n\ndef g():\n    pass\n";
        match find(file, "    return x + 1", "    return x + 2") {
            Found::Miss {
                first_line,
                last_line,
                region,
            } => {
                assert_eq!((first_line, last_line), (4, 4));
                assert_eq!(region, "    retrun x + 1");
            }
            other => panic!("{other:?}"),
        }
        match find("", "x", "y") {
            Found::Miss { first_line, .. } => assert_eq!(first_line, 1),
            other => panic!("{other:?}"),
        }
    }
}
