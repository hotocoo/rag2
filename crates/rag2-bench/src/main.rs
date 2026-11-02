//! `rag2-bench vector --index flat|hnsw|embedvec ...`: one index per process so resident memory is
//! attributable. Prints one JSON result line on stdout; `scripts/bench-vector.sh` runs the matrix
//! and `docs/benchmarks/vector-index.md` records the numbers.

mod data;
mod retrieval;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use hdrhistogram::Histogram;
use rag2_core::interfaces::VectorIndex;
use rag2_vector::{FlatConfig, FlatIndex, HnswConfig, HnswIndex};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashSet;
use std::time::Instant;

#[derive(Parser)]
#[command(about = "RAG2 benchmarks")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build one vector index over synthetic clustered vectors and measure it.
    Vector(VectorArgs),
    /// Retrieval quality and latency over a BEIR-format dataset, through the embedded engine.
    Retrieval(retrieval::RetrievalArgs),
}

#[derive(Clone, Copy, ValueEnum, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum IndexKind {
    Flat,
    Hnsw,
    Embedvec,
}

#[derive(Parser, Clone)]
struct VectorArgs {
    #[arg(long, value_enum)]
    index: IndexKind,
    #[arg(long, default_value_t = 100_000)]
    n: usize,
    #[arg(long, default_value_t = 768)]
    dim: usize,
    #[arg(long, default_value_t = 1_000)]
    queries: usize,
    #[arg(long, default_value_t = 10)]
    k: usize,
    /// Gaussian clusters in the synthetic corpus (embeddings are clustered, not uniform).
    #[arg(long, default_value_t = 256)]
    clusters: usize,
    /// Per-coordinate noise around a cluster centre.
    #[arg(long, default_value_t = 0.35)]
    noise: f32,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    #[arg(long, default_value_t = 16)]
    m: usize,
    #[arg(long, default_value_t = 200)]
    ef_construction: usize,
    /// Comma-separated search breadths to sweep.
    #[arg(long, default_value = "32,64,128,256")]
    ef_search: String,
    /// Threads for the concurrent-throughput phase.
    #[arg(long, default_value_t = 8)]
    threads: usize,
    /// Rows per parallel task in the flat scan.
    #[arg(long, default_value_t = 8_192)]
    flat_chunk_rows: usize,
}

#[derive(Serialize)]
struct SearchPoint {
    ef: usize,
    recall_at_k: f64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    p999_us: u64,
    max_us: u64,
    single_thread_qps: f64,
    concurrent_qps: f64,
}

#[derive(Serialize)]
struct Report {
    index: IndexKind,
    n: usize,
    dim: usize,
    k: usize,
    queries: usize,
    clusters: usize,
    m: usize,
    ef_construction: usize,
    threads: usize,
    build_s: f64,
    build_vectors_per_s: f64,
    rss_before_mb: f64,
    rss_after_build_mb: f64,
    points: Vec<SearchPoint>,
}

/// Current resident set of this process in MiB, from `ps` (portable across macOS and Linux).
fn rss_mb() -> Result<f64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .context("running ps")?;
    let kb: f64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .context("parsing ps rss")?;
    Ok(kb / 1024.0)
}

trait Searcher: Sync {
    fn query(&self, q: &[f32], k: usize, ef: usize) -> Result<Vec<u64>>;
}

struct Ours(Box<dyn VectorIndex>);

impl Searcher for Ours {
    fn query(&self, q: &[f32], k: usize, ef: usize) -> Result<Vec<u64>> {
        Ok(self
            .0
            .search(q, k, Some(ef))?
            .into_iter()
            .map(|h| h.chunk.0)
            .collect())
    }
}

struct EmbedVec(embedvec::EmbedVec);

impl Searcher for EmbedVec {
    fn query(&self, q: &[f32], k: usize, ef: usize) -> Result<Vec<u64>> {
        let hits = self
            .0
            .search_internal(q, k, ef, None)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(hits.into_iter().map(|h| h.id as u64).collect())
    }
}

fn build(a: &VectorArgs, corpus: &[(u64, Vec<f32>)]) -> Result<Box<dyn Searcher>> {
    Ok(match a.index {
        IndexKind::Flat => {
            let ix = FlatIndex::new(
                a.dim,
                FlatConfig {
                    parallel_chunk_rows: a.flat_chunk_rows,
                },
            )?;
            for (id, v) in corpus {
                ix.upsert(*id, v)?;
            }
            Box::new(Ours(Box::new(ix)))
        }
        IndexKind::Hnsw => {
            let ix = HnswIndex::new(
                a.dim,
                HnswConfig {
                    m: a.m,
                    ef_construction: a.ef_construction,
                    ef_search: 64,
                    capacity_hint: a.n,
                    max_tombstone_ratio: 0.2,
                },
            )?;
            ix.insert_batch(corpus)?;
            Box::new(Ours(Box::new(ix)))
        }
        IndexKind::Embedvec => {
            let mut ev = embedvec::EmbedVec::builder()
                .dimension(a.dim)
                .metric(embedvec::Distance::Cosine)
                .m(a.m)
                .ef_construction(a.ef_construction)
                .build()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            for (id, v) in corpus {
                let got = ev
                    .add_internal(v, serde_json::Value::Null)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                if got as u64 != *id {
                    bail!("embedvec assigned id {got} to vector {id}");
                }
            }
            Box::new(EmbedVec(ev))
        }
    })
}

fn recall(found: &[Vec<u64>], truth: &[Vec<u64>]) -> f64 {
    let hit: usize = found
        .iter()
        .zip(truth)
        .map(|(f, t)| {
            let t: HashSet<_> = t.iter().collect();
            f.iter().filter(|id| t.contains(id)).count()
        })
        .sum();
    let total: usize = truth.iter().map(Vec::len).sum();
    hit as f64 / total.max(1) as f64
}

fn vector(a: VectorArgs) -> Result<Report> {
    let efs: Vec<usize> = a
        .ef_search
        .split(',')
        .map(|s| s.trim().parse().context("ef_search must be integers"))
        .collect::<Result<_>>()?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(a.threads)
        .build()?;
    let (corpus, queries) = data::clustered(a.n, a.queries, a.dim, a.clusters, a.noise, a.seed);
    eprintln!(
        "[bench] ground truth: exact scan of {} x {} for {} queries",
        a.n, a.dim, a.queries
    );
    let truth = data::exact_top_k(&corpus, &queries, a.k);

    let rss_before = rss_mb()?;
    let t0 = Instant::now();
    let ix = build(&a, &corpus)?;
    let build_s = t0.elapsed().as_secs_f64();
    let rss_after = rss_mb()?;
    eprintln!(
        "[bench] built {} in {build_s:.2}s",
        serde_json::to_string(&a.index)?
    );

    // Exact search has no breadth knob: sweep once.
    let sweep = if a.index == IndexKind::Flat {
        vec![0]
    } else {
        efs
    };
    let mut points = Vec::new();
    for ef in sweep {
        // Warm up caches and lazily-touched pages before timing.
        for q in queries.iter().take(a.queries.min(100)) {
            ix.query(q, a.k, ef)?;
        }
        let mut hist = Histogram::<u64>::new(3)?;
        let mut found = Vec::with_capacity(queries.len());
        let t = Instant::now();
        for q in &queries {
            let s = Instant::now();
            found.push(ix.query(q, a.k, ef)?);
            hist.record(s.elapsed().as_micros() as u64)?;
        }
        let single_qps = queries.len() as f64 / t.elapsed().as_secs_f64();
        let t = Instant::now();
        pool.install(|| {
            queries
                .par_iter()
                .try_for_each(|q| ix.query(q, a.k, ef).map(drop))
        })?;
        let concurrent_qps = queries.len() as f64 / t.elapsed().as_secs_f64();
        let p = SearchPoint {
            ef,
            recall_at_k: recall(&found, &truth),
            p50_us: hist.value_at_quantile(0.50),
            p95_us: hist.value_at_quantile(0.95),
            p99_us: hist.value_at_quantile(0.99),
            p999_us: hist.value_at_quantile(0.999),
            max_us: hist.max(),
            single_thread_qps: single_qps,
            concurrent_qps,
        };
        eprintln!(
            "[bench] ef={ef:<4} recall@{}={:.4} p50={}us p99={}us p99.9={}us qps1={:.0} qps{}={:.0}",
            a.k,
            p.recall_at_k,
            p.p50_us,
            p.p99_us,
            p.p999_us,
            p.single_thread_qps,
            a.threads,
            p.concurrent_qps
        );
        points.push(p);
    }
    Ok(Report {
        index: a.index,
        n: a.n,
        dim: a.dim,
        k: a.k,
        queries: a.queries,
        clusters: a.clusters,
        m: a.m,
        ef_construction: a.ef_construction,
        threads: a.threads,
        build_s,
        build_vectors_per_s: a.n as f64 / build_s,
        rss_before_mb: rss_before,
        rss_after_build_mb: rss_after,
        points,
    })
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Vector(a) => println!("{}", serde_json::to_string(&vector(a)?)?),
        Cmd::Retrieval(a) => println!("{}", serde_json::to_string(&retrieval::run(a)?)?),
    }
    Ok(())
}
