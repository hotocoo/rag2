#!/usr/bin/env bash
# Vector index comparison. One process per index so RSS is attributable.
# Usage: scripts/bench-vector.sh [N] [DIM] [OUT]
set -euo pipefail
N="${1:-100000}"; DIM="${2:-768}"; OUT="${3:-docs/benchmarks/results/vector-$(date +%Y%m%d-%H%M%S).jsonl}"
mkdir -p "$(dirname "$OUT")"
cargo build --release -q -p rag2-bench
for ix in flat hnsw embedvec; do
  ./target/release/rag2-bench vector --index "$ix" --n "$N" --dim "$DIM" | tee -a "$OUT"
done
echo "results: $OUT" >&2
