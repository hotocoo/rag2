//! Validated identifiers. A `TenantId` is required by every data-access API, so isolation is a
//! type-level property rather than a convention.

use crate::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Longest tenant id accepted. Tenant ids become directory names and key prefixes.
pub const TENANT_ID_MAX_LEN: usize = 64;
/// Longest document id accepted.
pub const DOCUMENT_ID_MAX_LEN: usize = 512;

/// A tenant: lowercase ASCII letters, digits, `-` and `_`, 1..=64 bytes, starting with a letter or
/// digit. The character set makes it safe as a path segment and a key prefix.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(Arc<str>);

impl TenantId {
    pub fn new(s: impl AsRef<str>) -> Result<Self> {
        let s = s.as_ref();
        let ok_char =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
        let starts_ok = s
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if s.is_empty() || s.len() > TENANT_ID_MAX_LEN || !starts_ok || !s.chars().all(ok_char) {
            return Err(Error::InvalidInput(format!(
                "tenant id must be 1..={TENANT_ID_MAX_LEN} chars of [a-z0-9_-], starting with [a-z0-9]"
            )));
        }
        Ok(TenantId(Arc::from(s)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TenantId {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        TenantId::new(s)
    }
}

impl From<TenantId> for String {
    fn from(t: TenantId) -> String {
        t.0.to_string()
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TenantId({})", self.0)
    }
}

/// A caller-chosen document id: 1..=512 bytes, no control characters.
#[derive(
    Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, Debug,
)]
#[serde(try_from = "String", into = "String")]
pub struct DocumentId(Arc<str>);

impl DocumentId {
    pub fn new(s: impl AsRef<str>) -> Result<Self> {
        let s = s.as_ref();
        if s.is_empty() || s.len() > DOCUMENT_ID_MAX_LEN || s.chars().any(char::is_control) {
            return Err(Error::InvalidInput(format!(
                "document id must be 1..={DOCUMENT_ID_MAX_LEN} bytes with no control characters"
            )));
        }
        Ok(DocumentId(Arc::from(s)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DocumentId {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        DocumentId::new(s)
    }
}

impl From<DocumentId> for String {
    fn from(d: DocumentId) -> String {
        d.0.to_string()
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A chunk, numbered per tenant by storage. Dense `u64` so vector and lexical indexes key on it
/// without string hashing on the hot path.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, Debug,
)]
pub struct ChunkId(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_ids_accept_the_safe_charset_only() {
        assert!(TenantId::new("acme-prod_1").is_ok());
        for bad in ["", "Acme", "-x", "a/b", "..", "a b", &"a".repeat(65)] {
            assert!(TenantId::new(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn tenant_ids_validate_on_deserialize() {
        assert!(serde_json::from_str::<TenantId>("\"ok\"").is_ok());
        assert!(serde_json::from_str::<TenantId>("\"../etc\"").is_err());
    }

    #[test]
    fn document_ids_refuse_control_characters_and_empty() {
        assert!(DocumentId::new("docs/a.md#1").is_ok());
        assert!(DocumentId::new("").is_err());
        assert!(DocumentId::new("a\nb").is_err());
    }
}
