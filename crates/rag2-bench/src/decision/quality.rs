//! Three-way entailment over real labelled pairs (ANLI, XNLI, SciFact claims), asked three ways:
//! the production adapter (`choice`), the same question with the options in reverse order (an
//! answer that flips with option order is confusion, not judgement), and RAG2's contradiction
//! gate (`noul`).

use super::Raw;
use super::stats::{Latency, Threshold, auroc, fan_out, latency, threshold_for_precision};
use crate::retrieval::{ece, read_jsonl};
use anyhow::{Context, Result};
use rag2_core::interfaces::{Answer, ChoiceOption, DecisionProvider, Question};
use rag2_laya::systemone::{SystemOneHttp, parse_answers};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

pub const LABELS: [&str; 3] = ["entailment", "neutral", "contradiction"];
const DESCRIPTIONS: [&str; 3] = [
    "The premise shows the hypothesis is true",
    "The premise neither proves nor disproves the hypothesis",
    "The premise shows the hypothesis is false",
];
const ENTAILMENT: usize = 0;
const NEUTRAL: usize = 1;
const CONTRADICTION: usize = 2;
const NLI_INSTRUCTIONS: &str = "How does the hypothesis relate to the premise?";
const CONTRADICTION_INSTRUCTIONS: &str = "Does the premise contradict the hypothesis?";
const ANLI_ROUNDS: [&str; 3] = ["R1", "R2", "R3"];
/// Languages d1-2B lists that XNLI covers.
const XNLI_LANGUAGES: [&str; 10] = ["en", "ar", "zh", "fr", "de", "es", "vi", "th", "hi", "ru"];
const ALL: &str = "all";

pub struct Item {
    pub group: String,
    pub premise: String,
    pub hypothesis: String,
    pub gold: usize,
}

fn state(it: &Item) -> String {
    format!("Premise: {}\nHypothesis: {}", it.premise, it.hypothesis)
}

pub fn anli(dir: &Path, limit: usize) -> Result<Vec<Item>> {
    let mut out = Vec::new();
    for r in ANLI_ROUNDS {
        let rows = read_jsonl(&dir.join(format!("anli_v1.0/{r}/test.jsonl")))?;
        let take = if limit == 0 { rows.len() } else { limit };
        for v in rows.into_iter().take(take) {
            let gold = match v["label"].as_str() {
                Some("e") => ENTAILMENT,
                Some("n") => NEUTRAL,
                Some("c") => CONTRADICTION,
                other => anyhow::bail!("ANLI {r}: unknown label {other:?}"),
            };
            out.push(Item {
                group: format!("anli-{}", r.to_lowercase()),
                premise: v["context"].as_str().unwrap_or_default().to_string(),
                hypothesis: v["hypothesis"].as_str().unwrap_or_default().to_string(),
                gold,
            });
        }
    }
    Ok(out)
}

pub fn xnli(dir: &Path, per_language: usize) -> Result<Vec<Item>> {
    let rows = read_jsonl(&dir.join("XNLI-1.0/xnli.test.jsonl"))?;
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for v in rows {
        let lang = v["language"].as_str().unwrap_or_default();
        if !XNLI_LANGUAGES.contains(&lang) {
            continue;
        }
        let n = seen.entry(lang.to_string()).or_default();
        if *n >= per_language {
            continue;
        }
        let gold = match v["gold_label"].as_str() {
            Some("entailment") => ENTAILMENT,
            Some("neutral") => NEUTRAL,
            Some("contradiction") => CONTRADICTION,
            _ => continue,
        };
        *n += 1;
        out.push(Item {
            group: format!("xnli-{lang}"),
            premise: v["sentence1"].as_str().unwrap_or_default().to_string(),
            hypothesis: v["sentence2"].as_str().unwrap_or_default().to_string(),
            gold,
        });
    }
    Ok(out)
}

/// SciFact dev claims against each cited abstract: SUPPORT is entailment, CONTRADICT is
/// contradiction, a cited abstract without rationale is neutral.
pub fn scifact_claims(dir: &Path) -> Result<Vec<Item>> {
    let corpus: HashMap<u64, String> = read_jsonl(&dir.join("data/corpus.jsonl"))?
        .into_iter()
        .filter_map(|d| {
            let id = d["doc_id"].as_u64()?;
            let sentences: Vec<&str> = d["abstract"]
                .as_array()?
                .iter()
                .filter_map(|s| s.as_str())
                .collect();
            Some((
                id,
                format!(
                    "{}\n{}",
                    d["title"].as_str().unwrap_or_default(),
                    sentences.join(" ")
                ),
            ))
        })
        .collect();
    let mut out = Vec::new();
    for c in read_jsonl(&dir.join("data/claims_dev.jsonl"))? {
        let claim = c["claim"].as_str().unwrap_or_default();
        for doc in c["cited_doc_ids"].as_array().into_iter().flatten() {
            let Some(id) = doc.as_u64() else { continue };
            let Some(text) = corpus.get(&id) else {
                continue;
            };
            let gold = match c["evidence"][id.to_string()][0]["label"].as_str() {
                Some("SUPPORT") => ENTAILMENT,
                Some("CONTRADICT") => CONTRADICTION,
                _ => NEUTRAL,
            };
            out.push(Item {
                group: "scifact-claims".into(),
                premise: text.clone(),
                hypothesis: claim.to_string(),
                gold,
            });
        }
    }
    Ok(out)
}

fn choice(order: &[usize]) -> Question {
    Question::Choice {
        instructions: NLI_INSTRUCTIONS.into(),
        options: order
            .iter()
            .map(|&i| ChoiceOption {
                label: LABELS[i].into(),
                description: DESCRIPTIONS[i].into(),
            })
            .collect(),
    }
}

/// The wire body with criteria in exactly `order` (the adapter's JSON map sorts keys).
fn ordered_body(state: &str, order: &[usize]) -> String {
    let criteria: Vec<String> = order
        .iter()
        .map(|&i| format!("{}:{}", json!(LABELS[i]), json!(DESCRIPTIONS[i])))
        .collect();
    format!(
        r#"{{"state":{},"questions":{{"q0":{{"type":"choice","instructions":{},"criteria":{{{}}}}}}}}}"#,
        json!(state),
        json!(NLI_INSTRUCTIONS),
        criteria.join(",")
    )
}

struct Rec {
    pred: Option<(usize, f64)>,
    reversed: Option<usize>,
    p_contradiction: Option<f64>,
    us: u64,
}

fn label_index(a: &Answer) -> Option<(usize, f64)> {
    match a {
        Answer::Choice { label, confidence } => LABELS
            .iter()
            .position(|l| l == label)
            .map(|i| (i, f64::from(*confidence))),
        Answer::YesNo { .. } => None,
    }
}

fn p_yes(a: &Answer) -> Option<f64> {
    match a {
        Answer::YesNo { yes, confidence } => Some(if *yes {
            f64::from(*confidence)
        } else {
            1.0 - f64::from(*confidence)
        }),
        Answer::Choice { .. } => None,
    }
}

#[derive(Serialize)]
pub struct GroupReport {
    group: String,
    n: usize,
    errors: usize,
    accuracy: f64,
    macro_f1: f64,
    /// Rows: gold label; columns: predicted label (order of `labels`).
    confusion: [[usize; 3]; 3],
    labels: [&'static str; 3],
    mean_confidence: f64,
    ece_top1: f64,
    /// Share of items whose answer is the same with the options reversed.
    order_agreement: f64,
    contradiction_auroc: Option<f64>,
    contradiction_threshold: Option<Threshold>,
    latency: Latency,
}

fn report(group: &str, items: &[&Item], recs: &[&Rec], target: f64) -> Result<GroupReport> {
    let mut confusion = [[0usize; 3]; 3];
    let mut conf_pairs = Vec::new();
    let mut agree = (0usize, 0usize);
    let mut contra = Vec::new();
    let mut errors = 0;
    for (it, r) in items.iter().zip(recs) {
        match r.pred {
            Some((p, c)) => {
                confusion[it.gold][p] += 1;
                conf_pairs.push((c, p == it.gold));
                if let Some(rv) = r.reversed {
                    agree.0 += usize::from(rv == p);
                    agree.1 += 1;
                }
            }
            None => errors += 1,
        }
        if let Some(p) = r.p_contradiction {
            contra.push((p, it.gold == CONTRADICTION));
        }
    }
    let answered = conf_pairs.len().max(1) as f64;
    let correct: usize = (0..3).map(|i| confusion[i][i]).sum();
    let f1s: Vec<f64> = (0..3)
        .filter_map(|c| {
            let tp = confusion[c][c] as f64;
            let fp: f64 = (0..3)
                .filter(|&g| g != c)
                .map(|g| confusion[g][c] as f64)
                .sum();
            let fneg: f64 = (0..3)
                .filter(|&p| p != c)
                .map(|p| confusion[c][p] as f64)
                .sum();
            (tp + fp + fneg > 0.0).then(|| 2.0 * tp / (2.0 * tp + fp + fneg))
        })
        .collect();
    Ok(GroupReport {
        group: group.to_string(),
        n: items.len(),
        errors,
        accuracy: correct as f64 / answered,
        macro_f1: f1s.iter().sum::<f64>() / f1s.len().max(1) as f64,
        confusion,
        labels: LABELS,
        mean_confidence: conf_pairs.iter().map(|p| p.0).sum::<f64>() / answered,
        ece_top1: ece(&conf_pairs),
        order_agreement: agree.0 as f64 / agree.1.max(1) as f64,
        contradiction_auroc: auroc(&contra),
        contradiction_threshold: threshold_for_precision(&contra, target),
        latency: latency(&recs.iter().map(|r| r.us).collect::<Vec<_>>())?,
    })
}

pub fn run(
    m: &SystemOneHttp,
    raw: &Raw,
    items: &[Item],
    workers: usize,
    target_precision: f64,
) -> Result<Vec<GroupReport>> {
    let forward = [choice(&[0, 1, 2])];
    let reverse_order = [2, 1, 0];
    let contra = [Question::YesNo {
        instructions: CONTRADICTION_INSTRUCTIONS.into(),
    }];
    eprintln!("[decision] quality: {} items", items.len());
    let started = Instant::now();
    let recs: Vec<Rec> = fan_out(workers, items, |it| {
        let s = state(it);
        let t = Instant::now();
        let pred = m.decide(&s, &forward).ok().and_then(|a| label_index(&a[0]));
        let us = t.elapsed().as_micros() as u64;
        let reversed = raw
            .post(ordered_body(&s, &reverse_order))
            .ok()
            .and_then(|v| parse_answers(&v, &forward).ok())
            .and_then(|a| label_index(&a[0]))
            .map(|(i, _)| i);
        let p_contradiction = m.decide(&s, &contra).ok().and_then(|a| p_yes(&a[0]));
        Rec {
            pred,
            reversed,
            p_contradiction,
            us,
        }
    });
    eprintln!(
        "[decision] quality: {} items x 3 calls in {:.1}s",
        items.len(),
        started.elapsed().as_secs_f64()
    );
    let mut groups: BTreeMap<&str, (Vec<&Item>, Vec<&Rec>)> = BTreeMap::new();
    for (it, r) in items.iter().zip(&recs) {
        let g = groups.entry(it.group.as_str()).or_default();
        g.0.push(it);
        g.1.push(r);
        let all = groups.entry(ALL).or_default();
        all.0.push(it);
        all.1.push(r);
    }
    groups
        .iter()
        .map(|(g, (its, rs))| report(g, its, rs, target_precision).context("group report"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_body_keeps_option_order_and_escapes() {
        let b = ordered_body("a \"quoted\" state", &[2, 1, 0]);
        let v: serde_json::Value = serde_json::from_str(&b).expect("valid json");
        assert_eq!(v["state"], "a \"quoted\" state");
        let c = b.find("contradiction").expect("c");
        let e = b.find("entailment").expect("e");
        assert!(c < e, "reverse order on the wire");
    }
}
