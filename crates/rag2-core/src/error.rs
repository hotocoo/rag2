//! One error type shared across crates, mapped onto transport status codes at the edge.

/// Errors a component may return. The variant decides the status a client sees; the message is
/// safe to show (it never carries secrets or another tenant's data).
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("overloaded: {0}")]
    Overloaded(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl Error {
    /// Stable machine-readable code for API responses.
    pub fn code(&self) -> &'static str {
        match self {
            Error::InvalidInput(_) => "invalid_input",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::Unauthenticated(_) => "unauthenticated",
            Error::Forbidden(_) => "forbidden",
            Error::RateLimited(_) => "rate_limited",
            Error::Overloaded(_) => "overloaded",
            Error::Unavailable(_) => "unavailable",
            Error::Storage(_) => "storage",
            Error::Internal(_) => "internal",
        }
    }

    pub fn storage(e: impl std::fmt::Display) -> Self {
        Error::Storage(e.to_string())
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        Error::Internal(e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
