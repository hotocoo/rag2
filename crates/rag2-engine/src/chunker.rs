//! Boundary-aware chunking. Sizes count Unicode scalar values (chars), offsets are byte offsets.
//! A chunk ends at the last paragraph break that fits, else the last sentence end, else the last
//! whitespace, else a hard cut. Consecutive chunks overlap by about `overlap_chars`, starting at a
//! word boundary. A trailing fragment shorter than `min_chars` joins the previous chunk.

use rag2_config::schema::Chunking;

/// One chunk's byte span in the source text (trimmed of surrounding whitespace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

pub fn chunk(text: &str, c: &Chunking) -> Vec<Span> {
    // Byte offset of every char, plus the end, so char counts map to byte positions.
    let pos: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let n = pos.len() - 1;
    let max = c.max_chars.max(1);
    let overlap = c.overlap_chars.min(max.saturating_sub(1));
    let mut out = Vec::new();
    let mut s = skip_ws(text, &pos, 0);
    // End of the previous chunk: every chunk must end beyond it, or overlap could re-select the
    // same boundary and creep forward a word at a time.
    let mut prev_end = 0usize;
    while s < n {
        let mut e = (s + max).min(n);
        if n - e < c.min_chars {
            e = n;
        } else if e < n {
            let floor = c.min_chars.min(max / 2).max(prev_end.saturating_sub(s) + 1);
            let found = if s + floor < e {
                best_break(text, &pos, s, e, floor)
            } else {
                None
            };
            match found {
                Some(b) => e = b,
                // No boundary past the previous end: drop the overlap rather than cut a word.
                None if s < prev_end => {
                    s = skip_ws(text, &pos, prev_end);
                    continue;
                }
                None => {}
            }
        }
        prev_end = e;
        push_trimmed(text, pos[s], pos[e], &mut out);
        if e >= n {
            break;
        }
        // Next chunk starts `overlap` chars back, moved forward to the start of a word.
        // Overlap at most half of this chunk, so a short chunk is never repeated whole.
        let mut next = e.saturating_sub(overlap.min((e - s) / 2)).max(s + 1);
        while next < e
            && !text[pos[next - 1]..pos[next]]
                .chars()
                .all(char::is_whitespace)
        {
            next += 1;
        }
        s = skip_ws(text, &pos, next);
    }
    out
}

fn skip_ws(text: &str, pos: &[usize], mut i: usize) -> usize {
    while i + 1 < pos.len() && text[pos[i]..pos[i + 1]].chars().all(char::is_whitespace) {
        i += 1;
    }
    i
}

/// The best break position (char index) in `(s + floor, e]`, or `None` when there is no boundary
/// (the caller then hard-cuts at `e`).
fn best_break(text: &str, pos: &[usize], s: usize, e: usize, floor: usize) -> Option<usize> {
    let lo = s + floor.max(1);
    let window = &text[pos[lo.min(e)]..pos[e]];
    let base = pos[lo.min(e)];
    let to_char = |byte: usize| pos.partition_point(|&p| p < byte);
    if let Some(i) = window.rfind("\n\n") {
        return Some(to_char(base + i + 2));
    }
    let sentence = window.char_indices().rev().find(|&(i, ch)| {
        matches!(ch, '.' | '!' | '?' | '\n' | '。' | '！' | '？')
            && window[i + ch.len_utf8()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
    });
    if let Some((i, ch)) = sentence {
        return Some(to_char(base + i + ch.len_utf8()));
    }
    if let Some((i, ch)) = window
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
    {
        return Some(to_char(base + i + ch.len_utf8()));
    }
    None
}

fn push_trimmed(text: &str, a: usize, b: usize, out: &mut Vec<Span>) {
    let slice = &text[a..b];
    let lead = slice.len() - slice.trim_start().len();
    let trail = slice.len() - slice.trim_end().len();
    if lead + trail < slice.len() {
        out.push(Span {
            start: a + lead,
            end: b - trail,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn cfg(max: usize, overlap: usize, min: usize) -> Chunking {
        Chunking {
            max_chars: max,
            overlap_chars: overlap,
            min_chars: min,
        }
    }

    #[test]
    fn short_text_is_one_trimmed_chunk() {
        let t = "  hello world  ";
        let s = chunk(t, &cfg(100, 10, 5));
        assert_eq!(s.len(), 1);
        assert_eq!(&t[s[0].start..s[0].end], "hello world");
        assert!(chunk("   ", &cfg(100, 10, 5)).is_empty());
    }

    #[test]
    fn prefers_paragraph_then_sentence_boundaries() {
        let t = "First para line.\n\nSecond paragraph is here. It has two sentences.";
        let s = chunk(t, &cfg(30, 0, 5));
        assert_eq!(&t[s[0].start..s[0].end], "First para line.");
        let t2 = "One sentence here. Another sentence follows it closely.";
        let s2 = chunk(t2, &cfg(30, 0, 5));
        assert_eq!(&t2[s2[0].start..s2[0].end], "One sentence here.");
    }

    #[test]
    fn overlap_repeats_trailing_words() {
        let t = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        let s = chunk(t, &cfg(30, 12, 3));
        assert!(s.len() >= 2);
        assert!(s[1].start < s[0].end, "chunks should overlap");
    }

    #[test]
    fn a_short_paragraph_before_a_long_block_does_not_repeat_chunks() {
        let block = "word ".repeat(400);
        let t = format!("Intro line here.\n\n## Heading\n\n{block}");
        let spans = chunk(&t, &cfg(300, 100, 20));
        let ends: Vec<usize> = spans.iter().map(|s| s.end).collect();
        let mut dedup = ends.clone();
        dedup.dedup();
        assert_eq!(ends, dedup, "two chunks share an end");
        // ~2000 chars at 300 per chunk with 100 overlap: about 10 chunks, not dozens.
        assert!(spans.len() <= 12, "{} chunks", spans.len());
        // The short first chunk is not contained in the second.
        assert!(spans[1].start >= spans[0].start + (spans[0].end - spans[0].start) / 2);
    }

    #[test]
    fn multibyte_text_never_splits_a_char() {
        let t = "日本語のテキストです。これは二番目の文です。三番目の文もあります。";
        for s in chunk(t, &cfg(8, 2, 2)) {
            assert!(t.is_char_boundary(s.start) && t.is_char_boundary(s.end));
        }
    }

    proptest! {
        #[test]
        fn chunks_cover_the_text_in_order_within_bounds(
            words in prop::collection::vec("[a-z]{1,12}", 0..300),
            max in 20usize..200,
            overlap in 0usize..30,
        ) {
            let text = words.join(" ");
            let c = cfg(max, overlap.min(max - 1), 5);
            let spans = chunk(&text, &c);
            let mut last_start = 0;
            let mut last_end = 0;
            for s in &spans {
                prop_assert!(s.start < s.end && s.end <= text.len());
                prop_assert!(s.start >= last_start);
                prop_assert!(s.end > last_end || last_end == 0, "chunk ends must strictly advance");
                last_start = s.start;
                last_end = s.end;
                prop_assert!(text[s.start..s.end].chars().count() <= max + c.min_chars);
            }
            // Every word appears in some chunk.
            let covered: String = spans.iter().map(|s| &text[s.start..s.end]).collect::<Vec<_>>().join(" ");
            for w in &words {
                prop_assert!(covered.contains(w.as_str()));
            }
        }
    }
}
