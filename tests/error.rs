use axum::http::StatusCode;
use axum::response::IntoResponse;

use uranium_engine::error::AppError;

#[test]
fn test_not_found_status() {
    let err = AppError::NotFound("missing".into());
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn test_bad_request_status() {
    let err = AppError::BadRequest("invalid input".into());
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_internal_status() {
    let err = AppError::Internal("something broke".into());
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn test_io_error_status() {
    let err = AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, "io error"));
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn test_database_error_status() {
    let err = AppError::Database(rusqlite::Error::InvalidParameterName("?1".into()));
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
