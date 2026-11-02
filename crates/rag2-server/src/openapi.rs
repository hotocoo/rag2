//! OpenAPI 3.1 document generated from the same `schemars` types the handlers use, so the spec
//! cannot drift from the code. Served at `/v1/openapi.json`; `rag2 openapi` prints it.

use crate::admin::{ActiveConfig, ComponentHealth, ProfileSubmission, SubmitResult, Versions};
use crate::error::ErrorBody;
use rag2_config::Preview;
use rag2_core::model::DocumentRecord;
use rag2_engine::{
    ChunkView, IngestReport, IngestRequest, QueryRequest, QueryResponse, TenantStats,
};
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

struct Gen {
    generator: schemars::SchemaGenerator,
}

impl Gen {
    fn new() -> Self {
        let mut settings = SchemaSettings::draft2020_12();
        settings.definitions_path = "/components/schemas".into();
        Gen {
            generator: settings.into_generator(),
        }
    }

    fn schema<T: schemars::JsonSchema>(&mut self) -> Value {
        serde_json::to_value(self.generator.subschema_for::<T>()).unwrap_or_default()
    }

    fn components(mut self) -> Value {
        Value::Object(self.generator.take_definitions(true).into_iter().collect())
    }
}

fn json_body(schema: Value) -> Value {
    json!({ "required": true, "content": { "application/json": { "schema": schema } } })
}

fn ok(schema: Value) -> Value {
    json!({ "description": "OK", "content": { "application/json": { "schema": schema } } })
}

fn op(
    summary: &str,
    tag: &str,
    params: &[&str],
    body: Option<Value>,
    resp: Value,
    err: &Value,
) -> Value {
    let mut o = Map::new();
    o.insert("summary".into(), json!(summary));
    o.insert("tags".into(), json!([tag]));
    let ps: Vec<Value> = params
        .iter()
        .map(|p| json!({ "name": p, "in": "path", "required": true, "schema": { "type": if *p == "version" { "integer" } else { "string" } } }))
        .collect();
    if !ps.is_empty() {
        o.insert("parameters".into(), Value::Array(ps));
    }
    if let Some(b) = body {
        o.insert("requestBody".into(), b);
    }
    o.insert(
        "responses".into(),
        json!({
            "200": resp,
            "400": { "description": "Invalid input", "content": { "application/json": { "schema": err } } },
            "401": { "description": "Unauthenticated" },
            "403": { "description": "Forbidden" },
            "429": { "description": "Rate limited (Retry-After)" },
            "503": { "description": "Overloaded or unavailable (Retry-After)" }
        }),
    );
    Value::Object(o)
}

pub fn spec(version: &str) -> Value {
    let mut g = Gen::new();
    let err = g.schema::<ErrorBody>();
    let t = &["tenant"];
    let td = &["tenant", "id"];
    let paths = json!({
        "/v1/tenants/{tenant}/query": { "post": op("Adaptive hybrid retrieval", "retrieval", t, Some(json_body(g.schema::<QueryRequest>())), ok(g.schema::<QueryResponse>()), &err) },
        "/v1/tenants/{tenant}/documents": {
            "post": op("Ingest documents (incremental: unchanged documents are skipped)", "documents", t, Some(json_body(g.schema::<IngestRequest>())), ok(g.schema::<IngestReport>()), &err),
            "get": op("List documents (query: after, limit)", "documents", t, None, ok(g.schema::<Vec<DocumentRecord>>()), &err)
        },
        "/v1/tenants/{tenant}/documents/{id}": {
            "get": op("Get a document record (percent-encode the id)", "documents", td, None, ok(g.schema::<DocumentRecord>()), &err),
            "delete": op("Delete a document and its chunks from every index", "documents", td, None, json!({ "description": "Deleted" }), &err)
        },
        "/v1/tenants/{tenant}/documents/{id}/chunks": { "get": op("A document's chunks", "documents", td, None, ok(g.schema::<Vec<ChunkView>>()), &err) },
        "/v1/tenants/{tenant}/stats": { "get": op("Tenant index statistics", "documents", t, None, ok(g.schema::<TenantStats>()), &err) },
        "/v1/admin/config": { "get": op("Active configuration", "config", &[], None, ok(g.schema::<ActiveConfig>()), &err) },
        "/v1/admin/config/schema": { "get": op("JSON Schema of profiles", "config", &[], None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/config/versions": {
            "get": op("Version history and activations", "config", &[], None, ok(g.schema::<Versions>()), &err),
            "post": op("Store a new version (optionally activate)", "config", &[], Some(json_body(g.schema::<ProfileSubmission>())), ok(g.schema::<SubmitResult>()), &err)
        },
        "/v1/admin/config/versions/{version}": { "get": op("One stored version", "config", &["version"], None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/config/versions/{version}/activate": { "post": op("Activate a version atomically", "config", &["version"], None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/config/preview": { "post": op("Validate and diff a profile without storing it", "config", &[], Some(json_body(g.schema::<ProfileSubmission>())), ok(g.schema::<Preview>()), &err) },
        "/v1/admin/config/rollback": { "post": op("Re-activate the previous version", "config", &[], None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/tenants": { "get": op("List tenants", "tenants", &[], None, ok(json!({ "type": "array", "items": { "type": "string" } })), &err) },
        "/v1/admin/tenants/{tenant}": { "delete": op("Delete a tenant and all its data (requires ?confirm=<tenant>)", "tenants", t, None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/tenants/{tenant}/reindex": { "post": op("Re-embed and rebuild a tenant's indexes", "tenants", t, None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/audit": { "get": op("Audit trail (query: after, limit)", "operations", &[], None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/components": { "get": op("Loaded embedders and decision models", "operations", &[], None, ok(g.schema::<ComponentHealth>()), &err) },
        "/v1/admin/connectors": { "get": op("Connectors and their last sync", "connectors", &[], None, ok(g.schema::<Vec<crate::connectors::ConnectorView>>()), &err) },
        "/v1/admin/connectors/{name}/test": { "post": op("Test a connector's connection and credentials", "connectors", &["name"], None, ok(json!({ "type": "object" })), &err) },
        "/v1/admin/connectors/{name}/run": { "post": op("Sync a connector now", "connectors", &["name"], None, ok(g.schema::<rag2_engine::SyncReport>()), &err) },
        "/mcp": { "post": { "summary": "Model Context Protocol (JSON-RPC 2.0): tools search, get_document_chunks, list_documents", "tags": ["integration"], "responses": { "200": { "description": "JSON-RPC response" }, "202": { "description": "notification accepted" } } } },
        "/healthz": { "get": { "summary": "Liveness", "tags": ["operations"], "responses": { "200": { "description": "alive" } } } },
        "/readyz": { "get": { "summary": "Readiness", "tags": ["operations"], "responses": { "200": { "description": "ready" }, "503": { "description": "starting" } } } },
        "/metrics": { "get": { "summary": "Prometheus metrics", "tags": ["operations"], "responses": { "200": { "description": "text exposition" } } } }
    });
    json!({
        "openapi": "3.1.0",
        "info": { "title": "RAG2 API", "version": version, "description": "Adaptive retrieval and evidence intelligence." },
        "servers": [{ "url": "/" }],
        "security": [{ "bearer": [] }, { "apiKey": [] }],
        "paths": paths,
        "components": {
            "schemas": g.components(),
            "securitySchemes": {
                "bearer": { "type": "http", "scheme": "bearer", "description": "API key or OIDC access token" },
                "apiKey": { "type": "apiKey", "in": "header", "name": crate::auth::API_KEY_HEADER }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_schema_reference_resolves() {
        let s = super::spec("test");
        let text = s.to_string();
        let comps = s["components"]["schemas"].as_object().expect("schemas");
        for part in text.split("\"$ref\":\"#/components/schemas/").skip(1) {
            let name = part.split('"').next().expect("name");
            assert!(comps.contains_key(name), "dangling $ref {name}");
        }
        assert!(comps.contains_key("QueryResponse"));
    }
}
