//! Keyword scorer parity — exact TF-IDF cosine values + chunking.

use orchard_runtime::{chunk_text, content_hash, KeywordScorer};

#[test]
fn tokenize_drops_stopwords_and_short() {
    let toks = KeywordScorer::tokenize("The quick brown fox, a 77!");
    assert_eq!(toks, vec!["quick", "brown", "fox", "77"]); // "the"/"a" stopped, 1-char dropped
}

#[test]
fn cosine_scores_match_formula() {
    let docs = vec![
        "the quick brown fox".to_string(),
        "lorem ipsum dolor".to_string(),
    ];
    let scores = KeywordScorer::score("fox", &docs);
    // doc1 has 3 distinct terms each idf-weighted equally → cosine = 1/sqrt(3).
    assert!(
        (scores[0] - (1.0_f64 / 3.0_f64.sqrt())).abs() < 1e-9,
        "got {}",
        scores[0]
    );
    assert_eq!(scores[1], 0.0);
}

#[test]
fn empty_query_and_docs() {
    assert_eq!(KeywordScorer::score("", &["a b c".to_string()]), vec![0.0]);
    assert!(KeywordScorer::score("x", &[]).is_empty());
}

#[test]
fn chunking_packs_paragraphs() {
    let text = "para one.\n\npara two.\n\npara three.";
    let chunks = chunk_text(text, 1200, 150);
    assert_eq!(chunks.len(), 1); // all fit in one chunk
    assert!(chunks[0].contains("para one") && chunks[0].contains("para three"));
}

#[test]
fn chunking_hard_splits_long_paragraphs() {
    let long = "x".repeat(3000);
    let chunks = chunk_text(&long, 1000, 100);
    assert!(
        chunks.len() >= 3,
        "expected multiple windows, got {}",
        chunks.len()
    );
    assert!(chunks.iter().all(|c| c.chars().count() <= 1000));
}

#[test]
fn content_hash_is_sha256_hex() {
    let h = content_hash("hello");
    assert_eq!(
        h,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
}
