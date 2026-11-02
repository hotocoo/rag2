//! Context assembly: the evidence rendered into one prompt-ready string with citation numbers,
//! within the configured character and chunk budgets.

use rag2_config::schema::ContextAssembly;
use rag2_core::model::Evidence;

pub fn assemble(evidence: &[Evidence], c: &ContextAssembly) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for (i, e) in evidence.iter().take(c.max_chunks).enumerate() {
        let p = &e.provenance;
        let block = c
            .template
            .replace("{n}", &(i + 1).to_string())
            .replace("{doc_id}", p.doc_id.as_str())
            .replace("{title}", p.title.as_deref().unwrap_or(""))
            .replace("{source}", p.source.as_deref().unwrap_or(""))
            .replace("{score}", &format!("{:.4}", e.score))
            // Last, so placeholder-like text inside the evidence is never expanded.
            .replace("{text}", &e.text);
        let sep = if out.is_empty() {
            0
        } else {
            c.separator.chars().count()
        };
        let len = block.chars().count();
        if used + sep + len > c.max_chars {
            break;
        }
        if !out.is_empty() {
            out.push_str(&c.separator);
        }
        out.push_str(&block);
        used += sep + len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rag2_core::model::Provenance;
    use rag2_core::{ChunkId, DocumentId};

    fn ev(doc: &str, text: &str) -> Evidence {
        Evidence {
            chunk_id: ChunkId(0),
            text: text.into(),
            score: 0.5,
            signals: Vec::new(),
            provenance: Provenance {
                doc_id: DocumentId::new(doc).expect("id"),
                doc_version: 1,
                chunk_ordinal: 0,
                start: 0,
                end: 0,
                source: None,
                title: None,
            },
        }
    }

    #[test]
    fn numbers_citations_and_respects_budgets() {
        let c = ContextAssembly {
            max_chars: 40,
            max_chunks: 5,
            template: "[{n}] ({doc_id}) {text}".into(),
            separator: "\n".into(),
        };
        let out = assemble(
            &[
                ev("a", "first fact"),
                ev("b", "second fact"),
                ev("c", "third fact that will not fit"),
            ],
            &c,
        );
        assert_eq!(out, "[1] (a) first fact\n[2] (b) second fact");
    }

    #[test]
    fn evidence_text_is_not_template_expanded() {
        let c = ContextAssembly {
            max_chars: 100,
            max_chunks: 5,
            template: "{text}".into(),
            separator: "".into(),
        };
        assert_eq!(
            assemble(&[ev("a", "literal {doc_id}")], &c),
            "literal {doc_id}"
        );
    }
}
