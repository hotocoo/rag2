//! Errors as HTTP responses: one JSON envelope, a stable machine-readable code, a safe message.
//! Internal and storage errors are logged in full and returned generically.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rag2_core::Error;
use serde::Serialize;

/// Seconds a client should wait after 429/503 before retrying.
pub const RETRY_AFTER_SECS: &str = "1";

#[derive(Debug)]
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

pub fn status_of(e: &Error) -> StatusCode {
    match e {
        Error::InvalidInput(_) => StatusCode::BAD_REQUEST,
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::Conflict(_) => StatusCode::CONFLICT,
        Error::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
        Error::Forbidden(_) => StatusCode::FORBIDDEN,
        Error::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
        Error::Overloaded(_) | Error::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        Error::Storage(_) | Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = status_of(&self.0);
        let message = if status.is_server_error()
            && !matches!(self.0, Error::Overloaded(_) | Error::Unavailable(_))
        {
            tracing::error!(error = %self.0, "request failed");
            "internal error; see server logs".to_string()
        } else {
            self.0.to_string()
        };
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.0.code().into(),
                message,
            },
        };
        let mut resp = (status, Json(body)).into_response();
        if matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            resp.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_static(RETRY_AFTER_SECS),
            );
        }
        resp
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
