//! Every user-facing string in the console, in the Hesperides voice: sentence case, short
//! declaratives, errors say what happened and then what to do, empty states are calm and specific.
//! Strings with values are functions so the wording stays here too.

pub mod common {
    pub const LOADING: &str = "Loading";
    pub const PENDING: &str = "…";
    pub const ERROR_TITLE: &str = "The server refused that request";
    pub const YES: &str = "Yes";
    pub const NO: &str = "No";
}

/// Section groups: the sidebar titles and the page eyebrows.
pub mod group {
    pub const STATUS: &str = "Status";
    pub const CONFIGURATION: &str = "Configuration";
    pub const RETRIEVAL: &str = "Retrieval";
    pub const GOVERNANCE: &str = "Governance";
}

pub mod nav {
    pub const LABEL: &str = "Console sections";
    pub const BRAND: &str = "RAG2";
    pub const OVERVIEW: &str = "Overview";
    pub const PIPELINES: &str = "Pipelines";
    pub const TENANTS: &str = "Tenants and quotas";
    pub const CONFIG: &str = "Profile and versions";
    pub const CONNECTORS: &str = "Connectors";
    pub const PLAYGROUND: &str = "Playground";
    pub const DOCUMENTS: &str = "Documents";
    pub const AUDIT: &str = "Audit log";
    pub const SIGN_OUT: &str = "Sign out";
}

pub mod login {
    pub const EYEBROW: &str = "RAG2 console";
    pub const TITLE: &str = "Sign in";
    pub const SERVER_URL: &str = "Server URL";
    pub const KEY: &str = "API key or access token";
    pub const KEY_HINT: &str =
        "Needs the manage_config permission. Kept in this tab's session storage only.";
    pub const SUBMIT: &str = "Sign in";
}

pub mod change {
    pub const LABEL: &str = "Review and apply";
    pub const NOTE: &str = "Change note";
    pub const NOTE_HINT: &str = "Recorded in the version history.";
    pub const PREVIEW: &str = "Preview changes";
    pub const FIELD: &str = "Field";
    pub const BEFORE: &str = "Before";
    pub const AFTER: &str = "After";
    pub const APPLY: &str = "Apply as the new active version";

    pub fn count(n: usize) -> String {
        if n == 1 {
            "1 change".to_string()
        } else {
            format!("{n} changes")
        }
    }

    pub fn issue(path: &str, message: &str) -> String {
        format!("{path}: {message}")
    }
}

pub mod overview {
    pub const TITLE: &str = "Overview";
    pub const LEAD: &str = "The running profile, its tenants and the components it has loaded.";
    pub const ACTIVE_VERSION: &str = "Active version";
    pub const PROFILE: &str = "Profile";
    pub const TENANTS: &str = "Tenants";
    pub const ROLE: &str = "Role";
    pub const DEFAULT_ROLE: &str = "standalone";
    pub const COMPONENTS: &str = "Components";
    pub const KIND: &str = "Kind";
    pub const NAME: &str = "Name";
    pub const STATUS: &str = "Status";
    pub const EMBEDDER: &str = "Embedder";
    pub const DECISION_MODEL: &str = "Decision model";
    pub const LOADED: &str = "Loaded";
    pub const HEALTHY: &str = "Healthy";
    pub const UNREACHABLE: &str = "Unreachable";

    pub fn version(v: &serde_json::Value) -> String {
        format!("v{v}")
    }
}

pub mod pipelines {
    pub const TITLE: &str = "Pipelines";
    pub const LEAD: &str = "Edit retrieval behaviour without code. The server previews and validates every change before it becomes a new version.";
    pub const PIPELINE: &str = "Pipeline";
    pub const RETRIEVERS: &str = "Retrievers";
    pub const KIND: &str = "Kind";
    pub const ENABLED: &str = "Enabled";
    pub const WEIGHT: &str = "Weight";
    pub const CANDIDATES: &str = "Candidates (top_k)";
    pub const RANKING: &str = "Ranking and results";
    pub const FUSION: &str = "Fusion";
    pub const FUSION_RRF: &str = "Reciprocal rank (robust)";
    pub const FUSION_WEIGHTED: &str = "Weighted scores";
    pub const RRF_K: &str = "RRF k";
    pub const RESULTS: &str = "Results (top_k)";
    pub const EXTRA_HOPS: &str = "Extra hops";
    pub const QUALITY: &str = "Evidence quality";
    pub const MIN_CONFIDENCE: &str = "Abstain below confidence";
    pub const MIN_EVIDENCE: &str = "Minimum evidence";
    pub const NEAR_DUPLICATE: &str = "Near-duplicate similarity";
    pub const CONTRADICTION: &str = "Contradiction overlap";

    pub fn retriever(kind: &str, embedder: Option<&str>) -> String {
        embedder.map_or_else(|| kind.to_string(), |e| format!("{kind} ({e})"))
    }
}

pub mod tenants {
    pub const TITLE: &str = "Tenants and quotas";
    pub const LEAD: &str = "Rate limits and document quotas per tenant. Tenants without an override use the profile defaults.";
    pub const QUOTAS: &str = "Quotas";
    pub const TENANT: &str = "Tenant";
    pub const RPS: &str = "Requests/s";
    pub const BURST: &str = "Burst";
    pub const MAX_DOCS: &str = "Max documents";
    pub const HAS_DATA: &str = "Has data";
    pub const DELETE: &str = "Delete data";
    pub const ADD_LABEL: &str = "Add a tenant override";
    pub const ADD_PLACEHOLDER: &str = "tenant-id";
    pub const ADD: &str = "Add";
    pub const CONFIRM_LABEL: &str = "Confirm deletion";
    pub const CONFIRM_HINT: &str = "Type the tenant id, then choose Delete data on its row.";
    pub const EMPTY_TITLE: &str = "No tenants yet";
    pub const EMPTY_TEXT: &str = "Tenants appear here once they hold data or have an override.";

    pub fn confirm_needed(tenant: &str) -> String {
        format!(
            "Deletion not confirmed. Type \"{tenant}\" in Confirm deletion to remove it and all its data."
        )
    }
}

pub mod config {
    pub const TITLE: &str = "Profile and versions";
    pub const LEAD: &str = "The whole profile as YAML, for anything the guided pages do not cover, with its version history.";
    pub const EDITOR: &str = "Profile";
    pub const YAML: &str = "Profile (YAML)";
    pub const YAML_HINT: &str = "Secrets stay references such as env:NAME.";
    pub const NOTE: &str = "Change note";
    pub const VALIDATE: &str = "Validate and diff";
    pub const SAVE: &str = "Save and activate";
    pub const SAVED: &str = "A new version is active.";
    pub const ROLLED_BACK: &str = "The previous version is active again.";
    pub const ACTIVATED: &str = "That version is active.";
    pub const VALID: &str = "The profile is valid.";
    pub const INVALID: &str =
        "The profile is not valid. Fix the issues below, then validate again.";
    pub const DIFF: &str = "Changes and issues";
    pub const HISTORY: &str = "History";
    pub const ROLLBACK: &str = "Roll back to previous";
    pub const VERSION: &str = "Version";
    pub const AUTHOR: &str = "Author";
    pub const COMMENT: &str = "Note";
    pub const ACTIVE: &str = "Active";
    pub const ACTIVATE: &str = "Activate";

    pub fn version(n: u64) -> String {
        format!("v{n}")
    }
}

pub mod connectors {
    pub const TITLE: &str = "Connectors";
    pub const LEAD: &str = "Sources synchronised on a schedule. Add or change them in Profile and versions, under connectors; every change is validated and versioned.";
    pub const SOURCES: &str = "Sources";
    pub const NAME: &str = "Name";
    pub const TENANT: &str = "Tenant";
    pub const KIND: &str = "Kind";
    pub const EVERY: &str = "Every";
    pub const LAST_SYNC: &str = "Last sync";
    pub const NEVER: &str = "Never";
    pub const FAILED: &str = "Failed";
    pub const ON_DEMAND: &str = "On demand";
    pub const TEST: &str = "Test connection";
    pub const SYNC: &str = "Sync now";
    pub const EMPTY_TITLE: &str = "No connectors yet";
    pub const EMPTY_TEXT: &str = "Define one in the profile and it will appear here.";

    pub fn fetched(n: &serde_json::Value) -> String {
        format!("{n} fetched")
    }

    pub fn interval(seconds: u64) -> String {
        format!("{seconds} s")
    }

    pub fn report(name: &str, v: &serde_json::Value) -> String {
        format!(
            "{name}: {} created, {} updated, {} unchanged, {} deleted.",
            v["created"], v["updated"], v["unchanged"], v["deleted"]
        )
    }
}

pub mod playground {
    pub const TITLE: &str = "Playground";
    pub const LEAD: &str = "Run a query as a client would and read the evidence, scores, route, confidence and timings.";
    pub const TENANT: &str = "Tenant";
    pub const QUERY: &str = "Query";
    pub const SEARCH: &str = "Search";
    pub const RESULT: &str = "Answer";
    pub const CONFIDENCE: &str = "Confidence";
    pub const ROUTE: &str = "Route";
    pub const LATENCY: &str = "Latency";
    pub const ABSTAINED: &str = "Abstained";
    pub const CONTRADICTION: &str = "Contradiction";
    pub const EVIDENCE: &str = "Evidence";

    pub fn confidence(c: f64) -> String {
        format!("{c:.3}")
    }

    pub fn route(retrievers: &str, reason: &str) -> String {
        format!("{retrievers} ({reason})")
    }

    pub fn micros(v: &serde_json::Value) -> String {
        format!("{v} µs")
    }

    pub fn abstained(reason: &serde_json::Value, detail: &str) -> String {
        format!("{reason}: {detail}")
    }

    pub fn contradiction(
        detector: &str,
        a: &serde_json::Value,
        b: &serde_json::Value,
        reason: &str,
    ) -> String {
        format!("{detector}: chunk {a} against chunk {b}. {reason}")
    }

    pub fn signal(retriever: &str, rank: &serde_json::Value, score: f64) -> String {
        format!("{retriever}#{rank} ({score:.3})")
    }

    /// Figures go in mono (Cormorant's old-style figures read 1 as a Roman I).
    pub fn evidence_meta(
        rank: usize,
        ordinal: &serde_json::Value,
        version: &serde_json::Value,
        score: f64,
        signals: &str,
    ) -> String {
        format!("#{rank} · chunk {ordinal} · v{version} · score {score:.4} · {signals}")
    }
}

pub mod documents {
    pub const TITLE: &str = "Documents";
    pub const LEAD: &str = "Browse what a tenant holds and read how each document was chunked.";
    pub const TENANT: &str = "Tenant";
    pub const LIST: &str = "List";
    pub const ID: &str = "Id";
    pub const DOC_TITLE: &str = "Title";
    pub const VERSION: &str = "Version";
    pub const CHUNKS: &str = "Chunks";
    pub const NEXT: &str = "Next page";
    pub const EMPTY_TITLE: &str = "No documents listed";
    pub const EMPTY_TEXT: &str = "Choose a tenant and list its documents.";

    pub fn stats(v: &serde_json::Value) -> String {
        format!(
            "{} documents · generation {} · lexical {} chunks · vector indexes {}",
            v["documents"], v["generation"], v["lexical_docs"], v["vector_indexes"]
        )
    }

    pub fn chunks_of(id: &str) -> String {
        format!("Chunks of {id}")
    }

    pub fn chunk_meta(ordinal: &serde_json::Value, id: &serde_json::Value) -> String {
        format!("#{ordinal} · id {id}")
    }
}

pub mod audit {
    pub const TITLE: &str = "Audit log";
    pub const LEAD: &str = "Every administrative action, who took it and how it ended.";
    pub const EVENTS: &str = "Events";
    pub const TIME: &str = "Time (UTC ms)";
    pub const PRINCIPAL: &str = "Principal";
    pub const ACTION: &str = "Action";
    pub const TENANT: &str = "Tenant";
    pub const TARGET: &str = "Target";
    pub const OUTCOME: &str = "Outcome";
    pub const MORE: &str = "More events";
    pub const EMPTY_TITLE: &str = "No entries yet";
    pub const EMPTY_TEXT: &str = "The record begins with the first administrative action.";
}

#[cfg(test)]
mod tests {
    #[test]
    fn change_count_is_singular_for_one() {
        assert_eq!(super::change::count(1), "1 change");
        assert_eq!(super::change::count(3), "3 changes");
    }
}
