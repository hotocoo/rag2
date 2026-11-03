//! Load: real SciFact abstracts as state, from one abstract to near the server's batch budget,
//! with one question or many per call, swept over concurrent clients. Every point reports the
//! full latency distribution, throughput, input tokens and errors (an overflow is a finding).

use super::Raw;
use super::stats::{Latency, fan_out, latency};
use crate::retrieval::read_jsonl;
use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::Instant;

/// Questions a triage step would ask about a scientific abstract, cycled to the requested count.
const NOUL_QUESTIONS: [&str; 6] = [
    "Does the text report results of a randomized controlled trial?",
    "Does the text involve human participants?",
    "Does the text report a statistically significant effect?",
    "Is the text about cancer?",
    "Does the text describe an experiment in mice?",
    "Does the text mention a specific gene or protein?",
];
const CHOICE_INSTRUCTIONS: &str = "Which field does the text belong to?";
const CHOICE_CRITERIA: [(&str, &str); 4] = [
    ("clinical", "Clinical medicine and trials"),
    ("molecular", "Molecular and cell biology"),
    ("public_health", "Epidemiology and public health"),
    ("other", "Any other field"),
];
/// Every this-many questions one is a `choice`; the rest are `noul`.
const CHOICE_EVERY: usize = 4;
/// Minimum requests each concurrent client sends per point.
const MIN_REQUESTS_PER_CLIENT: usize = 4;

pub fn abstracts(dir: &Path) -> Result<Vec<String>> {
    let out: Vec<String> = read_jsonl(&dir.join("data/corpus.jsonl"))?
        .into_iter()
        .filter_map(|d| {
            let s: Vec<&str> = d["abstract"]
                .as_array()?
                .iter()
                .filter_map(|x| x.as_str())
                .collect();
            Some(format!(
                "{}\n{}",
                d["title"].as_str().unwrap_or_default(),
                s.join(" ")
            ))
        })
        .collect();
    if out.is_empty() {
        bail!("no abstracts in {}", dir.display());
    }
    Ok(out)
}

fn questions(n: usize) -> Value {
    let qs: Map<String, Value> = (0..n)
        .map(|i| {
            let q = if i % CHOICE_EVERY == CHOICE_EVERY - 1 {
                let crit: Map<String, Value> = CHOICE_CRITERIA
                    .iter()
                    .map(|(k, d)| ((*k).to_string(), json!(d)))
                    .collect();
                json!({"type": "choice", "instructions": CHOICE_INSTRUCTIONS, "criteria": crit})
            } else {
                json!({"type": "noul", "instructions": NOUL_QUESTIONS[i % NOUL_QUESTIONS.len()]})
            };
            (format!("q{i}"), q)
        })
        .collect();
    Value::Object(qs)
}

#[derive(Serialize)]
pub struct LoadPoint {
    abstracts_per_state: usize,
    questions_per_call: usize,
    concurrency: usize,
    requests: usize,
    errors: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_error: Option<String>,
    mean_input_tokens: f64,
    calls_per_s: f64,
    decisions_per_s: f64,
    latency: Latency,
}

pub fn run(
    raw: &Raw,
    abstracts: &[String],
    sizes: &[usize],
    question_counts: &[usize],
    concurrency: &[usize],
    requests: usize,
) -> Result<Vec<LoadPoint>> {
    let mut points = Vec::new();
    for &size in sizes {
        for &nq in question_counts {
            let qs = questions(nq);
            for &c in concurrency {
                let n = requests.max(c * MIN_REQUESTS_PER_CLIENT);
                // A different window of abstracts per request: no two states are the same.
                let bodies: Vec<String> = (0..n)
                    .map(|r| {
                        let state: Vec<&str> = (0..size)
                            .map(|j| abstracts[(r * size + j) % abstracts.len()].as_str())
                            .collect();
                        json!({"state": state.join("\n\n"), "questions": qs}).to_string()
                    })
                    .collect();
                // Warm-up outside the measurement.
                let _ = raw.post(bodies[0].clone());
                let started = Instant::now();
                let res: Vec<(u64, Result<u64>)> = fan_out(c, &bodies, |b| {
                    let t = Instant::now();
                    let r = raw
                        .post(b.clone())
                        .map(|v| v["usage"]["input_tokens"].as_u64().unwrap_or_default());
                    (t.elapsed().as_micros() as u64, r)
                });
                let wall = started.elapsed().as_secs_f64();
                let ok: Vec<(u64, u64)> = res
                    .iter()
                    .filter_map(|(us, r)| r.as_ref().ok().map(|tok| (*us, *tok)))
                    .collect();
                let first_error = res
                    .iter()
                    .find_map(|(_, r)| r.as_ref().err().map(|e| e.to_string()));
                let p = LoadPoint {
                    abstracts_per_state: size,
                    questions_per_call: nq,
                    concurrency: c,
                    requests: n,
                    errors: n - ok.len(),
                    first_error,
                    mean_input_tokens: ok.iter().map(|x| x.1 as f64).sum::<f64>()
                        / ok.len().max(1) as f64,
                    calls_per_s: ok.len() as f64 / wall,
                    decisions_per_s: (ok.len() * nq) as f64 / wall,
                    latency: latency(&ok.iter().map(|x| x.0).collect::<Vec<_>>())?,
                };
                eprintln!(
                    "[decision] load abstracts={size} questions={nq} c={c}: p50={:.1}ms p99={:.1}ms p99.9={:.1}ms {:.1} calls/s errors={} tokens={:.0}",
                    p.latency.p50_ms,
                    p.latency.p99_ms,
                    p.latency.p999_ms,
                    p.calls_per_s,
                    p.errors,
                    p.mean_input_tokens
                );
                points.push(p);
            }
        }
    }
    Ok(points)
}
