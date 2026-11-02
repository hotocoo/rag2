# Vector index selection

Decision: **`hnsw_rs` 0.3.5 behind `rag2_vector::HnswIndex`** for large tenants, **exact
`rag2_vector::FlatIndex`** below `indexes.vector.flat_below` (default 10,000 vectors).
embedvec 0.10.0 was evaluated first, as the goal required, and rejected on measured recall.

## Method

- Machine: Apple M4 Max, 16 cores, 64 GB, macOS 26 (Darwin 25.5), rustc 1.98.1, release profile
  (`lto = "fat"`, `codegen-units = 1`).
- Corpus: synthetic, deterministic (`--seed 42`). 256 Gaussian clusters on the unit sphere,
  per-vector noise of norm 0.35. Queries are held out (drawn from the same mixture, not from the
  corpus). Inside a cluster the neighbours are isotropic noise, which is harder for graph indexes
  than real embeddings usually are; treat recall here as a conservative floor.
- Ground truth: exact cosine top-10 by brute force.
- Latency: 1,000 queries, single thread, after 100 warm-up queries, HdrHistogram (3 significant
  digits). Throughput: the same queries across an 8-thread rayon pool.
- RSS: `ps -o rss` before and after building, one index per process. The "before" figure includes
  the generated corpus held by the harness.
- Parameters for both graph indexes: `m = 16`, `ef_construction = 200`.
- Reproduce: `scripts/bench-vector.sh 100000 768`. Raw results:
  `docs/benchmarks/results/vector-100k-768.jsonl`.

## Results: 100,000 x 768, k = 10

| Index | Build | RSS added | ef | recall@10 | p50 | p99 | p99.9 | QPS (1 thread) | QPS (8 threads) |
|---|---|---|---|---|---|---|---|---|---|
| flat (exact) | 0.06 s | 295 MB | n/a | 1.0000 | 1,248 µs | 2,247 µs | 3,609 µs | 765 | 1,209 |
| hnsw_rs | 5.56 s | 496 MB | 32 | 0.9689 | 155 µs | 376 µs | 420 µs | 5,735 | 36,702 |
| hnsw_rs | | | 64 | 0.9955 | 202 µs | 471 µs | 568 µs | 4,494 | 29,187 |
| hnsw_rs | | | **128** | **0.9989** | 264 µs | 609 µs | 698 µs | 3,434 | 21,191 |
| hnsw_rs | | | 256 | 1.0000 | 383 µs | 818 µs | 1,019 µs | 2,392 | 14,981 |
| embedvec | 63.69 s | 53 MB | 32 | 0.3143 | 119 µs | 281 µs | 441 µs | 7,988 | 60,314 |
| embedvec | | | 64 | 0.3426 | 122 µs | 283 µs | 324 µs | 6,819 | 50,863 |
| embedvec | | | 128 | 0.3594 | 127 µs | 317 µs | 366 µs | 6,238 | 47,621 |
| embedvec | | | 256 | 0.3899 | 192 µs | 348 µs | 456 µs | 5,203 | 38,852 |

The flat scan's single-query latency includes intra-query parallelism (rayon chunks of 8,192 rows),
which is why its 8-thread throughput is low: every query already uses the whole pool.

## Harness check (5,000 x 64)

embedvec reaches recall@10 0.877 / 0.918 / 0.954 / 0.989 at ef 32 / 64 / 128 / 256, against
1.0000 for hnsw_rs at every ef. The harness maps ids correctly (it refuses to run if embedvec
assigns an id other than the insertion index), so the low recall at 100k is embedvec's.

## Flat vs HNSW crossover (768 dims, ef = 128)

| n | flat p50 / p99 | hnsw p50 / p99 | flat QPS (8 thr) | hnsw QPS (8 thr) |
|---|---|---|---|---|
| 10,000 | 365 / 681 µs | 389 / 1,114 µs | 16,286 | 15,814 |
| 20,000 | 433 / 1,053 µs | 394 / 726 µs | 6,506 | 17,248 |
| 50,000 | 742 / 1,180 µs | 270 / 485 µs | 2,743 | 23,241 |

Flat matches HNSW at 10,000 vectors with exact recall and no build cost; at 20,000 HNSW already
has 2.6x the concurrent throughput. Default `flat_below = 10_000`.

## Why not embedvec

- Recall@10 of 0.31-0.39 at 100k vectors, at every search breadth tried.
- Single-writer API (`&mut self` inserts) made the build 11x slower than `hnsw_rs`'s parallel
  insert.
- Default features depend on fjall 2.x, which conflicts with RAG2's fjall 3.x storage.
- Its lower memory is real (53 MB added against 496 MB) and is worth revisiting with a quantized
  index of our own if memory becomes the binding constraint.

## Known limits of hnsw_rs, and how RAG2 handles them

- No delete: RAG2 tombstones and filters; `HnswIndex::needs_rebuild` reports when tombstones exceed
  `hnsw_max_tombstone_ratio`, and the engine rebuilds from stored vectors.
- `anndists::DistDot` asserts `dot <= 1`, which float rounding violates on self-matches and would
  abort the process under `panic = "abort"`. RAG2 uses its own clamped `UnitCosine` distance.
- `Hnsw::new` calls `process::exit` when `m > 256`; RAG2 validates parameters first.

## Not yet measured

Real embedding distributions (needs a local embedding checkpoint), 1M+ vectors, filtered search
under high tombstone ratios, and memory with quantization.
