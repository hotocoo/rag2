//! The ModernBERT encoder on candle, with fused CPU kernels for its hot elementwise path. RAG2's
//! candle embedder and the Laya decision runtime both run it.

// The kernel tests use `unwrap()` for brevity.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod encoder;
mod fused;

pub use encoder::{Config, ModernBert};
