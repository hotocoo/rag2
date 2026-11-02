//! Model Context Protocol endpoint (`POST /mcp`, JSON-RPC 2.0, JSON responses). Exposes RAG2 as
//! tools an AI assistant can call. Authentication and tenant scoping are the same as REST: the
//! bearer token's principal decides which tenants and operations are allowed.

use crate::auth::Principal;
use crate::routes::{blocking, principal};
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rag2_config::schema::Permission;
use rag2_core::{DocumentId, Error, TenantId};
use rag2_engine::QueryRequest;
use serde_json::{Value, json};
use std::sync::Arc;

/// MCP revision this endpoint implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

fn tools() -> Value {
    json!([
        {
            "name": "search",
            "description": "Retrieve ranked, cited evidence from a RAG2 tenant. Returns passages with document ids, scores, a calibrated confidence and whether the engine abstained.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tenant": { "type": "string", "description": "Tenant id" },
                    "query": { "type": "string" },
                    "top_k": { "type": "integer", "minimum": 1 },
                    "pipeline": { "type": "string" }
                },
                "required": ["tenant", "query"]
            }
        },
        {
            "name": "get_document_chunks",
            "description": "Full text of one document, chunk by chunk.",
            "inputSchema": {
                "type": "object",
                "properties": { "tenant": { "type": "string" }, "document_id": { "type": "string" } },
                "required": ["tenant", "document_id"]
            }
        },
        {
            "name": "list_documents",
            "description": "Document ids and titles in a tenant (first page).",
            "inputSchema": {
                "type": "object",
                "properties": { "tenant": { "type": "string" }, "limit": { "type": "integer", "minimum": 1, "maximum": 1000 } },
                "required": ["tenant"]
            }
        }
    ])
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn tool_text(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, Error> {
    args[name]
        .as_str()
        .ok_or_else(|| Error::InvalidInput(format!("argument `{name}` is required")))
}

async fn call_tool(
    s: &Arc<AppState>,
    who: &Principal,
    name: &str,
    args: &Value,
) -> Result<String, Error> {
    let t = TenantId::new(arg(args, "tenant")?)?;
    let e = s.engine.clone();
    match name {
        "search" => {
            who.require_tenant(Permission::Query, &t)?;
            s.admit(&t)?;
            let mut req = QueryRequest::new(arg(args, "query")?);
            req.top_k = args["top_k"].as_u64().map(|k| k as usize);
            req.pipeline = args["pipeline"].as_str().map(String::from);
            let r = blocking(move || e.query(&t, req)).await.map_err(|e| e.0)?;
            let evidence: Vec<Value> = r
                .evidence
                .iter()
                .enumerate()
                .map(|(i, ev)| {
                    json!({
                        "n": i + 1,
                        "document_id": ev.provenance.doc_id,
                        "chunk": ev.provenance.chunk_ordinal,
                        "source": ev.provenance.source,
                        "score": ev.score,
                        "text": ev.text,
                    })
                })
                .collect();
            Ok(serde_json::to_string_pretty(&json!({
                "confidence": r.confidence,
                "abstained": r.abstained,
                "contradictions": r.contradictions,
                "evidence": evidence,
            }))
            .unwrap_or_default())
        }
        "get_document_chunks" => {
            who.require_tenant(Permission::ReadDocuments, &t)?;
            s.admit(&t)?;
            let id = DocumentId::new(arg(args, "document_id")?)?;
            let chunks = blocking(move || e.document_chunks(&t, &id))
                .await
                .map_err(|e| e.0)?;
            Ok(chunks
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
                .join("\n\n"))
        }
        "list_documents" => {
            who.require_tenant(Permission::ReadDocuments, &t)?;
            s.admit(&t)?;
            let limit = args["limit"].as_u64().unwrap_or(100).clamp(1, 1000) as usize;
            let docs = blocking(move || e.list_documents(&t, None, limit))
                .await
                .map_err(|e| e.0)?;
            let rows: Vec<Value> = docs
                .iter()
                .map(|d| json!({ "id": d.id, "title": d.title, "version": d.version }))
                .collect();
            Ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
        }
        other => Err(Error::NotFound(format!("unknown tool {other}"))),
    }
}

async fn handle_one(s: &Arc<AppState>, who: &Principal, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    // Notifications (no id) get no response.
    let id = match id {
        Some(id) => id,
        None => return None,
    };
    if msg["jsonrpc"] != "2.0" || method.is_empty() {
        return Some(rpc_error(id, INVALID_REQUEST, "not a JSON-RPC 2.0 request"));
    }
    Some(match method {
        "initialize" => rpc_ok(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "rag2", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Use `search` to retrieve cited evidence; respect `abstained` and `confidence`."
            }),
        ),
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            if name.is_empty() {
                return Some(rpc_error(id, INVALID_PARAMS, "params.name is required"));
            }
            let args = msg["params"]["arguments"].clone();
            match call_tool(s, who, name, &args).await {
                Ok(text) => rpc_ok(id, tool_text(text, false)),
                // Tool failures are results the model can read, not protocol errors.
                Err(e) => rpc_ok(id, tool_text(e.to_string(), true)),
            }
        }
        _ => rpc_error(
            id,
            METHOD_NOT_FOUND,
            &format!("method {method} not supported"),
        ),
    })
}

pub async fn endpoint(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let who = match principal(&s, &h).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(rpc_error(Value::Null, PARSE_ERROR, "invalid JSON")).into_response(),
    };
    match msg {
        Value::Array(batch) => {
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = handle_one(&s, &who, m).await {
                    out.push(r);
                }
            }
            if out.is_empty() {
                StatusCode::ACCEPTED.into_response()
            } else {
                Json(Value::Array(out)).into_response()
            }
        }
        m => match handle_one(&s, &who, m).await {
            Some(r) => Json(r).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}
