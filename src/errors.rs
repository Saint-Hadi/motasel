use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::models::ErrorResponse;

pub type Result<T> = std::result::Result<T, AppError>;

/// Application-level errors. Each maps to a sensible HTTP status code and a
/// JSON body; no internal details leak to the client.
#[derive(Debug)]
#[allow(dead_code)]
pub enum AppError {
    NotFound,
    PayloadTooLarge { limit: u64 },
    InvalidUpload,
    StorageError(String),
    Io(String),
    Conflict,
    BadRequest(String),
}

impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::InvalidUpload => StatusCode::BAD_REQUEST,
            Self::Conflict => StatusCode::CONFLICT,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::StorageError(_) | Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::NotFound => "Resource not found.".to_string(),
            Self::PayloadTooLarge { .. } => {
                "Uploaded file exceeds the maximum allowed size.".to_string()
            }
            Self::InvalidUpload => "Malformed multipart upload.".to_string(),
            Self::Conflict => "A file with this ID already exists.".to_string(),
            Self::BadRequest(msg) => msg.clone(),
            Self::StorageError(_) | Self::Io(_) => {
                "An internal storage error occurred.".to_string()
            }
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::StorageError(e.to_string())
    }
}

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(_e: axum::extract::multipart::MultipartError) -> Self {
        Self::InvalidUpload
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::StorageError(e.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let payload = ErrorResponse {
            error: self.message(),
            code: status.as_u16(),
        };
        (status, Json(payload)).into_response()
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for AppError {}
