//! Small text utilities shared by routing, evidence analysis and the hash embedder. Deliberately
//! language-light: lowercase alphanumeric words, an English stopword list for salience, and
//! negation cues for contradiction screening. Operators who need other languages rely on the
//! decision-model checks, which are configured, not coded.

use std::collections::HashSet;
use std::sync::LazyLock;

static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "he", "in",
        "is", "it", "its", "of", "on", "or", "that", "the", "this", "to", "was", "were", "will",
        "with", "what", "which", "who", "how", "when", "where", "why", "do", "does", "did", "can",
        "could", "should", "would", "i", "you", "we", "they", "them", "their", "our", "your", "me",
        "my", "about", "into", "than", "then", "there", "these", "those", "so", "if", "but",
        "also", "been", "being", "such", "any", "all", "some",
    ]
    .into_iter()
    .collect()
});

/// Words that flip a claim. Contractions are matched on their `n't` suffix.
const NEGATIONS: &[&str] = &[
    "not",
    "no",
    "never",
    "none",
    "neither",
    "nor",
    "cannot",
    "without",
    "false",
    "incorrect",
    "deny",
    "denies",
];

/// Lowercase alphanumeric words.
pub fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
}

pub fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(w)
}

/// Distinct non-stopword words.
pub fn content_words(text: &str) -> HashSet<String> {
    words(text)
        .filter(|w| !is_stopword(w) && !is_negation(w))
        .collect()
}

pub fn is_negation(w: &str) -> bool {
    NEGATIONS.contains(&w) || w.ends_with("n't")
}

pub fn has_negation(text: &str) -> bool {
    words(text).any(|w| is_negation(&w))
}

pub fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    inter as f32 / (a.len() + b.len() - inter) as f32
}

/// Numbers in `text` with the word that follows each (its unit or subject), e.g. ("12", "ms").
pub fn numbers_with_context(text: &str) -> Vec<(f64, String)> {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let cleaned: String = t
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        let cleaned = cleaned.trim_end_matches('.');
        if cleaned.is_empty() || !cleaned.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(v) = cleaned.parse::<f64>() {
            let unit: String = t
                .chars()
                .filter(|c| c.is_alphabetic() || *c == '%')
                .collect::<String>()
                .to_lowercase();
            let ctx = if unit.is_empty() {
                toks.get(i + 1)
                    .map(|n| {
                        n.trim_matches(|c: char| !c.is_alphanumeric())
                            .to_lowercase()
                    })
                    .unwrap_or_default()
            } else {
                unit
            };
            out.push((v, ctx));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_words_drop_stopwords_and_negations() {
        let w = content_words("The pump is NOT failing at the plant");
        assert!(w.contains("pump") && w.contains("failing") && w.contains("plant"));
        assert!(!w.contains("the") && !w.contains("not"));
    }

    #[test]
    fn negation_cues_include_contractions() {
        assert!(has_negation("it doesn't work"));
        assert!(has_negation("there is no backup"));
        assert!(!has_negation("it works"));
    }

    #[test]
    fn numbers_carry_their_unit_or_next_word() {
        let n = numbers_with_context("latency is 12ms and 3 retries, 99.9% uptime");
        assert!(n.contains(&(12.0, "ms".into())));
        assert!(n.contains(&(3.0, "retries".into())));
        assert!(n.contains(&(99.9, "%".into())));
    }
}
