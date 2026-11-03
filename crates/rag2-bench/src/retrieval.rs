//! `rag2-bench retrieval`: ingest a BEIR-format dataset (`corpus.jsonl`, `queries.jsonl`,
//! `qrels/test.tsv`) through the embedded engine and measure quality (document-level recall@k,
//! nDCG@k, MRR@k), ingest throughput, single-thread latency percentiles and concurrent QPS for
//! each pipeline.

use anyhow::{Context, Result, bail};
use clap::Parser;
use hdrhistogram::Histogram;
use rag2_config::schema::{EmbedderConfig, Pipeline, Pooling, Profile, RetrieverConfig};
use rag2_config::{ConfigManager, FsConfigStore};
use rag2_core::model::{Document, Metadata, RetrieverKind};
use rag2_core::{DocumentId, TenantId};
use rag2_engine::{Engine, QueryRequest};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Parser, Clone)]
pub struct RetrievalArgs {
    /// BEIR dataset directory.
    #[arg(long)]
    pub(crate) data: PathBuf,
    /// Scratch directory for storage and indexes (wiped first).
    #[arg(long)]
    pub(crate) work: PathBuf,
    /// `hash` or a ModernBERT sentence-embedding checkpoint directory.
    #[arg(long, default_value = "hash")]
    pub(crate) embedder: String,
    /// Device for candle embedders: cpu, metal, cuda.
    #[arg(long, default_value = "cpu")]
    pub(crate) device: String,
    #[arg(long, default_value = "")]
    pub(crate) query_prefix: String,
    #[arg(long, default_value = "")]
    pub(crate) document_prefix: String,
    #[arg(long, default_value_t = 10)]
    pub(crate) k: usize,
    /// Documents per ingest call.
    #[arg(long, default_value_t = 256)]
    pub(crate) batch: usize,
    /// Threads for the concurrent-throughput phase.
    #[arg(long, default_value_t = 8)]
    pub(crate) threads: usize,
    /// Chunks retrieved per query before collapsing to documents (as a multiple of k).
    #[arg(long, default_value_t = 3)]
    pub(crate) chunk_factor: usize,
    #[arg(long, default_value = "lexical,dense,hybrid")]
    pub(crate) pipelines: String,
    /// Also measure the last pipeline while a second thread re-ingests the corpus.
    #[arg(long)]
    pub(crate) interference: bool,
    /// Also measure cold start and index-loss recovery after the run.
    #[arg(long)]
    pub(crate) recovery: bool,
}

#[derive(Serialize)]
pub struct Interference {
    pipeline: String,
    idle_p50_us: u64,
    idle_p99_us: u64,
    loaded_p50_us: u64,
    loaded_p99_us: u64,
    loaded_queries: usize,
    concurrent_ingest_docs_per_s: f64,
}

#[derive(Serialize)]
pub struct Recovery {
    /// Open storage and rebuild in-memory vector indexes from it (clean shutdown before).
    cold_start_s: f64,
    /// Same, after the lexical index directory was deleted (rebuilt from storage).
    lexical_loss_rebuild_s: f64,
    documents: u64,
    lexical_docs_after: u64,
    query_ok_after: bool,
}

#[derive(Serialize)]
pub struct PipelineReport {
    pipeline: String,
    queries: usize,
    recall_at_k: f64,
    ndcg_at_k: f64,
    mrr_at_k: f64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    p999_us: u64,
    single_thread_qps: f64,
    concurrent_qps: f64,
    calibration: Calibration,
}

/// Platt scaling fitted on even-indexed queries, evaluated on odd-indexed ones. The label is
/// "the top document is relevant". Paste `slope`/`intercept` into `pipelines.<p>.calibration`.
#[derive(Serialize)]
pub struct Calibration {
    slope: f64,
    intercept: f64,
    ece_raw_heldout: f64,
    ece_calibrated_heldout: f64,
    top1_accuracy: f64,
}

/// Equal-width bins for expected calibration error.
const ECE_BINS: usize = 10;
const FIT_STEPS: usize = 100;
const FIT_TOLERANCE: f64 = 1e-9;
/// Ridge strength pulling the fit toward the identity (slope 1, intercept 0).
const FIT_RIDGE: f64 = 1.0;
/// Largest parameter change per Newton step (damping against near-singular Hessians).
const FIT_MAX_STEP: f64 = 1.0;
const LOGIT_EPS: f64 = 1e-4;

pub(crate) fn logit(p: f64) -> f64 {
    let p = p.clamp(LOGIT_EPS, 1.0 - LOGIT_EPS);
    (p / (1.0 - p)).ln()
}

pub(crate) fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

pub(crate) fn ece(pairs: &[(f64, bool)]) -> f64 {
    let mut bins = vec![(0.0f64, 0.0f64, 0usize); ECE_BINS];
    for (p, y) in pairs {
        let b = ((p * ECE_BINS as f64) as usize).min(ECE_BINS - 1);
        bins[b].0 += p;
        bins[b].1 += f64::from(u8::from(*y));
        bins[b].2 += 1;
    }
    let n = pairs.len().max(1) as f64;
    bins.iter()
        .filter(|b| b.2 > 0)
        .map(|(ps, ys, c)| (*c as f64 / n) * ((ps - ys) / *c as f64).abs())
        .sum()
}

/// Logistic regression of the label on logit(raw) by Newton's method (two parameters), with a
/// small ridge term so separable data cannot push the weights to infinity.
pub(crate) fn fit(train: &[(f64, bool)]) -> (f64, f64) {
    let (mut a, mut b) = (1.0, 0.0);
    for _ in 0..FIT_STEPS {
        let (mut ga, mut gb, mut haa, mut hab, mut hbb) = (
            FIT_RIDGE * (a - 1.0),
            FIT_RIDGE * b,
            FIT_RIDGE,
            0.0,
            FIT_RIDGE,
        );
        for (p, y) in train {
            let x = logit(*p);
            let q = sigmoid(a * x + b);
            let err = q - f64::from(u8::from(*y));
            let w = q * (1.0 - q);
            ga += err * x;
            gb += err;
            haa += w * x * x;
            hab += w * x;
            hbb += w;
        }
        let det = haa * hbb - hab * hab;
        if det.abs() < f64::EPSILON {
            break;
        }
        let (da, db) = ((hbb * ga - hab * gb) / det, (haa * gb - hab * ga) / det);
        let norm = (da * da + db * db).sqrt();
        let (da, db) = if norm > FIT_MAX_STEP {
            (da * FIT_MAX_STEP / norm, db * FIT_MAX_STEP / norm)
        } else {
            (da, db)
        };
        a -= da;
        b -= db;
        if da.abs() + db.abs() < FIT_TOLERANCE {
            break;
        }
    }
    (a, b)
}

fn calibrate(samples: &[(f64, bool)]) -> Calibration {
    let train: Vec<_> = samples.iter().step_by(2).copied().collect();
    let test: Vec<_> = samples.iter().skip(1).step_by(2).copied().collect();
    let (slope, intercept) = fit(&train);
    let calibrated: Vec<_> = test
        .iter()
        .map(|(p, y)| (sigmoid(slope * logit(*p) + intercept), *y))
        .collect();
    Calibration {
        slope,
        intercept,
        ece_raw_heldout: ece(&test),
        ece_calibrated_heldout: ece(&calibrated),
        top1_accuracy: samples.iter().filter(|s| s.1).count() as f64 / samples.len().max(1) as f64,
    }
}

#[derive(Serialize)]
pub struct Report {
    dataset: String,
    embedder: String,
    documents: usize,
    chunks: u64,
    k: usize,
    ingest_s: f64,
    ingest_docs_per_s: f64,
    rss_after_ingest_mb: f64,
    pipelines: Vec<PipelineReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interference: Option<Interference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<Recovery>,
}

pub(crate) fn read_jsonl(p: &Path) -> Result<Vec<serde_json::Value>> {
    std::fs::read_to_string(p)
        .with_context(|| format!("reading {}", p.display()))?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).context("bad jsonl line"))
        .collect()
}

pub(crate) fn qrels(p: &Path) -> Result<HashMap<String, HashSet<String>>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    for line in std::fs::read_to_string(p)?.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() >= 3 && cols[2].trim().parse::<i32>().unwrap_or(0) > 0 {
            out.entry(cols[0].into())
                .or_default()
                .insert(cols[1].into());
        }
    }
    Ok(out)
}

pub(crate) fn profile(a: &RetrievalArgs) -> Profile {
    let mut p = Profile::minimal("bench");
    p.storage.data_dir = a.work.join("data").to_string_lossy().into_owned();
    p.indexes.lexical.commit_interval_ms = 0;
    p.limits.max_batch_documents = a.batch.max(1);
    p.limits.max_top_k = (a.k * a.chunk_factor).max(p.limits.max_top_k);
    p.cache.query_entries = 0;
    let emb = if a.embedder == "hash" {
        EmbedderConfig::Hash {
            dim: rag2_config::defaults::HASH_EMBEDDER_DIM,
        }
    } else {
        EmbedderConfig::Candle {
            model_dir: a.embedder.clone(),
            device: match a.device.as_str() {
                "metal" => rag2_config::schema::Device::Metal,
                "cuda" => rag2_config::schema::Device::Cuda,
                _ => rag2_config::schema::Device::Cpu,
            },
            pooling: Pooling::Mean,
            batch: rag2_config::defaults::EMBED_BATCH,
            query_prefix: a.query_prefix.clone(),
            document_prefix: a.document_prefix.clone(),
            max_tokens: rag2_config::defaults::EMBED_MAX_TOKENS,
        }
    };
    p.embedders = [("dense".to_string(), emb)].into();
    let base = p.pipelines["default"].clone();
    let r = |kind, embedder: Option<&str>| RetrieverConfig {
        kind,
        embedder: embedder.map(String::from),
        top_k: rag2_config::defaults::RETRIEVER_TOP_K,
        weight: rag2_config::defaults::RETRIEVER_WEIGHT,
        ef_search: None,
        enabled: true,
    };
    let mk = |retrievers: Vec<RetrieverConfig>| {
        let mut pl: Pipeline = base.clone();
        pl.retrievers = retrievers;
        pl.top_k = a.k * a.chunk_factor;
        pl.abstention.enabled = false;
        pl.contradiction.enabled = false;
        pl.dedup.enabled = false;
        pl
    };
    p.pipelines = [
        (
            "lexical".to_string(),
            mk(vec![r(RetrieverKind::Lexical, None)]),
        ),
        (
            "dense".to_string(),
            mk(vec![r(RetrieverKind::Dense, Some("dense"))]),
        ),
        (
            "hybrid".to_string(),
            mk(vec![
                r(RetrieverKind::Dense, Some("dense")),
                r(RetrieverKind::Lexical, None),
            ]),
        ),
    ]
    .into();
    p.default_pipeline = "hybrid".into();
    p
}

fn rss_mb() -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<f64>()
                .ok()
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

/// Ranked unique document ids from a response.
pub(crate) fn ranked_docs(r: &rag2_engine::QueryResponse, k: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    r.evidence
        .iter()
        .map(|e| e.provenance.doc_id.to_string())
        .filter(|d| seen.insert(d.clone()))
        .take(k)
        .collect()
}

pub(crate) fn score(ranked: &[String], rel: &HashSet<String>, k: usize) -> (f64, f64, f64) {
    let hits = ranked.iter().take(k).filter(|d| rel.contains(*d)).count();
    let recall = hits as f64 / rel.len().max(1) as f64;
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, d)| rel.contains(*d))
        .map(|(i, _)| 1.0 / (i as f64 + 2.0).log2())
        .sum();
    let idcg: f64 = (0..rel.len().min(k))
        .map(|i| 1.0 / (i as f64 + 2.0).log2())
        .sum();
    let mrr = ranked
        .iter()
        .take(k)
        .position(|d| rel.contains(d))
        .map_or(0.0, |i| 1.0 / (i as f64 + 1.0));
    (recall, if idcg > 0.0 { dcg / idcg } else { 0.0 }, mrr)
}

/// BEIR corpus rows as documents (title prepended to the text).
pub(crate) fn beir_documents(corpus: &[serde_json::Value]) -> Vec<Document> {
    corpus
        .iter()
        .filter_map(|d| {
            let id = DocumentId::new(d["_id"].as_str()?).ok()?;
            let title = d["title"].as_str().unwrap_or("").to_string();
            let body = d["text"].as_str().unwrap_or("");
            Some(Document {
                id,
                title: Some(title.clone()),
                text: format!("{title}\n\n{body}"),
                source: None,
                metadata: Metadata::new(),
            })
        })
        .collect()
}

pub fn run(a: RetrievalArgs) -> Result<Report> {
    if a.work.exists() {
        std::fs::remove_dir_all(&a.work).context("wiping work dir")?;
    }
    let corpus = read_jsonl(&a.data.join("corpus.jsonl"))?;
    let queries: HashMap<String, String> = read_jsonl(&a.data.join("queries.jsonl"))?
        .into_iter()
        .filter_map(|q| {
            Some((
                q["_id"].as_str()?.to_string(),
                q["text"].as_str()?.to_string(),
            ))
        })
        .collect();
    let rels = qrels(&a.data.join("qrels/test.tsv"))?;
    let eval: Vec<(String, &HashSet<String>)> = rels
        .iter()
        .filter_map(|(qid, r)| queries.get(qid).map(|t| (t.clone(), r)))
        .collect();
    if eval.is_empty() {
        bail!("no test queries with judgements");
    }
    let store = Arc::new(FsConfigStore::open(a.work.join("config"))?);
    let p = profile(&a);
    let cfg = Arc::new(ConfigManager::open(store, "bench", None, move || p, 10)?);
    let engine = Engine::open(cfg, vec![Arc::new(rag2_embed::EmbedFactory)])?;
    let t = TenantId::new("bench")?;

    let docs = beir_documents(&corpus);
    let n_docs = docs.len();
    eprintln!(
        "[bench] ingesting {n_docs} documents with embedder {}",
        a.embedder
    );
    let t0 = Instant::now();
    for group in docs.chunks(a.batch.max(1)) {
        engine.ingest(&t, group.to_vec())?;
    }
    engine.flush()?;
    let ingest_s = t0.elapsed().as_secs_f64();
    let stats = engine.stats(&t)?;
    let chunks = stats.vector_indexes.first().map_or(0, |v| v.vectors as u64);
    eprintln!(
        "[bench] ingested in {ingest_s:.1}s ({chunks} chunks, {:.0} docs/s)",
        n_docs as f64 / ingest_s
    );
    let rss = rss_mb();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(a.threads)
        .build()?;
    let mut reports = Vec::new();
    for name in a
        .pipelines
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let req = |q: &str| {
            let mut r = QueryRequest::new(q);
            r.pipeline = Some(name.to_string());
            r.include_context = Some(false);
            r.no_cache = true;
            r
        };
        for (q, _) in eval.iter().take(20) {
            engine.query(&t, req(q))?;
        }
        let mut hist = Histogram::<u64>::new(3)?;
        let (mut recall, mut ndcg, mut mrr) = (0.0, 0.0, 0.0);
        let mut samples: Vec<(f64, bool)> = Vec::with_capacity(eval.len());
        let started = Instant::now();
        for (q, rel) in &eval {
            let s = Instant::now();
            let r = engine.query(&t, req(q))?;
            hist.record(s.elapsed().as_micros() as u64)?;
            let ranked = ranked_docs(&r, a.k);
            samples.push((
                f64::from(r.confidence_detail.raw),
                ranked.first().is_some_and(|d| rel.contains(d)),
            ));
            let (rc, nd, mr) = score(&ranked, rel, a.k);
            recall += rc;
            ndcg += nd;
            mrr += mr;
        }
        let single = eval.len() as f64 / started.elapsed().as_secs_f64();
        let started = Instant::now();
        pool.install(|| {
            eval.par_iter()
                .try_for_each(|(q, _)| engine.query(&t, req(q)).map(drop))
        })?;
        let concurrent = eval.len() as f64 / started.elapsed().as_secs_f64();
        let n = eval.len() as f64;
        let rep = PipelineReport {
            pipeline: name.into(),
            queries: eval.len(),
            recall_at_k: recall / n,
            ndcg_at_k: ndcg / n,
            mrr_at_k: mrr / n,
            p50_us: hist.value_at_quantile(0.5),
            p95_us: hist.value_at_quantile(0.95),
            p99_us: hist.value_at_quantile(0.99),
            p999_us: hist.value_at_quantile(0.999),
            single_thread_qps: single,
            concurrent_qps: concurrent,
            calibration: calibrate(&samples),
        };
        eprintln!(
            "[bench] {name:<8} recall@{k}={:.4} nDCG@{k}={:.4} MRR@{k}={:.4} p50={}us p99={}us qps1={:.0} qps{}={:.0}",
            rep.recall_at_k,
            rep.ndcg_at_k,
            rep.mrr_at_k,
            rep.p50_us,
            rep.p99_us,
            rep.single_thread_qps,
            a.threads,
            rep.concurrent_qps,
            k = a.k
        );
        eprintln!(
            "[bench] {name:<8} calibration: slope={:.3} intercept={:.3} ECE raw={:.4} calibrated={:.4} (held-out) top1-acc={:.3}",
            rep.calibration.slope,
            rep.calibration.intercept,
            rep.calibration.ece_raw_heldout,
            rep.calibration.ece_calibrated_heldout,
            rep.calibration.top1_accuracy
        );
        reports.push(rep);
    }
    let interference = if a.interference {
        let name = a
            .pipelines
            .split(',')
            .map(str::trim)
            .rfind(|s| !s.is_empty())
            .unwrap_or("hybrid")
            .to_string();
        Some(measure_interference(
            &engine, &t, &name, &eval, &docs, a.batch,
        )?)
    } else {
        None
    };
    let recovery = if a.recovery {
        drop(engine);
        Some(measure_recovery(
            &a,
            &t,
            eval.first().map(|e| e.0.clone()).unwrap_or_default(),
        )?)
    } else {
        None
    };
    Ok(Report {
        interference,
        recovery,
        dataset: a
            .data
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        embedder: a.embedder.clone(),
        documents: n_docs,
        chunks,
        k: a.k,
        ingest_s,
        ingest_docs_per_s: n_docs as f64 / ingest_s,
        rss_after_ingest_mb: rss,
        pipelines: reports,
    })
}

pub(crate) fn percentiles(samples: &[u64]) -> Result<(u64, u64)> {
    let mut h = Histogram::<u64>::new(3)?;
    for s in samples {
        h.record(*s)?;
    }
    Ok((h.value_at_quantile(0.5), h.value_at_quantile(0.99)))
}

fn measure_interference(
    engine: &Arc<Engine>,
    t: &TenantId,
    pipeline: &str,
    eval: &[(String, &HashSet<String>)],
    docs: &[Document],
    batch: usize,
) -> Result<Interference> {
    let req = |q: &str| {
        let mut r = QueryRequest::new(q);
        r.pipeline = Some(pipeline.to_string());
        r.include_context = Some(false);
        r.no_cache = true;
        r
    };
    let mut idle = Vec::new();
    for (q, _) in eval {
        let s = Instant::now();
        engine.query(t, req(q))?;
        idle.push(s.elapsed().as_micros() as u64);
    }
    // Copies under new ids so every document is new work for the writer.
    let copies: Vec<Document> = docs
        .iter()
        .map(|d| Document {
            id: DocumentId::new(format!("{}#copy", d.id)).unwrap_or_else(|_| d.id.clone()),
            ..d.clone()
        })
        .collect();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (e2, t2, d2) = (engine.clone(), t.clone(), done.clone());
    let writer = std::thread::spawn(move || -> Result<f64> {
        let started = Instant::now();
        for g in copies.chunks(batch.max(1)) {
            e2.ingest(&t2, g.to_vec())?;
        }
        d2.store(true, std::sync::atomic::Ordering::Release);
        Ok(copies.len() as f64 / started.elapsed().as_secs_f64())
    });
    let mut loaded = Vec::new();
    'outer: loop {
        for (q, _) in eval {
            if done.load(std::sync::atomic::Ordering::Acquire) {
                break 'outer;
            }
            let s = Instant::now();
            engine.query(t, req(q))?;
            loaded.push(s.elapsed().as_micros() as u64);
        }
    }
    let rate = writer
        .join()
        .map_err(|_| anyhow::anyhow!("writer panicked"))??;
    let (ip50, ip99) = percentiles(&idle)?;
    let (lp50, lp99) = percentiles(&loaded)?;
    eprintln!(
        "[bench] interference {pipeline}: idle p50={ip50}us p99={ip99}us; during ingest p50={lp50}us p99={lp99}us over {} queries; ingest {rate:.0} docs/s",
        loaded.len()
    );
    Ok(Interference {
        pipeline: pipeline.into(),
        idle_p50_us: ip50,
        idle_p99_us: ip99,
        loaded_p50_us: lp50,
        loaded_p99_us: lp99,
        loaded_queries: loaded.len(),
        concurrent_ingest_docs_per_s: rate,
    })
}

fn open_engine(a: &RetrievalArgs) -> Result<Arc<Engine>> {
    let store = Arc::new(FsConfigStore::open(a.work.join("config"))?);
    let p = profile(a);
    let cfg = Arc::new(ConfigManager::open(store, "bench", None, move || p, 10)?);
    Ok(Engine::open(cfg, vec![Arc::new(rag2_embed::EmbedFactory)])?)
}

fn measure_recovery(a: &RetrievalArgs, t: &TenantId, probe: String) -> Result<Recovery> {
    let started = Instant::now();
    let e = open_engine(a)?;
    e.recover_all()?;
    let cold = started.elapsed().as_secs_f64();
    drop(e);
    std::fs::remove_dir_all(a.work.join("data/lexical")).context("removing lexical index")?;
    let started = Instant::now();
    let e = open_engine(a)?;
    e.recover_all()?;
    let rebuild = started.elapsed().as_secs_f64();
    let stats = e.stats(t)?;
    let ok = !e.query(t, QueryRequest::new(probe))?.evidence.is_empty();
    eprintln!(
        "[bench] recovery: cold start {cold:.2}s; after lexical loss {rebuild:.2}s; {} docs, {} lexical chunks, query ok={ok}",
        stats.documents, stats.lexical_docs
    );
    Ok(Recovery {
        cold_start_s: cold,
        lexical_loss_rebuild_s: rebuild,
        documents: stats.documents,
        lexical_docs_after: stats.lexical_docs,
        query_ok_after: ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platt_scaling_reduces_calibration_error_on_overconfident_scores() {
        // Scores of 0.9 that are right half the time, 0.6 right never: overconfident.
        // Each block puts the same mix on even (train) and odd (held-out) positions.
        let block = [
            (0.9, true),
            (0.9, true),
            (0.9, false),
            (0.9, false),
            (0.6, false),
            (0.6, false),
            (0.6, false),
            (0.6, false),
        ];
        let s: Vec<(f64, bool)> = block.iter().copied().cycle().take(800).collect();
        let c = calibrate(&s);
        assert!(
            c.ece_calibrated_heldout < c.ece_raw_heldout / 2.0,
            "{} vs {}",
            c.ece_calibrated_heldout,
            c.ece_raw_heldout
        );
    }
}
