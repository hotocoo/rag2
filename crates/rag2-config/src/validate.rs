//! Semantic validation: what serde cannot check. Every issue carries the path of the offending
//! field so the console can highlight it and the CLI can print it.

use crate::schema::*;
use rag2_core::model::RetrieverKind;
use serde::Serialize;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Issue {
    pub path: String,
    pub message: String,
    pub severity: Severity,
}

/// Placeholders the context template may use.
pub const TEMPLATE_PLACEHOLDERS: &[&str] = &["n", "text", "doc_id", "title", "source", "score"];
/// `hnsw_rs` refuses `m` above this.
const HNSW_MAX_M: usize = 256;

struct V(Vec<Issue>);

impl V {
    fn err(&mut self, path: impl Into<String>, msg: impl Into<String>) {
        self.0.push(Issue {
            path: path.into(),
            message: msg.into(),
            severity: Severity::Error,
        });
    }
    fn warn(&mut self, path: impl Into<String>, msg: impl Into<String>) {
        self.0.push(Issue {
            path: path.into(),
            message: msg.into(),
            severity: Severity::Warning,
        });
    }
    fn unit(&mut self, path: impl Into<String>, x: f32) {
        if !(0.0..=1.0).contains(&x) {
            self.err(path, format!("must be in [0, 1], got {x}"));
        }
    }
    fn positive(&mut self, path: impl Into<String>, x: usize) {
        if x == 0 {
            self.err(path, "must be greater than 0");
        }
    }
}

/// All issues in `p`. The profile is activatable when none has severity `Error`.
pub fn validate(p: &Profile) -> Vec<Issue> {
    let mut v = V(Vec::new());
    if p.api_version != API_VERSION {
        v.err(
            "api_version",
            format!("must be {API_VERSION:?}, got {:?}", p.api_version),
        );
    }
    if p.metadata.name.trim().is_empty() {
        v.err("metadata.name", "must not be empty");
    }
    server(&mut v, p);
    limits(&mut v, p);
    indexes(&mut v, p);
    for (name, e) in &p.embedders {
        if rag2_core::TenantId::new(name).is_err() {
            v.err(
                format!("embedders.{name}"),
                "embedder names use [a-z0-9_-], 1..=64 chars (they key stored vectors)",
            );
        }
        embedder(&mut v, name, e);
    }
    for (name, d) in &p.decision_models {
        decision_model(&mut v, name, d);
    }
    if p.pipelines.is_empty() {
        v.err("pipelines", "at least one pipeline is required");
    }
    if !p.pipelines.contains_key(&p.default_pipeline) {
        v.err(
            "default_pipeline",
            format!("no pipeline named {:?}", p.default_pipeline),
        );
    }
    for (name, pl) in &p.pipelines {
        pipeline(&mut v, p, name, pl);
    }
    security(&mut v, p);
    tenants(&mut v, p);
    cluster(&mut v, p);
    plugins(&mut v, p);
    connectors(&mut v, p);
    let mut names = std::collections::BTreeSet::new();
    for (i, w) in p.webhooks.iter().enumerate() {
        let at = |f: &str| format!("webhooks[{i}].{f}");
        if w.name.trim().is_empty() || !names.insert(&w.name) {
            v.err(at("name"), "webhook names must be non-empty and unique");
        }
        http_endpoint(&mut v, at("url"), &w.url);
        if w.events.is_empty() {
            v.err(at("events"), "subscribe to at least one event");
        }
        if w.secret.is_none() {
            v.warn(
                at("secret"),
                "unsigned webhooks cannot be verified by the receiver",
            );
        }
        v.positive(at("timeout_ms"), w.timeout_ms as usize);
    }
    v.0
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.severity == Severity::Error)
}

fn server(v: &mut V, p: &Profile) {
    let s = &p.server;
    if s.listen.parse::<SocketAddr>().is_err() {
        v.err(
            "server.listen",
            format!("{:?} is not an ip:port address", s.listen),
        );
    }
    if let Some(g) = &s.grpc_listen
        && g.parse::<SocketAddr>().is_err()
    {
        v.err(
            "server.grpc_listen",
            format!("{g:?} is not an ip:port address"),
        );
    }
    v.positive("server.request_timeout_ms", s.request_timeout_ms as usize);
    v.positive("server.max_body_bytes", s.max_body_bytes);
    v.positive("server.max_concurrent_requests", s.max_concurrent_requests);
    if s.cors_origins.iter().any(|o| o == "*") {
        v.warn(
            "server.cors_origins",
            "\"*\" lets any website call the API from a browser",
        );
    }
}

fn limits(v: &mut V, p: &Profile) {
    let l = &p.limits;
    v.positive("limits.max_top_k", l.max_top_k);
    v.positive("limits.max_query_chars", l.max_query_chars);
    v.positive("limits.max_document_bytes", l.max_document_bytes);
    v.positive("limits.max_batch_documents", l.max_batch_documents);
    v.positive("limits.ingest_queue_depth", l.ingest_queue_depth);
    v.positive("limits.ingest_workers", l.ingest_workers);
    if l.max_document_bytes > p.server.max_body_bytes {
        v.warn(
            "limits.max_document_bytes",
            "exceeds server.max_body_bytes; such documents can never arrive",
        );
    }
    let c = &p.chunking;
    v.positive("chunking.max_chars", c.max_chars);
    if c.overlap_chars >= c.max_chars {
        v.err(
            "chunking.overlap_chars",
            "must be smaller than chunking.max_chars",
        );
    }
    if c.min_chars >= c.max_chars {
        v.err(
            "chunking.min_chars",
            "must be smaller than chunking.max_chars",
        );
    }
}

fn indexes(v: &mut V, p: &Profile) {
    let x = &p.indexes.vector;
    v.positive(
        "indexes.vector.flat_parallel_chunk_rows",
        x.flat_parallel_chunk_rows,
    );
    if !(2..=HNSW_MAX_M).contains(&x.hnsw_m) {
        v.err(
            "indexes.vector.hnsw_m",
            format!("must be in 2..={HNSW_MAX_M}"),
        );
    }
    v.positive(
        "indexes.vector.hnsw_ef_construction",
        x.hnsw_ef_construction,
    );
    v.positive("indexes.vector.hnsw_ef_search", x.hnsw_ef_search);
    v.unit(
        "indexes.vector.hnsw_max_tombstone_ratio",
        x.hnsw_max_tombstone_ratio,
    );
    v.positive(
        "indexes.lexical.writer_heap_mb",
        p.indexes.lexical.writer_heap_mb,
    );
}

fn http_endpoint(v: &mut V, path: String, url: &str) {
    let ok = url.starts_with("https://") || url.starts_with("http://");
    if !ok {
        v.err(path.clone(), "must be an http:// or https:// URL");
    } else if url.starts_with("http://") && !is_loopback_url(url) {
        v.warn(
            path,
            "plain http to a non-loopback host sends data unencrypted",
        );
    }
}

fn is_loopback_url(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    matches!(host, "localhost" | "127.0.0.1" | "[") || host.starts_with("127.")
}

fn embedder(v: &mut V, name: &str, e: &EmbedderConfig) {
    let at = |f: &str| format!("embedders.{name}.{f}");
    match e {
        EmbedderConfig::Hash { dim } => v.positive(at("dim"), *dim),
        EmbedderConfig::Candle {
            model_dir, batch, ..
        } => {
            if model_dir.trim().is_empty() {
                v.err(at("model_dir"), "must not be empty");
            }
            v.positive(at("batch"), *batch);
        }
        EmbedderConfig::Http {
            endpoint,
            model,
            dim,
            batch,
            timeout_ms,
            ..
        } => {
            http_endpoint(v, at("endpoint"), endpoint);
            if model.trim().is_empty() {
                v.err(at("model"), "must not be empty");
            }
            v.positive(at("dim"), *dim);
            v.positive(at("batch"), *batch);
            v.positive(at("timeout_ms"), *timeout_ms as usize);
        }
    }
}

fn decision_model(v: &mut V, name: &str, d: &DecisionModelConfig) {
    let at = |f: &str| format!("decision_models.{name}.{f}");
    match d {
        DecisionModelConfig::LayaLocal { checkpoint_dir, .. } => {
            if checkpoint_dir.trim().is_empty() {
                v.err(at("checkpoint_dir"), "must not be empty");
            }
        }
        DecisionModelConfig::LayaHttp {
            endpoint,
            serve_id,
            timeout_ms,
        } => {
            http_endpoint(v, at("endpoint"), endpoint);
            if serve_id.trim().is_empty() {
                v.err(at("serve_id"), "must not be empty");
            }
            v.positive(at("timeout_ms"), *timeout_ms as usize);
        }
    }
}

fn gate(v: &mut V, p: &Profile, path: &str, g: &DecisionGate) {
    if !p.decision_models.contains_key(&g.model) {
        v.err(
            format!("{path}.model"),
            format!("no decision model named {:?}", g.model),
        );
    }
    if g.instructions.trim().is_empty() {
        v.err(format!("{path}.instructions"), "must not be empty");
    }
    v.unit(format!("{path}.min_confidence"), g.min_confidence);
}

fn pipeline(v: &mut V, p: &Profile, name: &str, pl: &Pipeline) {
    let at = |f: &str| format!("pipelines.{name}.{f}");
    let enabled: Vec<RetrieverKind> = pl
        .retrievers
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.kind)
        .collect();
    if enabled.is_empty() {
        v.err(
            at("retrievers"),
            "at least one enabled retriever is required",
        );
    }
    for (i, r) in pl.retrievers.iter().enumerate() {
        let rp = format!("pipelines.{name}.retrievers[{i}]");
        v.positive(format!("{rp}.top_k"), r.top_k);
        if !(r.weight.is_finite() && r.weight >= 0.0) {
            v.err(format!("{rp}.weight"), "must be a finite number >= 0");
        }
        match (r.kind, &r.embedder) {
            (RetrieverKind::Dense, None) => v.err(
                format!("{rp}.embedder"),
                "dense retrievers need an embedder",
            ),
            (RetrieverKind::Dense, Some(e)) if !p.embedders.contains_key(e) => {
                v.err(format!("{rp}.embedder"), format!("no embedder named {e:?}"))
            }
            (k, Some(_)) if k != RetrieverKind::Dense => v.warn(
                format!("{rp}.embedder"),
                "ignored: only dense retrievers embed",
            ),
            _ => {}
        }
        if r.top_k > p.limits.max_top_k.max(pl.top_k) * 100 {
            v.warn(
                format!("{rp}.top_k"),
                "very large candidate lists cost latency for little recall",
            );
        }
    }
    let dup = pl.retrievers.iter().filter(|r| r.enabled).count()
        != enabled
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
    if dup {
        v.err(
            at("retrievers"),
            "each retriever kind may appear once among enabled retrievers",
        );
    }
    v.positive(at("top_k"), pl.top_k);
    if pl.top_k > p.limits.max_top_k {
        v.err(
            at("top_k"),
            format!("exceeds limits.max_top_k ({})", p.limits.max_top_k),
        );
    }
    let route_ok = |v: &mut V, path: String, kinds: &[RetrieverKind]| {
        if kinds.is_empty() {
            v.err(path.clone(), "must name at least one retriever");
        }
        for k in kinds {
            if !enabled.contains(k) {
                v.err(
                    path.clone(),
                    format!("{k:?} is not an enabled retriever of this pipeline"),
                );
            }
        }
    };
    match &pl.routing {
        Routing::All => {}
        Routing::Rules { rules, fallback } => {
            for (i, r) in rules.iter().enumerate() {
                route_ok(
                    v,
                    at(&format!("routing.rules[{i}].retrievers")),
                    &r.retrievers,
                );
                if let RouteCondition::Matches(re) = &r.when
                    && let Err(e) = regex::Regex::new(re)
                {
                    v.err(
                        at(&format!("routing.rules[{i}].when.matches")),
                        format!("invalid regex: {e}"),
                    );
                }
            }
            route_ok(v, at("routing.fallback"), fallback);
        }
        Routing::Decision {
            model,
            instructions,
            routes,
            min_confidence,
            fallback,
        } => {
            if !p.decision_models.contains_key(model) {
                v.err(
                    at("routing.model"),
                    format!("no decision model named {model:?}"),
                );
            }
            if instructions.trim().is_empty() {
                v.err(at("routing.instructions"), "must not be empty");
            }
            if routes.len() < 2 {
                v.err(at("routing.routes"), "a choice needs at least two routes");
            }
            for (rn, rc) in routes {
                route_ok(
                    v,
                    at(&format!("routing.routes.{rn}.retrievers")),
                    &rc.retrievers,
                );
            }
            v.unit(at("routing.min_confidence"), *min_confidence);
            route_ok(v, at("routing.fallback"), fallback);
        }
    }
    if !(pl.fusion.rrf_k.is_finite() && pl.fusion.rrf_k > 0.0) {
        v.err(at("fusion.rrf_k"), "must be a finite number > 0");
    }
    v.positive(
        at("fusion.candidate_pool_factor"),
        pl.fusion.candidate_pool_factor,
    );
    v.unit(at("graph.decay"), pl.graph.decay);
    v.positive(
        at("abstention.coverage_top_n"),
        pl.abstention.coverage_top_n,
    );
    v.unit(
        at("dedup.near_duplicate_similarity"),
        pl.dedup.near_duplicate_similarity,
    );
    if pl.multi_hop.extra_hops > pl.multi_hop.max_hops {
        v.err(
            at("multi_hop.extra_hops"),
            "must not exceed multi_hop.max_hops",
        );
    }
    if pl.multi_hop.extra_hops > 0 {
        v.positive(
            at("multi_hop.expansion_terms"),
            pl.multi_hop.expansion_terms,
        );
    }
    v.unit(
        at("contradiction.min_overlap"),
        pl.contradiction.min_overlap,
    );
    if pl.contradiction.numeric_tolerance.is_nan() || pl.contradiction.numeric_tolerance < 0.0 {
        v.err(at("contradiction.numeric_tolerance"), "must be >= 0");
    }
    if let Some(g) = &pl.contradiction.decision {
        gate(v, p, &at("contradiction.decision"), g);
    }
    if let Some(g) = &pl.relevance_gate {
        gate(v, p, &at("relevance_gate"), g);
    }
    if !(pl.calibration.slope.is_finite() && pl.calibration.intercept.is_finite()) {
        v.err(at("calibration"), "slope and intercept must be finite");
    }
    v.unit(
        at("abstention.min_confidence"),
        pl.abstention.min_confidence,
    );
    v.positive(at("context.max_chars"), pl.context.max_chars);
    v.positive(at("context.max_chunks"), pl.context.max_chunks);
    for ph in placeholders(&pl.context.template) {
        if !TEMPLATE_PLACEHOLDERS.contains(&ph) {
            v.err(
                at("context.template"),
                format!("unknown placeholder {{{ph}}}"),
            );
        }
    }
}

/// `{name}` placeholders in a template.
pub fn placeholders(t: &str) -> Vec<&str> {
    t.split('{')
        .skip(1)
        .filter_map(|s| s.split_once('}').map(|(n, _)| n))
        .collect()
}

fn security(v: &mut V, p: &Profile) {
    let s = &p.security;
    let loopback = p
        .server
        .listen
        .parse::<SocketAddr>()
        .is_ok_and(|a| a.ip().is_loopback());
    let keys: &[ApiKey] = match &s.auth {
        AuthConfig::None => {
            if !loopback {
                v.err(
                    "security.auth",
                    "authentication is required when server.listen is not loopback",
                );
            }
            &[]
        }
        AuthConfig::ApiKeys { keys } => {
            if keys.is_empty() {
                v.err("security.auth.keys", "at least one key is required");
            }
            keys
        }
        AuthConfig::Oidc {
            issuer,
            audience,
            jwks_url,
            api_keys,
            ..
        } => {
            if !issuer.starts_with("https://") {
                v.err("security.auth.issuer", "must be an https:// URL");
            }
            if audience.trim().is_empty() {
                v.err("security.auth.audience", "must not be empty");
            }
            if !jwks_url.starts_with("https://") {
                v.err("security.auth.jwks_url", "must be an https:// URL");
            }
            api_keys
        }
    };
    let mut ids = std::collections::BTreeSet::new();
    for (i, k) in keys.iter().enumerate() {
        let at = format!("security.auth.keys[{i}]");
        if !ids.insert(&k.id) {
            v.err(format!("{at}.id"), format!("duplicate key id {:?}", k.id));
        }
        for r in &k.roles {
            if !s.roles.contains_key(r) {
                v.err(format!("{at}.roles"), format!("no role named {r:?}"));
            }
        }
        if k.tenants.is_empty() {
            v.err(
                format!("{at}.tenants"),
                "list tenants explicitly, or [\"*\"] for all",
            );
        }
        for t in &k.tenants {
            if t != "*" && rag2_core::TenantId::new(t).is_err() {
                v.err(
                    format!("{at}.tenants"),
                    format!("{t:?} is not a valid tenant id"),
                );
            }
        }
    }
    if p.server.tls.is_none() && !loopback {
        v.warn("server.tls", "serving without TLS on a non-loopback address; terminate TLS in front or set server.tls");
    }
}

fn plugins(v: &mut V, p: &Profile) {
    use rag2_core::interfaces::hooks;
    for (name, pc) in &p.plugins {
        let at = |f: &str| format!("plugins.{name}.{f}");
        if pc.path.trim().is_empty() {
            v.err(at("path"), "must not be empty");
        }
        if pc.hooks.is_empty() {
            v.err(at("hooks"), "grant at least one hook");
        }
        for h in &pc.hooks {
            if !hooks::ALL.contains(&h.as_str()) {
                v.err(
                    at("hooks"),
                    format!("unknown hook {h:?}; known: {:?}", hooks::ALL),
                );
            }
        }
        if let Some(sum) = &pc.sha256
            && (sum.len() != 64 || !sum.chars().all(|c| c.is_ascii_hexdigit()))
        {
            v.err(at("sha256"), "must be 64 hex characters");
        }
        v.positive(at("fuel"), pc.fuel as usize);
        v.positive(at("memory_mb"), pc.memory_mb as usize);
        v.positive(at("timeout_ms"), pc.timeout_ms as usize);
    }
    let uses = |v: &mut V, path: String, name: &str, hook: &str| match p.plugins.get(name) {
        None => v.err(path, format!("no plugin named {name:?}")),
        Some(pc) if !pc.hooks.iter().any(|h| h == hook) => v.err(
            path,
            format!("plugin {name} is not granted the {hook} hook"),
        ),
        Some(_) => {}
    };
    for (i, n) in p.ingest_plugins.iter().enumerate() {
        uses(
            v,
            format!("ingest_plugins[{i}]"),
            n,
            hooks::DOCUMENT_TRANSFORM,
        );
    }
    for (pn, pl) in &p.pipelines {
        for (i, n) in pl.plugins.query_rewrite.iter().enumerate() {
            uses(
                v,
                format!("pipelines.{pn}.plugins.query_rewrite[{i}]"),
                n,
                hooks::QUERY_REWRITE,
            );
        }
        if let Some(n) = &pl.plugins.rerank {
            uses(
                v,
                format!("pipelines.{pn}.plugins.rerank"),
                n,
                hooks::RERANK,
            );
        }
    }
}

fn connectors(v: &mut V, p: &Profile) {
    for (name, c) in &p.connectors {
        let at = |f: &str| format!("connectors.{name}.{f}");
        if rag2_core::TenantId::new(name).is_err() {
            v.err(
                format!("connectors.{name}"),
                "connector names use [a-z0-9_-], 1..=64 chars",
            );
        }
        match &c.source {
            SourceConfig::Filesystem { path, extensions } => {
                if path.trim().is_empty() {
                    v.err(at("source.path"), "must not be empty");
                }
                if extensions.is_empty() {
                    v.err(at("source.extensions"), "list at least one extension");
                }
            }
            SourceConfig::Web {
                urls, timeout_ms, ..
            } => {
                if urls.is_empty() {
                    v.err(at("source.urls"), "list at least one URL");
                }
                for (i, u) in urls.iter().enumerate() {
                    http_endpoint(v, at(&format!("source.urls[{i}]")), u);
                }
                v.positive(at("source.timeout_ms"), *timeout_ms as usize);
            }
            SourceConfig::HttpJson {
                url,
                id,
                text,
                timeout_ms,
                ..
            } => {
                http_endpoint(v, at("source.url"), url);
                for (f, ptr) in [("id", id), ("text", text)] {
                    if !ptr.starts_with('/') {
                        v.err(
                            at(&format!("source.{f}")),
                            "must be a JSON Pointer starting with /",
                        );
                    }
                }
                v.positive(at("source.timeout_ms"), *timeout_ms as usize);
            }
        }
    }
}

fn cluster(v: &mut V, p: &Profile) {
    let c = &p.cluster;
    if c.node_id.trim().is_empty() {
        v.err("cluster.node_id", "must not be empty");
    }
    match c.role {
        ClusterRole::Standalone => {}
        ClusterRole::Primary | ClusterRole::Replica => {
            if c.replication_key.is_none() {
                v.err(
                    "cluster.replication_key",
                    "primaries and replicas need a shared replication key",
                );
            }
            v.positive("cluster.batch", c.batch);
            v.positive("cluster.poll_interval_ms", c.poll_interval_ms as usize);
        }
    }
    if !c.tenant_routes.is_empty() {
        match &c.self_url {
            None => v.err("cluster.self_url", "required when tenant_routes is set"),
            Some(u) => http_endpoint(v, "cluster.self_url".into(), u),
        }
        for (t, u) in &c.tenant_routes {
            if t != "*" && rag2_core::TenantId::new(t).is_err() {
                v.err(
                    format!("cluster.tenant_routes.{t}"),
                    "not a valid tenant id or *",
                );
            }
            http_endpoint(v, format!("cluster.tenant_routes.{t}"), u);
        }
    }
    if c.role == ClusterRole::Replica {
        match &c.primary_url {
            None => v.err("cluster.primary_url", "replicas need the primary's URL"),
            Some(u) => http_endpoint(v, "cluster.primary_url".into(), u),
        }
    }
}

fn tenants(v: &mut V, p: &Profile) {
    let check = |v: &mut V, path: String, t: &TenantConfig| {
        if let Some(pl) = &t.pipeline
            && !p.pipelines.contains_key(pl)
        {
            v.err(
                format!("{path}.pipeline"),
                format!("no pipeline named {pl:?}"),
            );
        }
        v.positive(
            format!("{path}.requests_per_second"),
            t.requests_per_second as usize,
        );
        if t.burst < t.requests_per_second {
            v.warn(
                format!("{path}.burst"),
                "burst below requests_per_second throttles steady traffic",
            );
        }
    };
    check(v, "tenant_defaults".into(), &p.tenant_defaults);
    for (id, t) in &p.tenants {
        check(v, format!("tenants.{id}"), t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(p: &Profile) -> Vec<String> {
        validate(p)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.path)
            .collect()
    }

    #[test]
    fn the_minimal_profile_is_valid() {
        assert_eq!(errors(&Profile::minimal("t")), Vec::<String>::new());
    }

    #[test]
    fn dangling_references_are_reported_by_path() {
        let mut p = Profile::minimal("t");
        p.default_pipeline = "nope".into();
        let pl = p.pipelines.get_mut("default").expect("default pipeline");
        pl.retrievers[0].embedder = Some("missing".into());
        pl.relevance_gate = Some(DecisionGate {
            model: "laya".into(),
            instructions: "x".into(),
            min_confidence: 0.9,
        });
        let e = errors(&p);
        assert!(e.contains(&"default_pipeline".to_string()));
        assert!(e.contains(&"pipelines.default.retrievers[0].embedder".to_string()));
        assert!(e.contains(&"pipelines.default.relevance_gate.model".to_string()));
    }

    #[test]
    fn out_of_range_thresholds_and_bad_templates_are_errors() {
        let mut p = Profile::minimal("t");
        let pl = p.pipelines.get_mut("default").expect("default pipeline");
        pl.abstention.min_confidence = 1.5;
        pl.context.template = "{n} {secret}".into();
        pl.top_k = 10_000;
        p.chunking.overlap_chars = p.chunking.max_chars;
        let e = errors(&p);
        for path in [
            "pipelines.default.abstention.min_confidence",
            "pipelines.default.context.template",
            "pipelines.default.top_k",
            "chunking.overlap_chars",
        ] {
            assert!(e.contains(&path.to_string()), "{path} missing from {e:?}");
        }
    }

    #[test]
    fn no_auth_on_a_public_address_is_refused() {
        let mut p = Profile::minimal("t");
        p.server.listen = "0.0.0.0:8080".into();
        assert!(errors(&p).contains(&"security.auth".to_string()));
    }

    #[test]
    fn routes_must_use_enabled_retrievers_and_regexes_must_compile() {
        let mut p = Profile::minimal("t");
        let pl = p.pipelines.get_mut("default").expect("default pipeline");
        pl.routing = Routing::Rules {
            rules: vec![RouteRule {
                when: RouteCondition::Matches("(".into()),
                retrievers: vec![RetrieverKind::Graph],
            }],
            fallback: vec![RetrieverKind::Lexical],
        };
        let e = errors(&p);
        assert!(e.contains(&"pipelines.default.routing.rules[0].retrievers".to_string()));
        assert!(e.contains(&"pipelines.default.routing.rules[0].when.matches".to_string()));
    }
}
