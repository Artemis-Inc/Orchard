//! Chunking, content hashing, the pure-Rust keyword scorer (the offline
//! `provider: none` semantic path), and pluggable real embedders. Ports v2's
//! `embeddings.py` — the keyword scorer is reproduced bit-for-bit (tokenizer,
//! stopwords, `idf = ln((N+1)/(df+1)) + 1`, raw-count TF, cosine).

use crate::traits::{Hit, Store};
use std::collections::{HashMap, HashSet};

/// SHA-256 hex digest (incremental-indexing identity).
pub fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// L2-normalize a vector (a zero vector is returned unchanged).
pub fn normalize(vec: &[f32]) -> Vec<f32> {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        vec.to_vec()
    } else {
        vec.iter().map(|x| x / norm).collect()
    }
}

/// Paragraph-aware chunking (`size` chars, `overlap` carry-over).
pub fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<String> {
    if size == 0 || text.trim().is_empty() {
        return vec![];
    }
    let overlap = overlap.min(size - 1);
    // split on blank lines
    let paragraphs: Vec<String> = split_paragraphs(text);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for para in &paragraphs {
        if char_len(para) > size {
            if !current.trim().is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            current.clear();
            let step = (size - overlap).max(1);
            let chars: Vec<char> = para.chars().collect();
            let mut start = 0;
            while start < chars.len() {
                let end = (start + size).min(chars.len());
                let piece: String = chars[start..end].iter().collect();
                let piece = piece.trim().to_string();
                if !piece.is_empty() {
                    chunks.push(piece);
                }
                if start + size >= chars.len() {
                    break;
                }
                start += step;
            }
            continue;
        }
        if current.is_empty() {
            current = para.clone();
        } else if char_len(&current) + 2 + char_len(para) <= size {
            current = format!("{current}\n\n{para}");
        } else {
            chunks.push(current.clone());
            let tail: String = if overlap > 0 {
                let cs: Vec<char> = current.chars().collect();
                let start = cs.len().saturating_sub(overlap);
                cs[start..].iter().collect::<String>().trim().to_string()
            } else {
                String::new()
            };
            if !tail.is_empty() && char_len(&tail) + 2 + char_len(para) <= size {
                current = format!("{tail}\n\n{para}");
            } else {
                current = para.clone();
            }
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
        .into_iter()
        .filter(|c| !c.trim().is_empty())
        .collect()
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn split_paragraphs(text: &str) -> Vec<String> {
    // Split on a newline followed by optional whitespace then a newline.
    let mut out = Vec::new();
    let mut buf = String::new();
    let lines: Vec<&str> = text.split('\n').collect();
    for line in lines {
        if line.trim().is_empty() {
            if !buf.trim().is_empty() {
                out.push(buf.trim().to_string());
            }
            buf.clear();
        } else {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(line);
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

/// The offline TF-IDF cosine scorer.
pub struct KeywordScorer;

const STOPWORDS: &[&str] = &[
    "a", "about", "after", "all", "also", "an", "and", "any", "are", "as", "at", "be", "because",
    "been", "but", "by", "can", "could", "did", "do", "does", "for", "from", "had", "has", "have",
    "he", "her", "here", "his", "how", "i", "if", "in", "into", "is", "it", "its", "just", "me",
    "more", "most", "my", "no", "not", "of", "on", "or", "other", "our", "out", "she", "so",
    "some", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this",
    "those", "to", "too", "up", "was", "we", "were", "what", "when", "where", "which", "who",
    "why", "will", "with", "would", "you", "your",
];

impl KeywordScorer {
    /// Lowercase `[a-z0-9]+` tokens, minus stopwords and 1-char tokens.
    pub fn tokenize(text: &str) -> Vec<String> {
        let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();
        let lower = text.to_lowercase();
        let mut out = Vec::new();
        let mut tok = String::new();
        for c in lower.chars() {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                tok.push(c);
            } else if !tok.is_empty() {
                if tok.len() > 1 && !stop.contains(tok.as_str()) {
                    out.push(std::mem::take(&mut tok));
                } else {
                    tok.clear();
                }
            }
        }
        if tok.len() > 1 && !stop.contains(tok.as_str()) {
            out.push(tok);
        }
        out
    }

    /// Cosine similarity of `query` against each doc (`idf = ln((N+1)/(df+1))+1`).
    pub fn score(query: &str, docs: &[String]) -> Vec<f64> {
        if docs.is_empty() {
            return vec![];
        }
        let query_tokens = Self::tokenize(query);
        if query_tokens.is_empty() {
            return vec![0.0; docs.len()];
        }
        let doc_tokens: Vec<Vec<String>> = docs.iter().map(|d| Self::tokenize(d)).collect();
        let n = docs.len() as f64;
        let mut df: HashMap<String, f64> = HashMap::new();
        for tokens in &doc_tokens {
            for term in tokens.iter().collect::<HashSet<_>>() {
                *df.entry(term.clone()).or_insert(0.0) += 1.0;
            }
        }
        let idf = |term: &str| -> f64 {
            ((n + 1.0) / (df.get(term).copied().unwrap_or(0.0) + 1.0)).ln() + 1.0
        };

        let query_vec = weight_vec(&query_tokens, &idf);
        let query_norm: f64 = query_vec.values().map(|w| w * w).sum::<f64>().sqrt();

        doc_tokens
            .iter()
            .map(|tokens| {
                if tokens.is_empty() {
                    return 0.0;
                }
                let doc_vec = weight_vec(tokens, &idf);
                let doc_norm: f64 = doc_vec.values().map(|w| w * w).sum::<f64>().sqrt();
                if query_norm == 0.0 || doc_norm == 0.0 {
                    return 0.0;
                }
                let dot: f64 = query_vec
                    .iter()
                    .map(|(t, w)| w * doc_vec.get(t).copied().unwrap_or(0.0))
                    .sum();
                dot / (query_norm * doc_norm)
            })
            .collect()
    }
}

fn weight_vec(tokens: &[String], idf: &dyn Fn(&str) -> f64) -> HashMap<String, f64> {
    let mut counts: HashMap<String, f64> = HashMap::new();
    for t in tokens {
        *counts.entry(t.clone()).or_insert(0.0) += 1.0;
    }
    counts
        .into_iter()
        .map(|(t, c)| {
            let w = c * idf(&t);
            (t, w)
        })
        .collect()
}

/// Retrieve the `top_k` most relevant chunks as `(text, score, source)`. With an
/// embedder, query → vector → store dot-product scan; without, keyword scoring
/// over all chunks (positive scores only).
pub async fn semantic_search(
    store: &dyn Store,
    embedder: Option<&std::sync::Arc<dyn crate::traits::Embedder>>,
    query: &str,
    top_k: i64,
) -> Vec<Hit> {
    if top_k <= 0 {
        return vec![];
    }
    if let Some(e) = embedder {
        match e.embed(&[query.to_string()]).await {
            Ok(vs) if !vs.is_empty() => return store.search_vec(&vs[0], top_k),
            _ => return vec![],
        }
    }
    let chunks = store.all_chunks();
    if chunks.is_empty() {
        return vec![];
    }
    let texts: Vec<String> = chunks.iter().map(|(_, t, _)| t.clone()).collect();
    let scores = KeywordScorer::score(query, &texts);
    let mut ranked: Vec<Hit> = chunks
        .into_iter()
        .zip(scores)
        .filter(|(_, s)| *s > 0.0)
        .map(|((_, text, source), s)| (text, s, source))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(top_k as usize);
    ranked
}
