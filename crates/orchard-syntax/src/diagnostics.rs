//! Diagnostics: the [`Diagnostic`] record, the rustc/elm-style caret
//! [`render`], and [`suggest`] (a faithful port of Python `difflib`'s
//! `get_close_matches(name, cands, n=1)` so "did you mean" hints match v2
//! byte-for-byte).

use crate::span::Span;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A single diagnostic. No numeric codes (matching v2): just severity, message,
/// an optional span, and an optional hint (often a [`suggest`] result).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Option<Span>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Option<Span>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            span,
            hint: String::new(),
        }
    }

    pub fn warning(message: impl Into<String>, span: Option<Span>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            message: message.into(),
            span,
            hint: String::new(),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Render this diagnostic against its source, reproducing v2's exact format:
    ///
    /// ```text
    /// error: <message>
    ///  ┌─ <file>:<line>:<col>
    ///  │
    /// 3│     <source line>
    ///  │     ^ <hint>
    /// ```
    ///
    /// With no span, only the `severity: message` header is returned.
    pub fn render(&self, source: &str) -> String {
        let header = format!("{}: {}", self.severity.as_str(), self.message);
        let span = match &self.span {
            Some(s) => s,
            None => return header,
        };

        // Python's str.splitlines() over LF/CRLF; we split on '\n' and trim a
        // trailing '\r'. 1-based line index.
        let lines: Vec<&str> = source.split('\n').collect();
        let src_line = if span.line >= 1 && (span.line as usize) <= lines.len() {
            lines[span.line as usize - 1].trim_end_matches('\r')
        } else {
            ""
        };
        // Tabs → single space so the caret column math aligns.
        let display_line: String = src_line.replace('\t', " ");
        let display_len = display_line.chars().count();

        let gutter = span.line.to_string();
        let pad = " ".repeat(gutter.len());

        // caret_col = clamp(col - 1, 0 ..= display_len)
        let caret_col = (span.col.saturating_sub(1) as usize).min(display_len);
        let mut caret = format!("{}^", " ".repeat(caret_col));
        if !self.hint.is_empty() {
            caret.push(' ');
            caret.push_str(&self.hint);
        }

        format!(
            "{header}\n{pad}┌─ {file}:{line}:{col}\n{pad}│\n{gutter}│ {display_line}\n{pad}│ {caret}",
            header = header,
            pad = pad,
            file = span.file,
            line = span.line,
            col = span.col,
            gutter = gutter,
            display_line = display_line,
            caret = caret,
        )
    }
}

/// "did you mean '<x>'?" for the closest candidate, or `""` if none clears the
/// 0.6 similarity cutoff. Faithful to `difflib.get_close_matches(name, cands,
/// n=1, cutoff=0.6)`: tie-break by `(ratio, candidate)` descending.
pub fn suggest<I, S>(name: &str, candidates: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let a: Vec<char> = name.chars().collect();
    let mut best: Option<(f64, String)> = None;
    for cand in candidates {
        let cand = cand.as_ref();
        let b: Vec<char> = cand.chars().collect();
        let r = ratio(&a, &b);
        if r >= 0.6 {
            let better = match &best {
                None => true,
                // nlargest compares (ratio, candidate): larger ratio wins, ties
                // broken by lexicographically larger candidate.
                Some((br, bc)) => r > *br || (r == *br && cand > bc.as_str()),
            };
            if better {
                best = Some((r, cand.to_string()));
            }
        }
    }
    match best {
        Some((_, c)) => format!("did you mean '{c}'?"),
        None => String::new(),
    }
}

/// `difflib.SequenceMatcher.ratio()` = 2*M/T, where M is the total size of the
/// matching blocks and T = len(a)+len(b). Autojunk never triggers for the short
/// identifiers we compare (it requires len >= 200), so no junk handling.
fn ratio(a: &[char], b: &[char]) -> f64 {
    let t = a.len() + b.len();
    if t == 0 {
        return 1.0;
    }
    let m = matching_block_total(a, b);
    2.0 * m as f64 / t as f64
}

/// Sum of the sizes of all matching blocks, via difflib's recursive
/// longest-match decomposition.
fn matching_block_total(a: &[char], b: &[char]) -> usize {
    // b2j: element -> sorted indices in b.
    use std::collections::HashMap;
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, &ch) in b.iter().enumerate() {
        b2j.entry(ch).or_default().push(j);
    }

    let mut total = 0usize;
    let mut queue: Vec<(usize, usize, usize, usize)> = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = find_longest_match(a, &b2j, alo, ahi, blo, bhi);
        if k > 0 {
            total += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    total
}

fn find_longest_match(
    a: &[char],
    b2j: &std::collections::HashMap<char, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    use std::collections::HashMap;
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, ch) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut newj2len: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(ch) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = j2len.get(&j.wrapping_sub(1)).copied().unwrap_or(0) + 1;
                newj2len.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = newj2len;
    }
    (besti, bestj, bestsize)
}
