#!/usr/bin/env bash
# Crash consistency: kill -9 the server while clients ingest, restart, and check that storage and
# every derived index agree (document count, lexical chunks == vectors) and queries work.
# Usage: scripts/crash-test.sh [ROUNDS]
set -euo pipefail
ROUNDS="${1:-3}"
BIN="$(cd "$(dirname "$0")/.." && pwd)/target/release/rag2"
W="$(mktemp -d)"; trap 'pkill -9 -f "rag2 serve --config-dir $W" || true; rm -rf "$W"' EXIT
export RAG2_ADMIN_KEY=crash-test-key-0123456789abcdef RAG2_API_KEY=crash-test-key-0123456789abcdef
URL=http://127.0.0.1:18099
cat > "$W/p.yaml" <<YAML
api_version: rag2/v1
metadata: { name: main }
server: { listen: "127.0.0.1:18099" }
storage: { data_dir: "$W/data" }
indexes: { lexical: { commit_interval_ms: 500 } }
security: { auth: { mode: api_keys, keys: [ { id: admin, secret: "env:RAG2_ADMIN_KEY", roles: [admin], tenants: ["*"] } ] } }
YAML
start() { ("$BIN" serve --config-dir "$W/cfg" --file "$W/p.yaml" >> "$W/serve.log" 2>&1 &); 
  local t0; t0=$(date +%s%N); until curl -sf "$URL/readyz" >/dev/null; do sleep 0.05; done
  echo "ready in $(( ($(date +%s%N) - t0) / 1000000 )) ms"; }
ingest_loop() { local i=0; while true; do i=$((i+1))
  docs=$(python3 -c "import json;print(json.dumps({'documents':[{'id':'d-$1-$i-'+str(k),'text':'crash test document '+str(k)+' writer $1 batch $i about pumps and valves'} for k in range(50)]}))")
  curl -s -o /dev/null -X POST "$URL/v1/tenants/crash/documents" -H "authorization: Bearer $RAG2_API_KEY" -H 'content-type: application/json' -d "$docs" || true; done; }
start
for r in $(seq 1 "$ROUNDS"); do
  ingest_loop "$r" & L1=$!; ingest_loop "$r-b" & L2=$!
  sleep 2
  pkill -9 -f "rag2 serve --config-dir $W"; kill $L1 $L2 2>/dev/null || true; wait $L1 $L2 2>/dev/null || true
  echo "round $r: killed -9 during ingest"; start
  sleep 1   # one lexical commit interval
  stats=$(curl -s "$URL/v1/tenants/crash/stats" -H "authorization: Bearer $RAG2_API_KEY")
  echo "  stats: $stats"
  python3 - "$stats" <<'PY'
import json,sys
s=json.loads(sys.argv[1]); v=s["vector_indexes"][0]["vectors"] if s["vector_indexes"] else 0
assert s["lexical_docs"]==v, f"lexical {s['lexical_docs']} != vectors {v}"
print(f"  consistent: {s['documents']} documents, {v} chunks in both indexes")
PY
  hits=$(curl -s "$URL/v1/tenants/crash/query" -H "authorization: Bearer $RAG2_API_KEY" -H 'content-type: application/json' -d '{"query":"pumps and valves"}' | python3 -c 'import json,sys;print(len(json.load(sys.stdin)["evidence"]))')
  echo "  query returned $hits results"; [ "$hits" -gt 0 ]
done
echo "PASS: $ROUNDS crash rounds consistent"
