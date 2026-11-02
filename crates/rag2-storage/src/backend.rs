//! The key-value contract a storage engine must meet: named spaces, atomic multi-key batches,
//! ordered prefix scans. Fjall and redb implement it; the typed tenant API sits on top.

use rag2_core::Result;

/// Logical spaces. Each maps to one Fjall keyspace / one redb table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Space {
    /// `tenant \0 doc_id` -> DocumentRecord
    Documents,
    /// `tenant \0 chunk_id(be u64)` -> Chunk
    Chunks,
    /// `tenant \0 chunk_id(be u64)` -> f32 LE vector
    Vectors,
    /// `tenant \0 name` -> counters and per-tenant metadata; `\0tenants\0 tenant` registry
    Meta,
    /// `ts(be u64) seq(be u64)` -> audit event
    Audit,
    /// `tenant \0 seq(be u64)` -> replication log entry
    Log,
}

impl Space {
    pub const ALL: [Space; 6] = [
        Space::Documents,
        Space::Chunks,
        Space::Vectors,
        Space::Meta,
        Space::Audit,
        Space::Log,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Space::Documents => "documents",
            Space::Chunks => "chunks",
            Space::Vectors => "vectors",
            Space::Meta => "meta",
            Space::Audit => "audit",
            Space::Log => "log",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Op {
    Put(Space, Vec<u8>, Vec<u8>),
    Delete(Space, Vec<u8>),
}

pub type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

pub trait KvBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn get(&self, space: Space, key: &[u8]) -> Result<Option<Vec<u8>>>;
    /// All pairs whose key starts with `prefix`, ascending, starting strictly after `after` when
    /// given, at most `limit`.
    fn scan(
        &self,
        space: Space,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Pairs>;
    /// Apply every op or none.
    fn write(&self, ops: Vec<Op>) -> Result<()>;
    /// Flush to stable storage.
    fn persist(&self) -> Result<()>;
}
