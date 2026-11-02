//! RAG2 shared types and versioned component interfaces.
//!
//! Every replaceable component (vector index, lexical index, embedder, decision model, connector)
//! implements a trait from [`interfaces`]. The interface set is versioned by
//! [`INTERFACE_VERSION`]; a breaking trait change bumps it, so plugins and embedders can check
//! compatibility at load time instead of failing at call time.

pub mod error;
pub mod ids;
pub mod interfaces;
pub mod model;

pub use error::{Error, Result};
pub use ids::{ChunkId, DocumentId, TenantId};

/// Version of the component interfaces in [`interfaces`]. Bumped on any breaking change.
pub const INTERFACE_VERSION: u32 = 1;
