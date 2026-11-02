//! gRPC API (`rag2.v1.Rag2`, see `proto/rag2.proto`) on `server.grpc_listen`. Same engine,
//! authentication, tenant scoping, rate limits, audit and webhooks as REST.

use crate::state::AppState;
use axum::http::HeaderMap;
use rag2_config::schema::Permission;
use rag2_core::model::{Document, Metadata};
use rag2_core::{DocumentId, Error, TenantId};
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub mod pb {
    tonic::include_proto!("rag2.v1");
}

use pb::rag2_server::{Rag2, Rag2Server};

pub fn status(e: Error) -> Status {
    match e {
        Error::InvalidInput(m) => Status::invalid_argument(m),
        Error::NotFound(m) => Status::not_found(m),
        Error::Conflict(m) => Status::aborted(m),
        Error::Unauthenticated(m) => Status::unauthenticated(m),
        Error::Forbidden(m) => Status::permission_denied(m),
        Error::RateLimited(m) => Status::resource_exhausted(m),
        Error::Overloaded(m) | Error::Unavailable(m) => Status::unavailable(m),
        Error::Storage(m) | Error::Internal(m) => {
            tracing::error!(error = %m, "grpc request failed");
            Status::internal("internal error; see server logs")
        }
    }
}

pub struct Service {
    state: Arc<AppState>,
}

impl Service {
    async fn authorize<T>(
        &self,
        req: &Request<T>,
        tenant: &str,
        p: Permission,
    ) -> Result<(crate::auth::Principal, TenantId), Status> {
        let mut h = HeaderMap::new();
        for key in ["authorization", crate::auth::API_KEY_HEADER] {
            if let Some(v) = req
                .metadata()
                .get(key)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
            {
                h.insert(key, v);
            }
        }
        let who = self
            .state
            .auth
            .load_full()
            .authenticate(&h)
            .await
            .map_err(status)?;
        let t = TenantId::new(tenant).map_err(status)?;
        who.require_tenant(p, &t).map_err(status)?;
        self.state.admit(&t).map_err(status)?;
        Ok((who, t))
    }

    async fn run<T: Send + 'static>(
        f: impl FnOnce() -> rag2_core::Result<T> + Send + 'static,
    ) -> Result<T, Status> {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .map_err(status)
    }
}

fn json_map(s: &str) -> Result<Metadata, Status> {
    if s.trim().is_empty() {
        return Ok(Metadata::new());
    }
    serde_json::from_str(s)
        .map_err(|e| Status::invalid_argument(format!("expected a JSON object: {e}")))
}

fn wire<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

#[tonic::async_trait]
impl Rag2 for Service {
    async fn query(
        &self,
        req: Request<pb::QueryRequest>,
    ) -> Result<Response<pb::QueryResponse>, Status> {
        let (_, t) = self
            .authorize(&req, &req.get_ref().tenant.clone(), Permission::Query)
            .await?;
        let r = req.into_inner();
        let mut q = rag2_engine::QueryRequest::new(r.query);
        q.top_k = r.top_k.map(|k| k as usize);
        q.pipeline = r.pipeline;
        q.filters = json_map(&r.filters_json)?;
        q.hops = r.hops;
        q.include_context = Some(r.include_context);
        let e = self.state.engine.clone();
        let resp = Self::run(move || e.query(&t, q)).await?;
        Ok(Response::new(pb::QueryResponse {
            evidence: resp
                .evidence
                .into_iter()
                .map(|ev| pb::Evidence {
                    chunk_id: ev.chunk_id.0,
                    text: ev.text,
                    score: ev.score,
                    signals: ev
                        .signals
                        .iter()
                        .map(|s| pb::Signal {
                            retriever: wire(&s.retriever),
                            rank: s.rank,
                            score: s.score,
                            hop: s.hop,
                        })
                        .collect(),
                    doc_id: ev.provenance.doc_id.to_string(),
                    doc_version: ev.provenance.doc_version,
                    chunk_ordinal: ev.provenance.chunk_ordinal,
                    start: ev.provenance.start,
                    end: ev.provenance.end,
                    source: ev.provenance.source,
                    title: ev.provenance.title,
                })
                .collect(),
            confidence: resp.confidence,
            abstained_reason: resp.abstained.as_ref().map(|a| wire(&a.reason)),
            abstained_detail: resp.abstained.map(|a| a.detail),
            contradictions: resp
                .contradictions
                .into_iter()
                .map(|c| pb::Contradiction {
                    a: c.a.0,
                    b: c.b.0,
                    detector: c.detector,
                    reason: c.reason,
                    confidence: c.confidence,
                })
                .collect(),
            context: resp.context,
            pipeline: resp.pipeline,
            route: resp.route.retrievers.iter().map(wire).collect(),
            route_reason: resp.route.reason,
            total_us: resp.timings.total_us,
            cached: resp.cached,
            warnings: resp.warnings,
            config_version: resp.config_version,
        }))
    }

    async fn ingest(
        &self,
        req: Request<pb::IngestRequest>,
    ) -> Result<Response<pb::IngestResponse>, Status> {
        let (who, t) = self
            .authorize(&req, &req.get_ref().tenant.clone(), Permission::Ingest)
            .await?;
        let docs = req
            .into_inner()
            .documents
            .into_iter()
            .map(|d| {
                Ok(Document {
                    id: DocumentId::new(&d.id).map_err(status)?,
                    title: d.title,
                    text: d.text,
                    source: d.source,
                    metadata: json_map(&d.metadata_json)?,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let n = docs.len();
        let queued = self
            .state
            .ingest_queue
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::unavailable("ingest queue is full"))?;
        let worker = self
            .state
            .ingest_workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let (e, tq) = (self.state.engine.clone(), t.clone());
        let r = Self::run(move || {
            let _held = (queued, worker);
            e.ingest(&tq, docs)
        })
        .await;
        self.state.audit(
            &who,
            "ingest",
            Some(&t),
            &format!("{n} documents (grpc)"),
            if r.is_ok() { "ok" } else { "error" },
        );
        let r = r?;
        Ok(Response::new(pb::IngestResponse {
            documents: r
                .documents
                .into_iter()
                .map(|d| pb::IngestedDocument {
                    id: d.id.to_string(),
                    status: wire(&d.status),
                    chunks: d.chunks as u32,
                    version: d.version,
                })
                .collect(),
            generation: r.generation,
        }))
    }

    async fn delete_document(
        &self,
        req: Request<pb::DeleteDocumentRequest>,
    ) -> Result<Response<pb::DeleteDocumentResponse>, Status> {
        let (who, t) = self
            .authorize(
                &req,
                &req.get_ref().tenant.clone(),
                Permission::DeleteDocuments,
            )
            .await?;
        let id = DocumentId::new(&req.get_ref().id).map_err(status)?;
        let (e, tq, idq) = (self.state.engine.clone(), t.clone(), id.clone());
        let n = Self::run(move || e.delete_documents(&tq, &[idq])).await?;
        self.state.audit(
            &who,
            "delete_document",
            Some(&t),
            id.as_str(),
            if n > 0 { "ok" } else { "not_found" },
        );
        Ok(Response::new(pb::DeleteDocumentResponse { deleted: n > 0 }))
    }
}

pub fn service(state: Arc<AppState>) -> Rag2Server<Service> {
    Rag2Server::new(Service { state })
}
