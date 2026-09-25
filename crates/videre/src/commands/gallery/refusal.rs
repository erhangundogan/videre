//! Why a gallery write was refused, turned into its response at the handler.
//! Small on purpose: a handler's helpers return `Result<_, Refusal>`, and an
//! axum `Response` as the error type is large enough to trip clippy.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub(crate) enum Refusal {
    /// 400 `{"error":"invalid_parameter","field":...}`
    BadParameter(&'static str),
    /// 400 `{"error":...}`
    Invalid(&'static str),
    /// 409 `{"error":...}`: another run holds what this needs.
    Busy(&'static str),
    Status(StatusCode),
}

impl From<StatusCode> for Refusal {
    fn from(status: StatusCode) -> Self {
        Self::Status(status)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::BadParameter(field) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_parameter", "field": field })),
            )
                .into_response(),
            Self::Invalid(error) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
            }
            Self::Busy(error) => {
                (StatusCode::CONFLICT, Json(json!({ "error": error }))).into_response()
            }
            Self::Status(status) => status.into_response(),
        }
    }
}

/// An unexpected failure: logged once, answered 500.
pub(crate) fn failed<E: Into<anyhow::Error>>(e: E) -> Refusal {
    Refusal::Status(super::server::internal(e))
}
