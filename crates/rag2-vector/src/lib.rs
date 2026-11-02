//! Vector indexes behind [`rag2_core::interfaces::VectorIndex`].
//!
//! - [`flat::FlatIndex`]: exact scan, the recall reference, best for small tenants.
//! - [`hnsw::HnswIndex`]: approximate graph search for large tenants.
//!
//! The choice and every tuning value come from configuration; see `docs/benchmarks/vector-index.md`
//! for the measurements behind the defaults.

pub mod flat;
pub mod hnsw;
pub mod simd;
pub mod topk;

pub use flat::{FlatConfig, FlatIndex};
pub use hnsw::{HnswConfig, HnswIndex};
