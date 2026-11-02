// RAG2 REST client. Shapes mirror the server's OpenAPI document (`/v1/openapi.json`); run
// `npm run types:openapi` to generate the complete type set from it.

export interface Provenance { doc_id: string; doc_version: number; chunk_ordinal: number; start: number; end: number; source?: string; title?: string }
export interface Signal { retriever: "dense" | "lexical" | "structured" | "graph"; rank: number; score: number; hop: number }
export interface Evidence { chunk_id: number; text: string; score: number; signals: Signal[]; provenance: Provenance }
export interface Contradiction { a: number; b: number; detector: string; reason: string; confidence: number }
export interface QueryRequest { query: string; top_k?: number; pipeline?: string; filters?: Record<string, unknown>; hops?: number; include_context?: boolean; no_cache?: boolean }
export interface QueryResponse {
  evidence: Evidence[]; confidence: number; abstained?: { reason: string; detail: string };
  contradictions?: Contradiction[]; context?: string; pipeline: string;
  route: { retrievers: string[]; reason: string }; hops: number; config_version: number;
  timings: { total_us: number }; cached: boolean; warnings?: string[]; query_rewritten?: string;
}
export interface Document { id: string; text: string; title?: string; source?: string; metadata?: Record<string, unknown> }
export interface IngestReport { documents: { id: string; status: "created" | "updated" | "unchanged" | "dropped"; chunks: number; version: number }[]; generation: number; searchable_within_ms: number }

export class Rag2Error extends Error {
  constructor(public status: number, public code: string, message: string) { super(`${status} ${code}: ${message}`); }
  /** 429 and 503 are worth retrying after the server's Retry-After. */
  get retryable(): boolean { return this.status === 429 || this.status === 503; }
}

const seg = (s: string) => encodeURIComponent(s);

export class Rag2Client {
  constructor(private baseUrl: string, private apiKey?: string, private timeoutMs = 30_000) {}

  private async call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await fetch(this.baseUrl.replace(/\/$/, "") + path, {
      method,
      headers: { "content-type": "application/json", ...(this.apiKey ? { authorization: `Bearer ${this.apiKey}` } : {}) },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    if (res.status === 204) return undefined as T;
    const json = await res.json().catch(() => ({}));
    if (!res.ok) throw new Rag2Error(res.status, json?.error?.code ?? "unknown", json?.error?.message ?? res.statusText);
    return json as T;
  }

  query(tenant: string, req: QueryRequest): Promise<QueryResponse> { return this.call("POST", `/v1/tenants/${seg(tenant)}/query`, req); }
  ingest(tenant: string, documents: Document[]): Promise<IngestReport> { return this.call("POST", `/v1/tenants/${seg(tenant)}/documents`, { documents }); }
  getDocument(tenant: string, id: string) { return this.call<Record<string, unknown>>("GET", `/v1/tenants/${seg(tenant)}/documents/${seg(id)}`); }
  deleteDocument(tenant: string, id: string): Promise<void> { return this.call("DELETE", `/v1/tenants/${seg(tenant)}/documents/${seg(id)}`); }
  stats(tenant: string) { return this.call<Record<string, unknown>>("GET", `/v1/tenants/${seg(tenant)}/stats`); }
}
