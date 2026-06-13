//! Token → USD pricing. Longest-prefix model match. `mock`/`ollama` are free
//! (handled in the policy engine). Table refreshed to the current model set.

/// `(prefix, input $/Mtok, output $/Mtok)`.
const PRICES: &[(&str, f64, f64)] = &[
    ("claude-opus-4", 15.0, 75.0),
    ("claude-fable-5", 20.0, 100.0),
    ("claude-sonnet-4", 3.0, 15.0),
    ("claude-haiku-4", 1.0, 5.0),
    ("claude-3-5-haiku", 0.8, 4.0),
    ("claude-3-5-sonnet", 3.0, 15.0),
    ("gpt-5", 1.25, 10.0),
    ("gpt-4o-mini", 0.15, 0.6),
    ("gpt-4o", 2.5, 10.0),
    ("o3", 2.0, 8.0),
    ("text-embedding-3-small", 0.02, 0.0),
    ("text-embedding-3-large", 0.13, 0.0),
];

/// Look up `(input, output)` rates by longest matching prefix (lowercased,
/// also trying the tail after `/` for vendor-prefixed model ids).
pub fn lookup(model: &str) -> Option<(f64, f64)> {
    let lower = model.to_lowercase();
    let mut candidates = vec![lower.clone()];
    if let Some((_, tail)) = lower.split_once('/') {
        candidates.push(tail.to_string());
    }
    let mut best: Option<(usize, f64, f64)> = None;
    for cand in &candidates {
        for (prefix, pin, pout) in PRICES {
            if cand.starts_with(prefix) {
                let len = prefix.len();
                if best.map(|(l, _, _)| len > l).unwrap_or(true) {
                    best = Some((len, *pin, *pout));
                }
            }
        }
    }
    best.map(|(_, pin, pout)| (pin, pout))
}

/// Cost in USD, or `None` if the model has no price data.
pub fn cost_usd(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    override_rates: Option<(f64, f64)>,
) -> Option<f64> {
    let (pin, pout) = override_rates.or_else(|| lookup(model))?;
    Some((input_tokens as f64 * pin + output_tokens as f64 * pout) / 1_000_000.0)
}
