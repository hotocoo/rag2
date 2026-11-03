//! d1 inside RAG2 on SciFact (BEIR): as a relevance judge and reranker over real retrieval
//! candidates (hard negatives come from the retriever itself), and end to end with the relevance
//! gate and contradiction check under concurrent queries while the corpus is re-ingested.

use super::DecisionArgs;
use super::stats::{Latency, Threshold, auroc, fan_out, latency, threshold_for_precision};
use crate::retrieval::{RetrievalArgs, beir_documents, profile, qrels, read_jsonl, score};
use anyhow::{Context, Result, bail};
use rag2_config::schema::{DecisionGate, DecisionModelConfig, GateMode};
use rag2_config::{ConfigManager, FsConfigStore};
use rag2_core::interfaces::{Answer, DecisionProvider, Question};
use rag2_core::model::Document;
use rag2_core::{DocumentId, TenantId};
use rag2_decision::systemone::SystemOneHttp;
use rag2_engine::components::ComponentFactory;
use rag2_engine::query::{joint_relevance_state, relevance_state};
use rag2_engine::{Engine, QueryRequest};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const MODEL: &str = "d1";
const BASE: &str = "hybrid";
const GATED: &str = "gated";
const GATED_JOINT: &str = "gated_joint";
const GATE_INSTRUCTIONS: &str = "Does the passage contain information that answers the query?";
const CONTRADICTION_INSTRUCTIONS: &str =
    "Do the two passages make claims that contradict each other?";
const TENANT: &str = "bench";
const CONFIG_HISTORY: usize = 10;
/// Documents per ingest call for the corpus and the concurrent re-ingest.
const INGEST_BATCH: usize = 256;

#[derive(Serialize)]
pub struct RelevanceReport {
    mode: GateMode,
    queries: usize,
    pairs: usize,
    relevant_pairs: usize,
    errors: usize,
    calls: usize,
    wall_s: f64,
    auroc: Option<f64>,
    retrieval_ndcg: f64,
    reranked_ndcg: f64,
    retrieval_mrr: f64,
    reranked_mrr: f64,
    retrieval_recall: f64,
    /// At `gate_min_confidence`: share of relevant passages the gate keeps.
    gate_keeps_relevant: f64,
    /// At `gate_min_confidence`: share of non-relevant passages the gate removes.
    gate_removes_irrelevant: f64,
    gate_min_confidence: f32,
    threshold_for_target: Option<Threshold>,
    call_latency: Latency,
}

#[derive(Serialize)]
pub struct E2ePoint {
    pipeline: String,
    threads: usize,
    with_ingest: bool,
    queries: usize,
    errors: usize,
    qps: f64,
    abstained: usize,
    decision_warnings: usize,
    contradictions: usize,
    latency: Latency,
    #[serde(skip_serializing_if = "Option::is_none")]
    ingest_docs_per_s: Option<f64>,
}

fn engine(a: &DecisionArgs) -> Result<Arc<Engine>> {
    let ra = RetrievalArgs {
        data: a.data.join("scifact"),
        work: a.work.clone(),
        embedder: a.embedder.clone(),
        device: a.device.clone(),
        query_prefix: String::new(),
        document_prefix: String::new(),
        k: a.depth,
        batch: INGEST_BATCH,
        threads: 1,
        chunk_factor: 1,
        pipelines: String::new(),
        interference: false,
        recovery: false,
    };
    let mut p = profile(&ra);
    p.decision_models.insert(
        MODEL.into(),
        DecisionModelConfig::SystemOne {
            endpoint: a.endpoint.clone(),
            serve_id: a.serve_id.clone(),
            timeout_ms: a.timeout_ms,
            concurrency: a.concurrency,
        },
    );
    for (name, mode) in [(GATED, GateMode::PerItem), (GATED_JOINT, GateMode::Joint)] {
        let gate = |instructions: &str| DecisionGate {
            model: MODEL.into(),
            instructions: instructions.into(),
            min_confidence: a.gate_min_confidence,
            mode,
        };
        let mut gated = p.pipelines[BASE].clone();
        gated.top_k = a.k;
        gated.relevance_gate = Some(gate(GATE_INSTRUCTIONS));
        gated.contradiction.enabled = true;
        gated.contradiction.decision = Some(gate(CONTRADICTION_INSTRUCTIONS));
        gated.abstention.enabled = true;
        p.pipelines.insert(name.into(), gated);
    }
    let store = Arc::new(FsConfigStore::open(a.work.join("config"))?);
    let cfg = Arc::new(ConfigManager::open(
        store,
        "bench",
        None,
        move || p,
        CONFIG_HISTORY,
    )?);
    let factories: Vec<Arc<dyn ComponentFactory>> = vec![
        Arc::new(rag2_embed::EmbedFactory),
        Arc::new(rag2_decision::DecisionFactory),
    ];
    Engine::open(cfg, factories).map_err(Into::into)
}

fn request(q: &str, pipeline: &str, top_k: usize) -> QueryRequest {
    let mut r = QueryRequest::new(q);
    r.pipeline = Some(pipeline.into());
    r.top_k = Some(top_k);
    r.include_context = Some(false);
    r.no_cache = true;
    r
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

pub fn run(a: &DecisionArgs, m: &SystemOneHttp, relevance: bool, e2e: bool) -> Result<RagReports> {
    if a.work.exists() {
        std::fs::remove_dir_all(&a.work).context("wiping work dir")?;
    }
    let data = a.data.join("scifact");
    let corpus = read_jsonl(&data.join("corpus.jsonl"))?;
    let queries: HashMap<String, String> = read_jsonl(&data.join("queries.jsonl"))?
        .into_iter()
        .filter_map(|q| Some((q["_id"].as_str()?.into(), q["text"].as_str()?.into())))
        .collect();
    let rels = qrels(&data.join("qrels/test.tsv"))?;
    let eval: Vec<(String, HashSet<String>)> = rels
        .into_iter()
        .filter_map(|(qid, r)| queries.get(&qid).map(|t| (t.clone(), r)))
        .collect();
    if eval.is_empty() {
        bail!(
            "no SciFact test queries with judgements in {}",
            data.display()
        );
    }
    let engine = engine(a)?;
    let t = TenantId::new(TENANT)?;
    let docs = beir_documents(&corpus);
    eprintln!("[decision] ingesting {} SciFact documents", docs.len());
    for g in docs.chunks(INGEST_BATCH) {
        engine.ingest(&t, g.to_vec())?;
    }
    engine.flush()?;
    let rel = relevance
        .then(|| -> Result<Vec<RelevanceReport>> {
            let c = candidates(a, &engine, &t, &eval)?;
            [GateMode::PerItem, GateMode::Joint]
                .into_iter()
                .map(|mode| judge_mode(a, m, &eval, &c, mode))
                .collect()
        })
        .transpose()?;
    let points = e2e
        .then(|| load(a, &engine, &t, &eval, &docs))
        .transpose()?;
    Ok((rel, points))
}

/// Relevance reports (one per gate mode) and end-to-end points.
pub type RagReports = (Option<Vec<RelevanceReport>>, Option<Vec<E2ePoint>>);
/// One query's candidates: (document id, best passage, relevant).
type Candidate = (String, String, bool);

/// P(yes), and the gate's verdict when its confidence reaches `min_confidence`.
type Judged = (f64, Option<bool>);
/// Per query: unique documents in retrieval order, each with its best passage and relevance.
type Candidates = Vec<Vec<Candidate>>;

fn judged(a: &DecisionArgs, ans: &Answer) -> Option<Judged> {
    let py = p_yes(ans)?;
    let verdict = match ans {
        Answer::YesNo { yes, confidence } if *confidence >= a.gate_min_confidence => Some(*yes),
        _ => None,
    };
    Some((py, verdict))
}

fn candidates(
    a: &DecisionArgs,
    engine: &Engine,
    t: &TenantId,
    eval: &[(String, HashSet<String>)],
) -> Result<Candidates> {
    eval.iter()
        .map(|(q, rel)| {
            let r = engine.query(t, request(q, BASE, a.depth))?;
            let mut seen = HashSet::new();
            Ok(r.evidence
                .iter()
                .filter_map(|e| {
                    let d = e.provenance.doc_id.to_string();
                    seen.insert(d.clone()).then(|| {
                        let is_rel = rel.contains(&d);
                        (d, e.text.clone(), is_rel)
                    })
                })
                .collect())
        })
        .collect()
}

/// Judge every candidate with d1, per passage (one call each) or jointly (one call per query).
fn judge_mode(
    a: &DecisionArgs,
    m: &SystemOneHttp,
    eval: &[(String, HashSet<String>)],
    cands: &Candidates,
    mode: GateMode,
) -> Result<RelevanceReport> {
    let started = Instant::now();
    let (results, call_us): (Vec<Vec<Option<Judged>>>, Vec<u64>) = match mode {
        GateMode::PerItem => {
            let gate = [Question::YesNo {
                instructions: GATE_INSTRUCTIONS.into(),
            }];
            let flat: Vec<(usize, String)> = eval
                .iter()
                .zip(cands)
                .enumerate()
                .flat_map(|(qi, ((q, _), c))| {
                    c.iter()
                        .map(move |(_, text, _)| (qi, relevance_state(q, text)))
                })
                .collect();
            let out = fan_out(a.concurrency, &flat, |(_, state)| {
                let s = Instant::now();
                let j = m
                    .decide(state, &gate)
                    .ok()
                    .and_then(|ans| judged(a, &ans[0]));
                (s.elapsed().as_micros() as u64, j)
            });
            let mut per_q: Vec<Vec<Option<Judged>>> = vec![Vec::new(); eval.len()];
            for ((qi, _), (_, j)) in flat.iter().zip(&out) {
                per_q[*qi].push(*j);
            }
            (per_q, out.iter().map(|o| o.0).collect())
        }
        GateMode::Joint => {
            let work: Vec<(&String, &Vec<Candidate>)> =
                eval.iter().map(|(q, _)| q).zip(cands).collect();
            let out = fan_out(a.concurrency, &work, |(q, c)| {
                let state = joint_relevance_state(q, c.iter().map(|x| x.1.as_str()));
                let qs: Vec<Question> = (1..=c.len())
                    .map(|i| Question::YesNo {
                        instructions: format!("About passage [{i}]: {GATE_INSTRUCTIONS}"),
                    })
                    .collect();
                let s = Instant::now();
                let r = if qs.is_empty() {
                    Vec::new()
                } else {
                    match m.decide(&state, &qs) {
                        Ok(ans) => ans.iter().map(|x| judged(a, x)).collect(),
                        Err(_) => vec![None; c.len()],
                    }
                };
                (s.elapsed().as_micros() as u64, r)
            });
            let call_us = out.iter().map(|o| o.0).collect();
            (out.into_iter().map(|o| o.1).collect(), call_us)
        }
    };
    let wall_s = started.elapsed().as_secs_f64();
    let mut scored = Vec::new();
    let (mut keep_rel, mut n_rel, mut drop_irr, mut n_irr, mut errors) = (0, 0, 0, 0, 0);
    let mut sums = [0.0f64; 5];
    for ((c, js), (_, rel)) in cands.iter().zip(&results).zip(eval) {
        for ((_, _, is_rel), j) in c.iter().zip(js) {
            errors += usize::from(j.is_none());
            if let Some((py, _)) = j {
                scored.push((*py, *is_rel));
            }
            let verdict = j.and_then(|x| x.1);
            if *is_rel {
                n_rel += 1;
                keep_rel += usize::from(verdict != Some(false));
            } else {
                n_irr += 1;
                drop_irr += usize::from(verdict == Some(false));
            }
        }
        let base: Vec<String> = c.iter().map(|x| x.0.clone()).collect();
        let mut order: Vec<(String, f64)> = c
            .iter()
            .zip(js)
            .map(|(x, j)| (x.0.clone(), j.map_or(0.0, |v| v.0)))
            .collect();
        order.sort_by(|x, y| y.1.total_cmp(&x.1));
        let reranked: Vec<String> = order.into_iter().map(|x| x.0).collect();
        let (rb, nb, mb) = score(&base, rel, a.k);
        let (_, nr, mr) = score(&reranked, rel, a.k);
        for (s, v) in sums.iter_mut().zip([nb, nr, mb, mr, rb]) {
            *s += v;
        }
    }
    let nq = eval.len() as f64;
    let report = RelevanceReport {
        mode,
        queries: eval.len(),
        pairs: n_rel + n_irr,
        relevant_pairs: n_rel,
        errors,
        calls: call_us.len(),
        wall_s,
        auroc: auroc(&scored),
        retrieval_ndcg: sums[0] / nq,
        reranked_ndcg: sums[1] / nq,
        retrieval_mrr: sums[2] / nq,
        reranked_mrr: sums[3] / nq,
        retrieval_recall: sums[4] / nq,
        gate_keeps_relevant: keep_rel as f64 / n_rel.max(1) as f64,
        gate_removes_irrelevant: drop_irr as f64 / n_irr.max(1) as f64,
        gate_min_confidence: a.gate_min_confidence,
        threshold_for_target: threshold_for_precision(&scored, a.target_precision),
        call_latency: latency(&call_us)?,
    };
    eprintln!(
        "[decision] relevance {mode:?}: auroc={:?} ndcg {:.3} -> {:.3} in {wall_s:.0}s ({} calls)",
        report.auroc, report.retrieval_ndcg, report.reranked_ndcg, report.calls
    );
    Ok(report)
}

fn load(
    a: &DecisionArgs,
    engine: &Arc<Engine>,
    t: &TenantId,
    eval: &[(String, HashSet<String>)],
    docs: &[Document],
) -> Result<Vec<E2ePoint>> {
    let mut out = Vec::new();
    for &threads in &a.e2e_threads {
        for pipeline in [BASE, GATED, GATED_JOINT] {
            for ingest in [false, a.e2e_ingest].into_iter().collect::<HashSet<_>>() {
                out.push(point(a, engine, t, eval, docs, pipeline, threads, ingest)?);
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn point(
    a: &DecisionArgs,
    engine: &Arc<Engine>,
    t: &TenantId,
    eval: &[(String, HashSet<String>)],
    docs: &[Document],
    pipeline: &str,
    threads: usize,
    ingest: bool,
) -> Result<E2ePoint> {
    let stop = Arc::new(AtomicBool::new(false));
    let writer = ingest.then(|| {
        // Copies under new ids, re-written until the queries finish: real write load.
        let copies: Vec<Document> = docs
            .iter()
            .filter_map(|d| {
                Some(Document {
                    id: DocumentId::new(format!("{}#e2e{threads}{pipeline}", d.id)).ok()?,
                    ..d.clone()
                })
            })
            .collect();
        let (e, t, stop) = (engine.clone(), t.clone(), stop.clone());
        std::thread::spawn(move || -> Result<f64> {
            let s = Instant::now();
            let mut n = 0usize;
            for g in copies.chunks(INGEST_BATCH) {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                e.ingest(&t, g.to_vec())?;
                n += g.len();
            }
            Ok(n as f64 / s.elapsed().as_secs_f64())
        })
    });
    let started = Instant::now();
    let eval = &eval[..a.e2e_queries.min(eval.len())];
    let res = fan_out(threads, eval, |(q, _)| {
        let s = Instant::now();
        let r = engine.query(t, request(q, pipeline, a.k));
        (s.elapsed().as_micros() as u64, r)
    });
    let wall = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Release);
    let ingest_docs_per_s = writer
        .map(|w| w.join().map_err(|_| anyhow::anyhow!("writer panicked"))?)
        .transpose()?;
    let ok: Vec<_> = res
        .iter()
        .filter_map(|(us, r)| r.as_ref().ok().map(|r| (*us, r)))
        .collect();
    let p = E2ePoint {
        pipeline: pipeline.into(),
        threads,
        with_ingest: ingest,
        queries: res.len(),
        errors: res.len() - ok.len(),
        qps: ok.len() as f64 / wall,
        abstained: ok.iter().filter(|x| x.1.abstained.is_some()).count(),
        decision_warnings: ok
            .iter()
            .map(|x| x.1.warnings.iter().filter(|w| w.contains(MODEL)).count())
            .sum(),
        contradictions: ok.iter().map(|x| x.1.contradictions.len()).sum(),
        latency: latency(&ok.iter().map(|x| x.0).collect::<Vec<_>>())?,
        ingest_docs_per_s,
    };
    eprintln!(
        "[decision] e2e {pipeline} threads={threads} ingest={ingest}: p50={:.1}ms p99={:.1}ms {:.1} qps errors={} warnings={}",
        p.latency.p50_ms, p.latency.p99_ms, p.qps, p.errors, p.decision_warnings
    );
    Ok(p)
}
