use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Unified error type returned by all API handlers.
///
/// Each variant maps to an HTTP status code and a JSON body:
/// ```json
/// {"error": "<message>"}
/// ```
///
/// | Variant | Status |
/// |---|---|
/// | [`NotFound`](AppError::NotFound) | 404 |
/// | [`BadRequest`](AppError::BadRequest) | 400 |
/// | [`Database`](AppError::Database) | 500 |
/// | [`Uranium`](AppError::Uranium) | 500 |
/// | [`Internal`](AppError::Internal) | 500 |
/// | [`Io`](AppError::Io) | 500 |
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The requested resource does not exist.
    #[error("Not found: {0}")]
    NotFound(String),

    /// The request was malformed or the operation is invalid for the current
    /// resource state.
    #[error("Bad request: {0}")]
    BadRequest(String),

    /// An internal SQLite error occurred.
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// An error from the underlying `uranium-rs` download or runtime library.
    #[error("Uranium error: {0}")]
    Uranium(#[from] uranium_rs::error::UraniumError),

    /// An unexpected internal error.
    #[error("Internal error: {0}")]
    Internal(String),

    /// An I/O error (file system, etc.).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            Self::Database(e) => {
                tracing::error!("Database error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error".into())
            }
            Self::Uranium(e) => {
                tracing::error!("Uranium error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Uranium error: {e}"),
                )
            }
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            Self::Io(e) => {
                tracing::error!("IO error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "IO error".into())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
