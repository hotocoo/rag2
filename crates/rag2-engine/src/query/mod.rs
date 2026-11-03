//! Query execution: route, retrieve (per hop), fuse, filter, expand, deduplicate, analyze,
//! calibrate, decide whether to abstain, assemble context.

pub mod analyze;
pub mod confidence;
pub mod context;
pub mod fuse;
pub mod route;

use crate::Engine;
use crate::api::{QueryRequest, QueryResponse};
use crate::components::Components;
use crate::text::{content_words, words};
use fuse::{Fused, RankedList};
use rag2_config::schema::{DecisionGate, GateMode, Pipeline, Profile, RetrieverConfig};
use rag2_core::interfaces::{Answer, Question, check_answers};
use rag2_core::model::{
    AbstainReason, Abstention, Chunk, Contradiction, DocumentRecord, Evidence, Hit, Metadata,
    Provenance, RetrieverKind, StageTimings,
};
use rag2_core::{ChunkId, DocumentId, Error, Result, TenantId};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

/// Per-request cache of document records (filters, graph and provenance all need them).
struct Docs<'a> {
    engine: &'a Engine,
    tenant: &'a TenantId,
    map: HashMap<DocumentId, Option<Arc<DocumentRecord>>>,
}

impl Docs<'_> {
    fn get(&mut self, id: &DocumentId) -> Result<Option<Arc<DocumentRecord>>> {
        if let Some(d) = self.map.get(id) {
            return Ok(d.clone());
        }
        let d = self.engine.record(self.tenant, id)?;
        self.map.insert(id.clone(), d.clone());
        Ok(d)
    }
}

fn value_matches(doc: Option<&serde_json::Value>, want: &serde_json::Value) -> bool {
    let Some(have) = doc else { return false };
    let wants: Vec<&serde_json::Value> = match want {
        serde_json::Value::Array(a) => a.iter().collect(),
        w => vec![w],
    };
    match have {
        serde_json::Value::Array(h) => h.iter().any(|x| wants.contains(&x)),
        h => wants.contains(&h),
    }
}

fn passes_filters(rec: &DocumentRecord, filters: &Metadata) -> bool {
    filters
        .iter()
        .all(|(k, v)| value_matches(rec.metadata.get(k), v))
}

fn us(t: Instant) -> u64 {
    t.elapsed().as_micros() as u64
}

/// The state a relevance gate judges: one query and one passage.
pub fn relevance_state(query: &str, passage: &str) -> String {
    format!("Query: {query}\nPassage: {passage}")
}

/// The state a contradiction check judges: two passages.
pub fn contradiction_state(a: &str, b: &str) -> String {
    format!("Passage A: {a}\nPassage B: {b}")
}

/// Passages numbered `[1]`, `[2]`, ... for a joint gate state.
fn numbered<'a>(passages: impl IntoIterator<Item = &'a str>) -> String {
    passages
        .into_iter()
        .enumerate()
        .map(|(i, p)| format!("Passage [{}]: {p}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The state a joint relevance gate judges: one query and every passage, numbered.
pub fn joint_relevance_state<'a>(
    query: &str,
    passages: impl IntoIterator<Item = &'a str>,
) -> String {
    format!("Query: {query}\n\n{}", numbered(passages))
}

/// One gate verdict: `Some(yes)` when the model answered with enough confidence, else `None`.
fn verdict(gate: &DecisionGate, a: &Answer) -> Option<bool> {
    match a {
        Answer::YesNo { yes, confidence } if *confidence >= gate.min_confidence => Some(*yes),
        _ => None,
    }
}

fn gate_failed(gate: &DecisionGate, e: &rag2_core::Error, warnings: &mut Vec<String>) {
    warnings.push(format!(
        "decision model {} failed ({e}); gate skipped",
        gate.model
    ));
}

/// Ask the gate about `n` items, per item or jointly as configured. `None` (model missing,
/// failed or unsure) keeps the item: a broken gate must not silently delete evidence.
fn ask_gate(
    comps: &Components,
    gate: &DecisionGate,
    n: usize,
    per_item: impl FnOnce() -> Vec<String>,
    joint: impl FnOnce() -> (String, Vec<String>),
    warnings: &mut Vec<String>,
) -> Vec<Option<bool>> {
    if n == 0 {
        return Vec::new();
    }
    let Some(m) = comps.decision_models.get(&gate.model) else {
        warnings.push(format!(
            "decision model {} not loaded; gate skipped",
            gate.model
        ));
        return vec![None; n];
    };
    match gate.mode {
        GateMode::PerItem => {
            let q = [Question::YesNo {
                instructions: gate.instructions.clone(),
            }];
            let mut failed = None;
            let out = m
                .decide_each(&per_item(), &q)
                .into_iter()
                .map(|r| match r.and_then(|a| check_answers(&q, &a).map(|_| a)) {
                    Ok(a) => a.first().and_then(|a| verdict(gate, a)),
                    Err(e) => {
                        failed.get_or_insert(e);
                        None
                    }
                })
                .collect();
            if let Some(e) = failed {
                gate_failed(gate, &e, warnings);
            }
            out
        }
        GateMode::Joint => {
            let (state, about) = joint();
            let q: Vec<Question> = about
                .iter()
                .map(|a| Question::YesNo {
                    instructions: format!("{a}: {}", gate.instructions),
                })
                .collect();
            match m
                .decide(&state, &q)
                .and_then(|a| check_answers(&q, &a).map(|_| a))
            {
                Ok(a) => a.iter().map(|a| verdict(gate, a)).collect(),
                Err(e) => {
                    gate_failed(gate, &e, warnings);
                    vec![None; n]
                }
            }
        }
    }
}

impl Engine {
    pub fn query(&self, t: &TenantId, req: QueryRequest) -> Result<QueryResponse> {
        let t_total = Instant::now();
        let active = self.config.current();
        self.check_tenant(&active, t)?;
        let p: &Profile = &active.profile;
        let q = req.query.trim();
        if q.is_empty() {
            return Err(Error::InvalidInput("query must not be empty".into()));
        }
        if q.chars().count() > p.limits.max_query_chars {
            return Err(Error::InvalidInput(format!(
                "query exceeds limits.max_query_chars ({})",
                p.limits.max_query_chars
            )));
        }
        let name = req
            .pipeline
            .clone()
            .unwrap_or_else(|| p.pipeline_for(t).to_string());
        let pl = p
            .pipelines
            .get(&name)
            .ok_or_else(|| Error::InvalidInput(format!("no pipeline named {name:?}")))?;
        let top_k = req.top_k.unwrap_or(pl.top_k);
        if top_k == 0 || top_k > p.limits.max_top_k {
            return Err(Error::InvalidInput(format!(
                "top_k must be in 1..={}",
                p.limits.max_top_k
            )));
        }
        let hops = req
            .hops
            .unwrap_or(pl.multi_hop.extra_hops)
            .min(pl.multi_hop.max_hops);
        // Both generations: storage (dense, documents) and what the lexical reader has committed,
        // so a response computed before a pending lexical commit is never cached past it.
        let generation = self.storage.generation(t)?;
        let lexical_generation = self.lexical.get(t)?.generation();
        let key = {
            let mut h = xxhash_rust::xxh3::Xxh3::new();
            for part in [
                t.as_str().as_bytes(),
                name.as_bytes(),
                &generation.to_le_bytes(),
                &lexical_generation.to_le_bytes(),
                &active.version.to_le_bytes(),
            ] {
                h.update(&(part.len() as u64).to_le_bytes());
                h.update(part);
            }
            h.update(serde_json::to_string(&req).unwrap_or_default().as_bytes());
            h.digest()
        };
        if !req.no_cache
            && let Some(hit) = self.cache.get(&key)
        {
            let mut r = (*hit).clone();
            r.cached = true;
            r.timings.total_us = us(t_total);
            return Ok(r);
        }

        let comps = self.components();
        let mut warnings = Vec::new();
        let mut timings = StageTimings::default();
        let t_route = Instant::now();
        let rewritten = self.rewrite_query(t, pl, q, &comps, &mut warnings);
        let routed_req;
        let req_for_route = match &rewritten {
            Some(r) => {
                routed_req = QueryRequest {
                    query: r.clone(),
                    ..req.clone()
                };
                &routed_req
            }
            None => &req,
        };
        let q: &str = rewritten.as_deref().unwrap_or(q);
        let route = route::decide(pl, req_for_route, &comps, &mut warnings);
        timings.route_us = us(t_route);

        let mut docs = Docs {
            engine: self,
            tenant: t,
            map: HashMap::new(),
        };
        let mut lists: Vec<RankedList> = Vec::new();
        let mut ran: BTreeSet<RetrieverKind> = BTreeSet::new();
        let mut text = q.to_string();
        let mut fused: Vec<Fused> = Vec::new();
        let t_retrieve = Instant::now();
        let mut fuse_us = 0;
        for hop in 0..=hops {
            for r in pl
                .retrievers
                .iter()
                .filter(|r| r.enabled && route.retrievers.contains(&r.kind))
            {
                if r.kind == RetrieverKind::Graph {
                    continue;
                }
                let hits = self.retrieve(t, r, &text, &req.filters, &comps, &mut warnings)?;
                if hop == 0 && !hits.is_empty() {
                    ran.insert(r.kind);
                }
                lists.push(RankedList {
                    retriever: r.kind,
                    hop,
                    weight: r.weight,
                    hits,
                });
            }
            let tf = Instant::now();
            fused = fuse::fuse(&lists, &pl.fusion);
            fuse_us += us(tf);
            if hop < hops {
                let next = self.expansion(t, q, &fused, pl, &mut docs)?;
                if next == text {
                    break;
                }
                text = next;
            }
        }
        if let Some(g) = pl
            .retrievers
            .iter()
            .find(|r| r.enabled && r.kind == RetrieverKind::Graph)
            && route.retrievers.contains(&RetrieverKind::Graph)
        {
            let hits = self.graph(t, &fused, pl, g.top_k, &mut docs)?;
            if !hits.is_empty() {
                ran.insert(RetrieverKind::Graph);
            }
            lists.push(RankedList {
                retriever: RetrieverKind::Graph,
                hop: 0,
                weight: g.weight,
                hits,
            });
            let tf = Instant::now();
            fused = fuse::fuse(&lists, &pl.fusion);
            fuse_us += us(tf);
        }
        timings.retrieve_us = us(t_retrieve).saturating_sub(fuse_us);
        timings.fuse_us = fuse_us;

        let t_analyze = Instant::now();
        let pool = top_k.saturating_mul(pl.fusion.candidate_pool_factor);
        let mut candidates: Vec<(Evidence, u64)> = Vec::with_capacity(pool);
        for f in fused.into_iter() {
            if candidates.len() >= pool {
                break;
            }
            let Some(c) = self.chunk(t, f.chunk)? else {
                continue;
            };
            let Some(rec) = docs.get(&c.doc_id)? else {
                continue;
            };
            if !passes_filters(&rec, &req.filters) {
                continue;
            }
            candidates.push((evidence((*c).clone(), &rec, f), c.hash));
        }
        let mut ev = analyze::dedup(candidates, &pl.dedup);
        self.rerank(t, pl, q, &comps, &mut ev, &mut warnings);
        let mut gated_out = false;
        if let Some(gate) = &pl.relevance_gate {
            let before = ev.len();
            ev.truncate(top_k);
            let keep = ask_gate(
                &comps,
                gate,
                ev.len(),
                || ev.iter().map(|e| relevance_state(q, &e.text)).collect(),
                || {
                    (
                        joint_relevance_state(q, ev.iter().map(|e| e.text.as_str())),
                        (1..=ev.len())
                            .map(|i| format!("About passage [{i}]"))
                            .collect(),
                    )
                },
                &mut warnings,
            );
            ev = ev
                .into_iter()
                .zip(keep)
                .filter_map(|(e, k)| (k != Some(false)).then_some(e))
                .collect();
            gated_out = before > 0 && ev.is_empty();
        }
        ev.truncate(top_k);
        let mut contradictions = analyze::contradictions(&ev, &pl.contradiction);
        if let Some(gate) = &pl.contradiction.decision {
            contradictions = confirm(&comps, gate, &ev, contradictions, &mut warnings);
        }
        let detail = confidence::detail(q, &ev, &ran, pl.abstention.coverage_top_n);
        let conf = if ev.is_empty() {
            0.0
        } else {
            confidence::calibrate(detail.raw, &pl.calibration)
        };
        let abstained = if gated_out && pl.abstention.enabled {
            Some(Abstention {
                reason: AbstainReason::DecisionModel,
                detail: format!(
                    "decision model {} judged no passage relevant",
                    pl.relevance_gate.as_ref().map_or("", |g| g.model.as_str())
                ),
            })
        } else {
            abstain(pl, &ev, conf, &contradictions)
        };
        timings.analyze_us = us(t_analyze);

        let t_assemble = Instant::now();
        let context = (req.include_context.unwrap_or(true) && abstained.is_none())
            .then(|| context::assemble(&ev, &pl.context));
        timings.assemble_us = us(t_assemble);
        timings.total_us = us(t_total);
        let resp = QueryResponse {
            evidence: ev,
            confidence: conf,
            confidence_detail: detail,
            abstained,
            contradictions,
            context,
            pipeline: name,
            route,
            hops,
            config_version: active.version,
            timings,
            cached: false,
            warnings,
            query_rewritten: rewritten,
        };
        if !req.no_cache {
            self.cache.insert(key, Arc::new(resp.clone()));
        }
        Ok(resp)
    }

    fn retrieve(
        &self,
        t: &TenantId,
        r: &RetrieverConfig,
        text: &str,
        filters: &Metadata,
        comps: &Components,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Hit>> {
        match r.kind {
            RetrieverKind::Lexical => self.lexical.get(t)?.search(text, r.top_k),
            RetrieverKind::Dense => {
                let Some(name) = &r.embedder else {
                    return Ok(Vec::new());
                };
                let Some(emb) = comps.embedders.get(name) else {
                    warnings.push(format!(
                        "embedder {name} not loaded; dense retrieval skipped"
                    ));
                    return Ok(Vec::new());
                };
                let Some(ix) = self.tenant_vectors(t)?.get(name) else {
                    return Ok(Vec::new());
                };
                if ix.dim() != emb.dim() {
                    warnings.push(format!("vectors for {name} have dim {} but the embedder has {}; reindex the tenant", ix.dim(), emb.dim()));
                    return Ok(Vec::new());
                }
                let v = emb.embed_query(text)?;
                ix.search(&v, r.top_k, r.ef_search)
            }
            RetrieverKind::Structured => self.structured(t, filters, r.top_k),
            RetrieverKind::Graph => Ok(Vec::new()),
        }
    }

    /// Chunks of documents matching every filter, in document order; score 1.
    fn structured(&self, t: &TenantId, filters: &Metadata, k: usize) -> Result<Vec<Hit>> {
        let mut docs: Option<BTreeSet<DocumentId>> = None;
        for (field, v) in filters {
            let values: Vec<&serde_json::Value> = match v {
                serde_json::Value::Array(a) => a.iter().collect(),
                x => vec![x],
            };
            let mut ids = BTreeSet::new();
            for val in values {
                ids.extend(self.storage.documents_with(t, field, val, k)?);
            }
            docs = Some(match docs {
                Some(prev) => prev.intersection(&ids).cloned().collect(),
                None => ids,
            });
        }
        let mut hits = Vec::new();
        for id in docs.unwrap_or_default() {
            if let Some(rec) = self.record(t, &id)? {
                hits.extend(rec.chunks.iter().map(|c| Hit {
                    chunk: *c,
                    score: 1.0,
                }));
            }
            if hits.len() >= k {
                break;
            }
        }
        hits.truncate(k);
        Ok(hits)
    }

    /// Neighbours of the best results: adjacent chunks and chunks of linked documents.
    fn graph(
        &self,
        t: &TenantId,
        fused: &[Fused],
        pl: &Pipeline,
        k: usize,
        docs: &mut Docs,
    ) -> Result<Vec<Hit>> {
        let g = &pl.graph;
        let mut out: HashMap<ChunkId, f32> = HashMap::new();
        let seeds: HashSet<ChunkId> = fused.iter().take(g.seeds).map(|f| f.chunk).collect();
        for f in fused.iter().take(g.seeds) {
            let Some(c) = self.chunk(t, f.chunk)? else {
                continue;
            };
            let Some(rec) = docs.get(&c.doc_id)? else {
                continue;
            };
            for d in 1..=g.neighbors {
                let s = f.score * g.decay.powi(d as i32);
                for ord in [c.ordinal.checked_sub(d), c.ordinal.checked_add(d)]
                    .into_iter()
                    .flatten()
                {
                    if let Some(id) = rec.chunks.get(ord as usize) {
                        let e = out.entry(*id).or_default();
                        *e = e.max(s);
                    }
                }
            }
            if let Some(serde_json::Value::Array(links)) = rec.metadata.get(&g.link_field) {
                for l in links.iter().filter_map(|v| v.as_str()) {
                    let Ok(lid) = DocumentId::new(l) else {
                        continue;
                    };
                    if let Some(linked) = docs.get(&lid)?
                        && let Some(first) = linked.chunks.first()
                    {
                        let e = out.entry(*first).or_default();
                        *e = e.max(f.score * g.decay);
                    }
                }
            }
        }
        let mut hits: Vec<Hit> = out
            .into_iter()
            .filter(|(c, _)| !seeds.contains(c))
            .map(|(chunk, score)| Hit { chunk, score })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.cmp(&b.chunk)));
        hits.truncate(k);
        Ok(hits)
    }

    /// The next hop's query: the original plus the most salient new terms of the top evidence
    /// (term frequency times inverse document frequency from the tenant's lexical index).
    fn expansion(
        &self,
        t: &TenantId,
        q: &str,
        fused: &[Fused],
        pl: &Pipeline,
        docs: &mut Docs,
    ) -> Result<String> {
        let known = content_words(q);
        let lex = self.lexical.get(t)?;
        let mut tf: HashMap<String, u32> = HashMap::new();
        for f in fused.iter().take(pl.abstention.coverage_top_n) {
            let Some(c) = self.chunk(t, f.chunk)? else {
                continue;
            };
            if docs.get(&c.doc_id)?.is_none() {
                continue;
            }
            for w in words(&c.text).filter(|w| !known.contains(w) && content_words(w).contains(w)) {
                *tf.entry(w).or_default() += 1;
            }
        }
        let mut scored: Vec<(f64, String)> = tf
            .into_iter()
            .map(|(w, n)| {
                let (df, total) = lex.doc_freq(&w).unwrap_or((0, 0));
                let idf = ((total as f64 + 1.0) / (df as f64 + 1.0)).ln();
                (f64::from(n) * idf, w)
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let terms: Vec<String> = scored
            .into_iter()
            .take(pl.multi_hop.expansion_terms)
            .map(|(_, w)| w)
            .collect();
        Ok(if terms.is_empty() {
            q.to_string()
        } else {
            format!("{q} {}", terms.join(" "))
        })
    }
}

fn evidence(c: Chunk, rec: &DocumentRecord, f: Fused) -> Evidence {
    Evidence {
        chunk_id: c.id,
        score: f.score,
        signals: f.signals,
        provenance: Provenance {
            doc_id: c.doc_id,
            doc_version: rec.version,
            chunk_ordinal: c.ordinal,
            start: c.start,
            end: c.end,
            source: rec.source.clone(),
            title: rec.title.clone(),
        },
        text: c.text,
    }
}

fn confirm(
    comps: &Components,
    gate: &DecisionGate,
    ev: &[Evidence],
    found: Vec<Contradiction>,
    warnings: &mut Vec<String>,
) -> Vec<Contradiction> {
    let text = |id: ChunkId| {
        ev.iter()
            .find(|e| e.chunk_id == id)
            .map(|e| e.text.as_str())
            .unwrap_or("")
    };
    // Joint: only the passages some pair mentions, numbered in evidence order.
    let involved: Vec<&Evidence> = ev
        .iter()
        .filter(|e| found.iter().any(|c| c.a == e.chunk_id || c.b == e.chunk_id))
        .collect();
    let num = |id: ChunkId| {
        involved
            .iter()
            .position(|e| e.chunk_id == id)
            .map_or(0, |i| i + 1)
    };
    let keep = ask_gate(
        comps,
        gate,
        found.len(),
        || {
            found
                .iter()
                .map(|c| contradiction_state(text(c.a), text(c.b)))
                .collect()
        },
        || {
            (
                numbered(involved.iter().map(|e| e.text.as_str())),
                found
                    .iter()
                    .map(|c| format!("About passages [{}] and [{}]", num(c.a), num(c.b)))
                    .collect(),
            )
        },
        warnings,
    );
    found
        .into_iter()
        .zip(keep)
        .filter_map(|(c, k)| (k != Some(false)).then_some(c))
        .collect()
}

fn abstain(
    pl: &Pipeline,
    ev: &[Evidence],
    conf: f32,
    contradictions: &[Contradiction],
) -> Option<Abstention> {
    let a = &pl.abstention;
    if !a.enabled {
        return None;
    }
    if ev.len() < a.min_evidence {
        return Some(Abstention {
            reason: AbstainReason::NoEvidence,
            detail: format!(
                "{} evidence passages, {} required",
                ev.len(),
                a.min_evidence
            ),
        });
    }
    if conf < a.min_confidence {
        return Some(Abstention {
            reason: AbstainReason::LowConfidence,
            detail: format!("confidence {conf:.3} below {:.3}", a.min_confidence),
        });
    }
    if a.on_contradiction && !contradictions.is_empty() {
        return Some(Abstention {
            reason: AbstainReason::Contradictory,
            detail: format!("{} contradictions in the evidence", contradictions.len()),
        });
    }
    None
}

impl Engine {
    /// Apply the pipeline's `query_rewrite` plugins in order. A failing plugin is skipped with a
    /// warning: rewriting improves recall but must never make a query fail.
    fn rewrite_query(
        &self,
        t: &TenantId,
        pl: &Pipeline,
        q: &str,
        comps: &Components,
        warnings: &mut Vec<String>,
    ) -> Option<String> {
        let mut cur = q.to_string();
        for name in &pl.plugins.query_rewrite {
            let Some(p) = comps.plugins.get(name) else {
                warnings.push(format!("plugin {name} not loaded; rewrite skipped"));
                continue;
            };
            match p.call(
                rag2_core::interfaces::hooks::QUERY_REWRITE,
                t.as_str(),
                &serde_json::json!({ "query": cur }),
            ) {
                Ok(v) => match v["query"].as_str().map(str::trim) {
                    Some(n) if !n.is_empty() => cur = n.to_string(),
                    _ => warnings.push(format!("plugin {name} returned no query; ignored")),
                },
                Err(e) => warnings.push(format!("plugin {name} failed ({e}); rewrite skipped")),
            }
        }
        (cur != q).then_some(cur)
    }

    /// Rescore candidates with the pipeline's `rerank` plugin; on failure keep the fused order.
    fn rerank(
        &self,
        t: &TenantId,
        pl: &Pipeline,
        q: &str,
        comps: &Components,
        ev: &mut [Evidence],
        warnings: &mut Vec<String>,
    ) {
        let Some(name) = &pl.plugins.rerank else {
            return;
        };
        if ev.is_empty() {
            return;
        }
        let Some(p) = comps.plugins.get(name) else {
            warnings.push(format!("plugin {name} not loaded; rerank skipped"));
            return;
        };
        let passages: Vec<&str> = ev.iter().map(|e| e.text.as_str()).collect();
        let input = serde_json::json!({ "query": q, "passages": passages });
        let scores: Option<Vec<f32>> =
            match p.call(rag2_core::interfaces::hooks::RERANK, t.as_str(), &input) {
                Ok(v) => serde_json::from_value(v["scores"].clone())
                    .ok()
                    .filter(|s: &Vec<f32>| s.len() == ev.len() && s.iter().all(|x| x.is_finite())),
                Err(e) => {
                    warnings.push(format!("plugin {name} failed ({e}); rerank skipped"));
                    return;
                }
            };
        let Some(scores) = scores else {
            warnings.push(format!(
                "plugin {name} returned unusable scores; rerank skipped"
            ));
            return;
        };
        for (e, s) in ev.iter_mut().zip(scores) {
            e.score = s;
        }
        ev.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.chunk_id.cmp(&b.chunk_id))
        });
    }
}
