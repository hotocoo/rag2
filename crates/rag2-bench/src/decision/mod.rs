//! `rag2-bench decision`: a real decision model behind RAG2's System One adapter (Liquid AI d1 on
//! llama.cpp), measured on real human-labelled data and real load. No mocks, no synthetic labels.
//!
//! Suites:
//! - `nli`: ANLI rounds 1-3 test sets (adversarially collected NLI, the hardest public NLI data).
//! - `xnli`: XNLI test set, every language d1 lists, human translations.
//! - `claims`: SciFact claim verification (expert-annotated SUPPORT / CONTRADICT / no evidence).
//! - `load`: concurrency sweep against the server with real SciFact abstracts as state, from one
//!   abstract to near the batch budget, one question and many questions per call.
//! - `relevance`: d1 as RAG2's relevance gate and reranker on SciFact retrieval candidates.
//! - `e2e`: full RAG2 queries with the d1 gate and contradiction check under concurrent load,
//!   while the corpus is re-ingested.

mod load;
mod quality;
mod rag;
mod stats;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use rag2_core::interfaces::DecisionProvider;
use rag2_decision::systemone::SystemOneHttp;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, ValueEnum, Serialize, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Suite {
    Nli,
    Xnli,
    Claims,
    Load,
    Relevance,
    E2e,
}

#[derive(Parser, Clone)]
pub struct DecisionArgs {
    /// Suites to run, comma separated.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "nli,xnli,claims,load,relevance,e2e"
    )]
    suites: Vec<Suite>,
    /// System One server (llama-server with a d1 GGUF).
    #[arg(long, default_value = "http://127.0.0.1:8081")]
    endpoint: String,
    /// Model id or alias the server must list.
    #[arg(long, default_value = "d1-2B")]
    serve_id: String,
    /// Requests in flight for the quality suites; match the server's `-np`.
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    #[arg(long, default_value_t = 300_000)]
    timeout_ms: u64,
    /// Directory holding `anli/`, `xnli/`, `scifact-claims/` and `scifact/` (BEIR).
    #[arg(long, default_value = "bench-data/raw")]
    data: PathBuf,
    /// Items per ANLI round; 0 means the whole test set.
    #[arg(long, default_value_t = 0)]
    anli_limit: usize,
    /// Items per XNLI language.
    #[arg(long, default_value_t = 200)]
    xnli_per_language: usize,
    /// Precision a gate threshold must reach to be recommended.
    #[arg(long, default_value_t = 0.9)]
    target_precision: f64,
    /// Load: concurrent clients to sweep.
    #[arg(long, value_delimiter = ',', default_value = "1,2,4,8,16,32,64")]
    load_concurrency: Vec<usize>,
    /// Load: SciFact abstracts concatenated into one state, per tier.
    #[arg(long, value_delimiter = ',', default_value = "1,8,24")]
    load_abstracts: Vec<usize>,
    /// Load: questions asked in one call, per tier.
    #[arg(long, value_delimiter = ',', default_value = "1,16")]
    load_questions: Vec<usize>,
    /// Load: requests per point (at least 4 per client).
    #[arg(long, default_value_t = 256)]
    load_requests: usize,
    /// Relevance/e2e: retrieval candidates judged per query.
    #[arg(long, default_value_t = 20)]
    depth: usize,
    /// Relevance/e2e: cutoff for nDCG, recall and MRR.
    #[arg(long, default_value_t = 10)]
    k: usize,
    /// Relevance/e2e: `hash` or a ModernBERT checkpoint directory.
    #[arg(long, default_value = "hash")]
    embedder: String,
    #[arg(long, default_value = "cpu")]
    device: String,
    /// Relevance/e2e: min_confidence of the d1 relevance gate and contradiction check.
    #[arg(long, default_value_t = 0.7)]
    gate_min_confidence: f32,
    /// E2E: concurrent query threads to sweep.
    #[arg(long, value_delimiter = ',', default_value = "1,4,16,32")]
    e2e_threads: Vec<usize>,
    /// E2E: SciFact test queries per point (the first N).
    #[arg(long, default_value_t = 100)]
    e2e_queries: usize,
    /// E2E: also re-ingest the corpus while querying.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    e2e_ingest: bool,
    /// Scratch directory for the engine (wiped first).
    #[arg(long, default_value = "target/bench-decision")]
    work: PathBuf,
}

/// Direct wire access for what the adapter does not expose: option order and token usage.
pub struct Raw {
    http: reqwest::blocking::Client,
    url: String,
}

impl Raw {
    fn new(endpoint: &str, timeout: Duration) -> Result<Self> {
        Ok(Raw {
            http: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()?,
            url: format!("{}/v1/systemone", endpoint.trim_end_matches('/')),
        })
    }

    /// POST a pre-serialized body; non-2xx is an error carrying the server's message.
    pub fn post(&self, body: String) -> Result<Value> {
        let r = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(body)
            .send()?;
        let status = r.status();
        let v: Value = r.json()?;
        if !status.is_success() {
            bail!(
                "{status}: {}",
                v["error"]["message"].as_str().unwrap_or("unknown")
            );
        }
        Ok(v)
    }
}

#[derive(Serialize)]
pub struct Report {
    model: Value,
    endpoint: String,
    concurrency: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<Vec<quality::GroupReport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    load: Option<Vec<load::LoadPoint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relevance: Option<Vec<rag::RelevanceReport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    e2e: Option<Vec<rag::E2ePoint>>,
}

pub fn run(a: DecisionArgs) -> Result<Report> {
    let timeout = Duration::from_millis(a.timeout_ms);
    let m = SystemOneHttp::new("d1", &a.endpoint, &a.serve_id, timeout, a.concurrency)?;
    if !m.healthy() {
        bail!(
            "{} does not serve {:?} (start llama-server with --alias {})",
            a.endpoint,
            a.serve_id,
            a.serve_id
        );
    }
    let raw = Raw::new(&a.endpoint, timeout)?;
    let model = reqwest::blocking::get(format!("{}/v1/models", a.endpoint.trim_end_matches('/')))?
        .json::<Value>()
        .context("reading /v1/models")?["data"][0]
        .clone();
    let has = |s| a.suites.contains(&s);

    let mut items = Vec::new();
    if has(Suite::Nli) {
        items.extend(quality::anli(&a.data.join("anli"), a.anli_limit)?);
    }
    if has(Suite::Xnli) {
        items.extend(quality::xnli(&a.data.join("xnli"), a.xnli_per_language)?);
    }
    if has(Suite::Claims) {
        items.extend(quality::scifact_claims(&a.data.join("scifact-claims"))?);
    }
    let quality = (!items.is_empty())
        .then(|| quality::run(&m, &raw, &items, a.concurrency, a.target_precision))
        .transpose()?;
    let load = has(Suite::Load)
        .then(|| {
            let abstracts = load::abstracts(&a.data.join("scifact-claims"))?;
            load::run(
                &raw,
                &abstracts,
                &a.load_abstracts,
                &a.load_questions,
                &a.load_concurrency,
                a.load_requests,
            )
        })
        .transpose()?;
    let (relevance, e2e) = if has(Suite::Relevance) || has(Suite::E2e) {
        rag::run(&a, &m, has(Suite::Relevance), has(Suite::E2e))?
    } else {
        (None, None)
    };
    Ok(Report {
        model,
        endpoint: a.endpoint,
        concurrency: a.concurrency,
        quality,
        load,
        relevance,
        e2e,
    })
}
