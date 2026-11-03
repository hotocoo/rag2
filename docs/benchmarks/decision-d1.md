# Decision model under extreme load: Liquid AI d1-2B (Q8_0) in RAG2

Real model, real data, real load. No mocks, no synthetic labels. Raw results:
`docs/benchmarks/results/decision-d1-*.json`.

Reproduce:

```sh
brew install llama.cpp                       # 0.8.0 (b11960) or newer serves /v1/systemone
hf download LiquidAI/d1-2B-GGUF d1-2B-Q8_0.gguf --local-dir models/d1-2B
llama-server -m models/d1-2B/d1-2B-Q8_0.gguf --alias d1-2B \
  -b 16384 -ub 16384 -np 8 -c 131072 --port 8081
# data (bench-data/raw): ANLI v1.0, XNLI 1.0, SciFact (claims + BEIR)
cargo build --release -p rag2-bench
rag2-bench decision --suites nli,xnli,claims                  # quality
rag2-bench decision --suites load --load-abstracts 1 --load-questions 1,16 \
  --load-concurrency 1,2,4,8,16,32,64 --load-requests 256     # load, short states
rag2-bench decision --suites load --load-abstracts 1 --load-questions 16 \
  --load-concurrency 1,2,8 --load-requests 16                 # many questions per call
rag2-bench decision --suites load --load-abstracts 8,24 --load-questions 1 \
  --load-concurrency 1,2,8 --load-requests 16                 # long states
rag2-bench decision --suites relevance --depth 5 --k 5 --concurrency 1          # both gate modes
rag2-bench decision --suites relevance,e2e --gate-modes per-item --depth 5 --k 5 \
  --concurrency 1 --e2e-threads 1,8,32 --e2e-queries 50 --timeout-ms 10000 \
  --gate-min-confidence 0.7                                   # e2e before (at commit c32e19f)
rag2-bench decision --suites e2e --depth 5 --k 5 --concurrency 1 \
  --e2e-threads 1,8,32 --e2e-queries 50 --timeout-ms 2000     # e2e after (stage deadline)
```

## Setup

- Model: `LiquidAI/d1-2B-GGUF`, file `d1-2B-Q8_0.gguf` (1.85 GB), a non-generative decision model
  (typed answers with probabilities from one forward pass). Served by llama.cpp 0.8.0
  (b11960, Homebrew bottle, Metal) on `/v1/systemone`.
- Client: RAG2's `SystemOneHttp` adapter (the production path) for every quality call; raw HTTP
  only where the adapter cannot express the probe (reversed option order, token counts).
- Machine: Apple M4 Max (16 cores, 64 GB unified memory), macOS 26, release build, 2026-11-03.

## Quality on the hardest public data

Each item is asked three ways: a `choice` between entailment / neutral / contradiction (the
adapter's order), the same question with the options reversed on the wire, and RAG2's
contradiction gate (`noul`: "Does the premise contradict the hypothesis?").

- ANLI rounds 1-3 test sets (3,200 items): premises and hypotheses written by people paid to
  fool strong NLI models, each round harder than the last. The hardest public NLI data.
- XNLI test (200 per language, 10 languages that d1 lists): human translations.
- SciFact dev claims against every cited abstract (340 pairs): expert-annotated
  SUPPORT / CONTRADICT, cited without rationale = neutral.

| Set | n | Accuracy | Macro-F1 | ECE (top-1) | Order agreement | Contradiction AUROC | Threshold for 0.9 precision (recall) |
|---|---|---|---|---|---|---|---|
| All | 5,540 | 0.667 | 0.667 | 0.033 | 0.983 | 0.792 | 0.869 (0.26) |
| ANLI R1 | 1,000 | 0.731 | 0.731 | 0.033 | 0.980 | 0.744 | 0.924 (0.08) |
| ANLI R2 | 1,000 | 0.588 | 0.588 | 0.098 | 0.977 | 0.668 | 0.984 (0.00) |
| ANLI R3 | 1,200 | 0.514 | 0.515 | 0.141 | 0.978 | 0.666 | not reachable |
| SciFact claims | 340 | 0.797 | 0.788 | 0.050 | 0.997 | 0.878 | 0.871 (0.34) |
| XNLI en | 200 | 0.845 | 0.847 | 0.079 | 0.995 | 0.967 | 0.468 (0.88) |
| XNLI de | 200 | 0.775 | 0.776 | 0.061 | 0.985 | 0.932 | 0.549 (0.78) |
| XNLI fr | 200 | 0.790 | 0.788 | 0.060 | 0.990 | 0.926 | 0.478 (0.84) |
| XNLI es | 200 | 0.764 | 0.763 | 0.090 | 1.000 | 0.921 | 0.522 (0.85) |
| XNLI ru | 200 | 0.720 | 0.712 | 0.054 | 0.985 | 0.893 | 0.780 (0.55) |
| XNLI vi | 200 | 0.715 | 0.706 | 0.040 | 0.980 | 0.894 | 0.697 (0.58) |
| XNLI zh | 200 | 0.715 | 0.708 | 0.040 | 0.985 | 0.876 | 0.684 (0.58) |
| XNLI th | 200 | 0.720 | 0.713 | 0.086 | 0.980 | 0.879 | 0.619 (0.63) |
| XNLI hi | 200 | 0.702 | 0.698 | 0.071 | 0.980 | 0.837 | 0.652 (0.51) |
| XNLI ar | 200 | 0.700 | 0.699 | 0.080 | 0.990 | 0.862 | 0.699 (0.58) |

Confusion over all 5,540 items (rows gold, columns predicted; entailment / neutral /
contradiction): `[[1114, 540, 219], [159, 1346, 352], [250, 321, 1230]]`.

What this means for RAG2:

- Chance is 0.333. On adversarial ANLI R3 d1-2B reaches 0.514; on natural language (XNLI,
  SciFact) 0.70-0.85. Its confidences are calibrated overall (ECE 0.033) and drift to
  overconfident only on the adversarial rounds (R3 ECE 0.141).
- Answers barely depend on option order (0.977-1.000 agreement): confusion comes from the
  content, not from where an option sits.
- As a contradiction gate the model separates well on natural text (AUROC 0.84-0.97) and
  modestly on adversarial text (0.67). A gate that must be right 90% of the time when it fires
  needs a high threshold and then catches a minority of contradictions; on ANLI R3 no threshold
  reaches 0.9 precision. RAG2 therefore keeps contradiction confirmation fail-open (unsure keeps
  the flag from the rule detectors) and the thresholds in configuration.
- 9 of 16,620 calls failed, all by hitting the 300 s client timeout (see the next sections).

## Load: the server under concurrent clients

Real SciFact abstracts as the state (a different window per request, so no two states repeat),
one question (`noul`) or 16 mixed questions per call, swept over concurrent clients talking to
llama-server directly (no RAG2 admission in front, to measure the server itself). Latency in ms;
with 16 requests per point, p95-max of the long tiers are a single slowest request.

| Abstracts | Input tokens | Questions | Clients | Requests | p50 | p95 | p99 | max | Calls/s | Decisions/s | Errors |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 346 | 1 | 1 | 256 | 110 | 318 | 382 | 459 | 7.6 | 7.6 | 0 |
| 1 | 346 | 1 | 2 | 256 | 388 | 567 | 703 | 734 | 5.1 | 5.1 | 0 |
| 1 | 346 | 1 | 4 | 256 | 346 | 844 | 1,768 | 45,154 | 5.6 | 5.6 | 0 |
| 1 | 346 | 1 | 8 | 256 | 386 | 616 | 49,414 | 49,906 | 5.1 | 5.1 | 0 |
| 1 | 346 | 1 | 16 | 256 | 1,801 | 3,049 | 52,003 | 52,363 | 4.9 | 4.9 | 0 |
| 1 | 346 | 1 | 32 | 256 | 3,793 | 8,303 | 44,204 | 44,630 | 5.7 | 5.7 | 0 |
| 1 | 346 | 1 | 64 | 256 | 9,306 | 11,575 | 44,597 | 44,958 | 5.7 | 5.7 | 0 |
| 1 | 5,723 | 16 | 1 | 16 | 2,976 | 6,730 | 6,730 | 6,730 | 0.3 | 4.8 | 0 |
| 1 | 5,723 | 16 | 2 | 16 | 1,864 | 14,778 | 14,778 | 14,778 | 0.6 | 9.2 | 0 |
| 1 | 5,616 | 16 | 8 | 32 | 17,891 | 26,821 | 87,359 | 87,359 | 0.4 | 5.9 | 0 |
| 8 | 2,577 | 1 | 1 | 16 | 805 | 978 | 978 | 978 | 1.2 | 1.2 | 0 |
| 8 | 2,577 | 1 | 2 | 16 | 2,058 | 3,195 | 3,195 | 3,195 | 0.9 | 0.9 | 0 |
| 8 | 2,550 | 1 | 8 | 32 | 2,035 | 29,295 | 30,179 | 30,179 | 1.1 | 1.1 | 0 |
| 24 | 7,448 | 1 | 1 | 16 | 2,927 | 3,582 | 3,582 | 3,582 | 0.3 | 0.3 | 0 |
| 24 | 7,448 | 1 | 2 | 16 | 5,734 | 8,405 | 8,405 | 8,405 | 0.3 | 0.3 | 0 |
| 24 | 7,436 | 1 | 8 | 32 | 5,808 | 88,474 | 90,964 | 90,964 | 0.4 | 0.4 | 0 |

Findings:

1. **llama.cpp 0.8.0 runs d1 requests one at a time.** Throughput is flat from 1 to 64 clients
   (4.9-7.6 calls/s at 346 tokens); its log shows slots released one after another. Parallel
   slots (`-np`) add queueing, not capacity. One client is the fastest configuration on every
   axis: p50 110 ms, p99 382 ms, 7.6 calls/s.
2. **Queued requests starve.** From 4 concurrent clients on, some requests wait 30-90 s while
   later ones are served: p99 jumps from 0.4 s to 44-52 s at short states and to 91 s at
   7.4K-token states. The quality run (8 clients) hit the 300 s timeout 9 times out of 16,620.
3. **Cost is linear in input tokens, about 0.39 ms per token** (346 tokens 110 ms; 2.6K 805 ms;
   7.4K 2.9 s) on this machine. The model card's 18.6 ms per decision is for Transformers on
   MPS with `model.compile()`, not llama.cpp.
4. **Many questions per call do not amortize the state on llama.cpp.** 16 questions over one
   346-token state are sent as 5.7K input tokens (the state is read once per question): 4.8
   decisions/s, below one question per call.

What RAG2 does about it:

- `SystemOneHttp` holds its own FIFO queue in front of the server (`concurrency` requests in
  flight, default **1**), so the server never holds a queue to starve, waiting is first-come
  first-served, and the queue wait counts against `timeout_ms` (then the gate fails open).
- `decision_models.*.concurrency` defaults to 1 and the d1 example profile and compose overlay use
  `-np 1`.

## d1 inside RAG2: relevance gate and reranker

SciFact BEIR test queries (300, expert relevance judgements) against the 5,183-abstract corpus,
ingested into the embedded engine (hash embedder + BM25, hybrid RRF). For each query the top 5
unique documents from RAG2's own retrieval are the candidates, so every negative is a hard
negative the retriever ranked highly. d1 judges each with the production gate question ("Does
the passage contain information that answers the query?"); rerank = sort by P(yes). Production
setting: one request in flight.

| Gate mode | Calls | Wall | AUROC | nDCG@5 retrieval / reranked | MRR@5 retrieval / reranked | Precision 0.9 threshold (recall) | Call p50 / p99 |
|---|---|---|---|---|---|---|---|
| per_item (default) | 1,351 | 150 s | 0.942 | 0.434 / **0.530** | 0.406 / **0.538** | P(yes) 0.70 (0.30) | 88 / 386 ms |
| joint | 300 | 443 s | 0.936 | 0.434 / 0.529 | 0.406 / 0.537 | P(yes) 0.69 (0.43) | 1,445 / 3,174 ms |

- d1 as reranker lifts nDCG@5 by 22% and MRR@5 by 33% over hybrid retrieval, with AUROC 0.94
  between relevant and hard-negative passages.
- `joint` (all passages in one state, one question each) gives the same quality but is 3x slower
  on llama.cpp, which reads the whole state once per question (finding 4 above). `per_item`
  stays the default; `joint` is for servers that read the state once.
- At `min_confidence: 0.7` the gate keeps 65% of relevant passages and removes 96% of
  irrelevant ones: too aggressive for recall-sensitive use. The threshold sweep below sets the
  shipped default.

Gate threshold sweep (same 1,348 judged pairs; a passage is dropped when d1 says "no" with at
least this confidence):

| min_confidence | Keeps relevant | Removes irrelevant |
|---|---|---|
| 0.6 | 0.526 | 0.982 |
| 0.7 | 0.649 | 0.962 |
| 0.8 | 0.731 | 0.937 |
| **0.9** (shipped) | **0.895** | **0.850** |
| 0.95 | 0.965 | 0.670 |
| 0.99 | 1.000 | 0.133 |

The shipped d1 profiles (`examples/profiles/d1-systemone.yaml`, `deploy/profile.container.d1.yaml`)
use 0.9 for the relevance gate and the contradiction check.

## End to end: RAG2 queries with d1 gates under concurrent load

Full engine queries (300 SciFact test queries, first 50 per point, top 5) with three pipelines:
`hybrid` (no model), `gated` (d1 relevance gate per passage + d1 contradiction check, the
default mode) and `gated_joint` (same, joint mode). Query threads 1 / 8 / 32, each with and
without a writer re-ingesting copies of the corpus at the same time. One d1 server, RAG2
admission `concurrency: 1`, abstention on.

### Before: one timeout per call (10 s)

| Pipeline | Threads | Ingest | p50 ms | p95 ms | p99 ms | QPS | Gate fail-open | Abstained |
|---|---|---|---|---|---|---|---|---|
| hybrid | 1 | no | 1 | 2 | 4 | 945 | 0 | 0 |
| hybrid | 32 | yes | 3 | 19 | 20 | 2,370 | 0 | 0 |
| gated | 1 | no | 638 | 1,214 | 3,613 | 1.3 | 0 | 29 |
| gated | 1 | yes | 666 | 1,049 | 1,168 | 1.5 | 0 | 28 |
| gated | 8 | no | 6,398 | 13,623 | 17,007 | 1.1 | 0 | 28 |
| gated | 32 | no | 20,595 | 29,475 | 41,386 | 1.2 | 0 | 32 |
| gated | 32 | yes | 17,121 | 24,625 | 35,226 | 1.4 | 0 | 32 |
| gated_joint | 1 | no | 1,338 | 2,587 | 2,726 | 0.7 | 0 | 26 |
| gated_joint | 8 | no | 10,011 | 20,021 | 20,021 | 0.6 | 41 | 9 |
| gated_joint | 32 | no | 10,011 | 20,021 | 20,021 | 1.7 | 57 | 1 |

- Retrieval alone is not the bottleneck (sub-5 ms p99 up to 32 threads, during re-ingest too).
- With d1 in the path, capacity is d1's: about 1.3 gated queries/s per server (5 gate calls plus
  contradiction checks at ~0.1 s each). Past that, waiting grows with the number of users; RAG2's
  FIFO admission keeps it fair (p99/p50 about 2, against 10-100 when clients hit the server
  directly) and nothing errors, but p99 reaches 41 s at 32 threads because each of a query's
  calls had its own 10 s timeout.
- Joint mode is slower per query and hits its timeout under load (fail-open warnings).

### After: one deadline per decision stage (2 s), shipped profile settings

`SystemOneHttp::decide_each` now gives every passage of a stage one shared deadline, queueing
included; whatever is unanswered by then fails open (passage or flag kept, warning attached).
Gate `min_confidence: 0.9`, d1 `timeout_ms: 2000`. Raw:
`docs/benchmarks/results/decision-d1-e2e-bounded.json`.

| Pipeline | Threads | Ingest | p50 ms | p95 ms | p99 ms | max ms | QPS | Gate fail-open | Abstained |
|---|---|---|---|---|---|---|---|---|---|
| hybrid | 32 | yes | 2 | 3 | 5 | 5 | 7,535 | 0 | 0 |
| gated | 1 | no | 608 | 996 | 1,167 | 1,167 | 1.5 | 0 | 13 |
| gated | 1 | yes | 370 | 1,158 | 3,316 | 3,316 | 1.9 | 1 | 11 |
| gated | 8 | no | 2,004 | 4,008 | **4,014** | 4,014 | 2.9 | 58 | 1 |
| gated | 32 | no | 2,004 | 4,004 | **4,006** | 4,006 | 8.3 | 63 | 0 |
| gated | 32 | yes | 2,004 | 4,006 | **4,006** | 4,006 | 8.3 | 65 | 0 |
| gated_joint | 1 | no | 1,803 | 2,914 | 4,006 | 4,006 | 0.6 | 17 | 9 |
| gated_joint | 32 | yes | 2,003 | 4,006 | 4,006 | 4,006 | 8.3 | 66 | 0 |

- p99 at 32 concurrent users drops from 41.4 s to 4.0 s, the configured bound (relevance stage
  plus contradiction stage, 2 s each), with zero errors, during concurrent re-ingest too.
- The cost is explicit: beyond about 1.5 gated queries/s per d1 server, the gate is skipped
  for the excess and responses carry `decision model d1 failed (... saturated ...)` warnings.
  Evidence is never dropped by an unavailable gate.
- To keep gating at higher load, run more d1 servers (each a `decision_models` entry or behind
  a balancer) or gate fewer passages (`top_k`).

## Summary

| Question | Answer (measured) |
|---|---|
| How good is d1-2B at hard decisions? | 0.667 three-way accuracy over 5,540 real items; 0.514 on ANLI R3 (chance 0.333); 0.70-0.85 on 10 XNLI languages; ECE 0.033 |
| Is it confused by option order? | No: 0.977-1.000 agreement when options are reversed |
| Does it help RAG2? | Yes: reranking lifts SciFact nDCG@5 0.434 to 0.530 (+22%), AUROC 0.94 |
| What limits it? | llama.cpp 0.8.0 serves one call at a time, ~0.39 ms per input token on M4 Max; queued requests starve (p99 49 s at 8 clients) |
| What does RAG2 do about it? | FIFO admission with 1 in flight, one deadline per decision stage, fail-open gates: p99 4.0 s at 32 users |
