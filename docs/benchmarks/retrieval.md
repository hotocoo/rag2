# Retrieval quality and latency: BEIR SciFact

Reproduce:

```sh
curl -LO https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip && unzip scifact.zip -d bench-data/raw
cargo build --release -p rag2-bench --features metal
rag2-bench retrieval --data bench-data/raw/scifact --work /tmp/rag2-bench --embedder hash
rag2-bench retrieval --data bench-data/raw/scifact --work /tmp/rag2-bench \
  --embedder models/modernbert-embed-base --device metal \
  --query-prefix "search_query: " --document-prefix "search_document: " --batch 128
```

Raw results: `docs/benchmarks/results/retrieval-scifact-*.json`.

## Setup

- Data: BEIR SciFact (CC BY-SA 4.0): 5,183 abstracts, 300 test queries with relevance judgements.
  Documents are `title + abstract`, chunked with the default profile (1,200 chars, 150 overlap):
  11,973 chunks.
- Engine: embedded `rag2-engine`, no HTTP. Lexical commit synchronous. Cache disabled
  (`no_cache`). Abstention, dedup and contradiction screening off, so ranking is measured alone.
- Metric level: documents. Each query retrieves 30 chunks, which collapse to the first 10 unique
  documents; recall@10, nDCG@10 (binary gains), MRR@10.
- Latency: single thread, 300 queries after 20 warm-up queries (HdrHistogram). Throughput: same
  queries over 8 threads.
- Machine: Apple M4 Max (16 cores, 64 GB), macOS 26, release build, 2026-11-02.
- Dense models: `hash` (feature hashing, no model) and `nomic-ai/modernbert-embed-base`
  (Apache-2.0) on candle 0.11 with Metal, mean pooling, prefixes per the model card.

## Quality

HNSW is built with parallel inserts, so graph shape varies between runs. Across repeated hash runs,
dense recall@10 varied between 0.300 and 0.309 and hybrid between 0.666 and 0.678. Lexical is
deterministic.

| Embedder | Pipeline | recall@10 | nDCG@10 | MRR@10 |
|---|---|---|---|---|
| (none) | lexical (BM25) | 0.7540 | 0.6336 | 0.6017 |
| hash | dense | 0.2969 | 0.2140 | 0.1897 |
| hash | hybrid (RRF) | 0.6633 | 0.4718 | 0.4161 |
| modernbert-embed-base | dense | 0.8131 | 0.6885 | 0.6571 |
| modernbert-embed-base | hybrid (RRF) | **0.8434** | **0.7011** | **0.6636** |

- With a real embedding model, hybrid beats both lexical and dense alone.
- With the hash embedder, hybrid at equal weights is worse than lexical alone. The hash embedder
  is a zero-setup baseline, not a quality option; lower its `weight` or use lexical routing if no
  model is available.
- The ModernBERT dense score is in line with that model's published SciFact result. That is
  independent evidence that the ModernBERT encoder vendored with Laya computes correctly on
  candle 0.11.

## Confidence calibration

Platt scaling of `confidence_detail.raw` against "the top document is relevant", fitted on the
even-indexed test queries and evaluated on the odd-indexed ones (10-bin expected calibration
error). The fitted `slope`/`intercept` go into `pipelines.<name>.calibration`.

| Embedder | Pipeline | top-1 accuracy | slope | intercept | ECE raw (held-out) | ECE calibrated (held-out) |
|---|---|---|---|---|---|---|
| hash | lexical | 0.520 | 0.254 | -0.586 | 0.3237 | 0.0845 |
| hash | dense | 0.143 | 2.969 | -2.731 | 0.4351 | 0.0537 |
| hash | hybrid | 0.310 | 2.520 | -2.304 | 0.3387 | 0.0671 |
| modernbert-embed-base | lexical | 0.520 | 0.240 | -0.627 | 0.3007 | 0.1174 |
| modernbert-embed-base | dense | 0.580 | 1.687 | -1.553 | 0.2160 | 0.0853 |
| modernbert-embed-base | hybrid | 0.567 | 2.036 | -2.369 | 0.1956 | 0.0697 |

The uncalibrated score is not a probability: its expected calibration error is 0.20 to 0.44.
Calibration on 150 labelled queries brings it to 0.05 to 0.12 on unseen queries. ModernBERT run
of 2026-11-03 (Metal); the coefficients depend on the embedder and the corpus, so fit them on
your own labelled queries.

## Latency and throughput

| Embedder | Pipeline | p50 | p99 | QPS (1 thread) | QPS (8 threads) |
|---|---|---|---|---|---|
| (none) | lexical | 388 µs | 768 µs | 2,465 | 15,335 |
| hash | dense | 545 µs | 1,372 µs | 1,624 | 10,123 |
| hash | hybrid | 938 µs | 1,427 µs | 1,053 | 6,240 |
| modernbert (Metal) | dense | 39.9 ms | 72.3 ms | 27 | 63 |
| modernbert (Metal) | hybrid | 38.0 ms | 75.5 ms | 27 | 35 |

With a neural embedder, query latency is the query-embedding forward pass (one Metal dispatch per
query). Throughput does not scale with threads because GPU submissions serialize. The retrieval
work after embedding (HNSW + BM25 + fusion + evidence analysis) costs under 1 ms, per the hash
rows.

## Ingestion

| Embedder | Time | Docs/s | Chunks/s | RSS after ingest |
|---|---|---|---|---|
| hash | 6.7 s | 772 | 1,783 | 244 MB |
| modernbert-embed-base (Metal) | 485.3 s | 11 | 25 | 959 MB |

## Ingestion and query interference

The `hybrid` pipeline (hash embedder) measured idle, then while a second thread re-ingested
the whole corpus under new ids (`--interference`):

| Condition | p50 | p99 | Queries measured |
|---|---|---|---|
| idle | 913 µs | 1,265 µs | 300 |
| during ingestion (473 docs/s) | 913 µs | 1,680 µs | 11,569 |

Reads take no tenant lock and the indexes are updated incrementally, so median latency is
unchanged; the tail grows with CPU contention.

## Recovery

| Scenario | Result |
|---|---|
| Cold start after clean shutdown (10,366 documents, 23,946 chunks): open storage, rebuild in-memory vector indexes | 1.55 s |
| Lexical index deleted, rebuilt from storage at startup | 2.02 s, queries OK |
| `scripts/crash-test.sh 3`: `kill -9` during two concurrent ingest streams, three rounds | PASS: after each restart storage, lexical and vector counts agree (3,950 / 7,850 / 10,950 documents, the last on HNSW); ready in 496-788 ms |

## Measured improvements during development

Same machine and dataset, hash embedder, before and after:

| Change | Metric | Before | After |
|---|---|---|---|
| One atomic storage batch and one id reservation per ingest request (was two fsyncs per document) | ingest | 106 docs/s | 796 docs/s |
| Decoded chunk and document caches on the query path | lexical p99 | 26.4 ms | 0.73 ms |
| (same) | hybrid QPS, 8 threads | 2,312 | 6,631 |

## Not yet measured

ModernBERT on CPU for the full corpus (estimated about an hour on this machine, so not run), a
query-embedding cache, other BEIR datasets, and corpora beyond about 10k documents. Those are next in `docs/STATUS.md`.
