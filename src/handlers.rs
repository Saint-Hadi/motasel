//! HTTP/API layer.
//!
//! Handlers are intentionally thin: they translate between HTTP and the
//! [`crate::storage::Storage`] backend, and map errors to JSON responses via
//! [`crate::errors::AppError`].

use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::stream::TryStreamExt;
use tokio_util::io::{ReaderStream, StreamReader};

use crate::errors::{AppError, Result};
use crate::extract::content_length_exceeds;
use crate::models::{FileEntry, UploadResult};
use crate::state::AppState;

/// The single-page UI, embedded at compile time so the binary needs no
/// companion asset files and no build toolchain at runtime.
pub const INDEX_HTML: &str = include_str!("../assets/index.html");

/// MIME type served for the embedded UI.
const HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";

/// Buffer size used when streaming a file from disk to the client.
const DOWNLOAD_CHUNK: usize = 64 * 1024;

/// `GET /` — serve the embedded UI.
pub async fn index() -> Response {
    ([(header::CONTENT_TYPE, HTML_CONTENT_TYPE)], INDEX_HTML).into_response()
}

/// `GET /api/files` — list stored files, newest first.
pub async fn list_files(State(state): State<AppState>) -> Result<Json<Vec<FileEntry>>> {
    let records = state.storage.list().await?;
    Ok(Json(records.into_iter().map(FileEntry::from).collect()))
}

/// `GET /api/download/:id` — stream a file as an attachment.
pub async fn download_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response> {
    let (file, record) = state.storage.open_for_download(&id).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header_value_or_octet_stream(record.content_type.as_deref())?,
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(record.size));
    headers.insert(
        header::CONTENT_DISPOSITION,
        attachment_disposition(&record.filename)?,
    );

    let stream = ReaderStream::with_capacity(file, DOWNLOAD_CHUNK);
    Ok((headers, Body::from_stream(stream)).into_response())
}

/// `POST /api/upload` — accept one multipart `file` field and stream it to disk.
pub async fn upload_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResult>)> {
    // Fast-fail on a declared body that already exceeds the configured cap.
    if content_length_exceeds(&headers, state.max_upload_bytes) {
        tracing::debug!(limit = state.max_upload_bytes, "rejecting oversized upload");
        return Err(AppError::PayloadTooLarge {
            limit: state.max_upload_bytes,
        });
    }

    while let Some(field) = multipart.next_field().await? {
        if field.name() != Some("file") {
            continue;
        }

        let filename = sanitize_upload_filename(field.file_name().unwrap_or(""));
        let content_type = field.content_type().map(str::to_owned);

        // `Field` is a `Stream<Item = Result<Bytes, MultipartError>>`; adapt it
        // to an `AsyncRead` so storage can stream it to disk chunk by chunk.
        let reader = StreamReader::new(
            field.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        );

        let record = state
            .storage
            .save_upload(filename, content_type, reader)
            .await?;

        return Ok((
            StatusCode::CREATED,
            Json(UploadResult {
                id: record.id,
                filename: record.filename,
                size: record.size,
            }),
        ));
    }

    Err(AppError::InvalidUpload)
}

/// `DELETE /api/files/:id` — remove a stored file.
pub async fn delete_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let removed = state.storage.delete(&id).await?;
    match removed {
        Some(rec) => {
            tracing::debug!(id = %rec.id, "deleted file");
            Ok(StatusCode::NO_CONTENT)
        }
        None => Ok(StatusCode::NOT_FOUND),
    }
}

/// Build an RFC 6266 `attachment` disposition with a UTF-8 encoded filename.
fn attachment_disposition(filename: &str) -> Result<HeaderValue> {
    let safe = sanitize_http_filename(filename);
    let encoded = percent_encoding::utf8_percent_encode(&safe, percent_encoding::NON_ALPHANUMERIC);
    HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{encoded}"))
        .map_err(|_| AppError::StorageError("unencodable filename".into()))
}

fn header_value_or_octet_stream(content_type: Option<&str>) -> Result<HeaderValue> {
    let ct = content_type.unwrap_or("application/octet-stream");
    HeaderValue::from_str(ct).map_err(|_| AppError::StorageError("unencodable content type".into()))
}

/// Reduce an untrusted client filename to a display-safe base name.
///
/// Strips any directory components and control characters. The result is only
/// ever stored as metadata and echoed back to clients — never used to build a
/// filesystem path.
pub fn sanitize_upload_filename(raw: &str) -> String {
    // Strip both `/` and `\` separators explicitly rather than relying on
    // `Path::file_name`, which only knows the *host* separator. A Windows
    // browser may send `C:\Users\me\report.pdf`; on this Linux server
    // `Path` would leave that string intact.
    let base = raw.trim().rsplit(['/', '\\']).next().unwrap_or_default();

    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '\0')
        .collect();
    let cleaned = cleaned.trim();

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "unnamed".to_string()
    } else {
        cleaned.chars().take(200).collect()
    }
}

/// Sanitize a filename destined for an HTTP header.
///
/// Drops characters that could break out of the header value or inject an
/// additional parameter.
pub fn sanitize_http_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && *c != '\0' && *c != '"' && *c != '\\')
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned.chars().take(200).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_components_from_uploaded_filename() {
        assert_eq!(sanitize_upload_filename("../../../../etc/passwd"), "passwd");
        assert_eq!(sanitize_upload_filename("/etc/shadow"), "shadow");
        assert_eq!(sanitize_upload_filename("C:\\Users\\me\\x.txt"), "x.txt");
        assert_eq!(sanitize_upload_filename("normal.pdf"), "normal.pdf");
    }

    #[test]
    fn strips_control_characters() {
        assert_eq!(sanitize_upload_filename("a\u{0}b\u{7}c.txt"), "abc.txt");
    }

    #[test]
    fn degenerate_names_become_unnamed() {
        assert_eq!(sanitize_upload_filename(""), "unnamed");
        assert_eq!(sanitize_upload_filename("   "), "unnamed");
        assert_eq!(sanitize_upload_filename(".."), "unnamed");
        assert_eq!(sanitize_upload_filename("/"), "unnamed");
    }

    #[test]
    fn long_names_are_truncated() {
        let long = "a".repeat(500);
        assert_eq!(sanitize_upload_filename(&long).len(), 200);
    }

    #[test]
    fn http_filename_drops_header_breakers() {
        assert_eq!(
            sanitize_http_filename("evil\"; filename=\"x.txt"),
            "evil; filename=x.txt"
        );
        assert_eq!(sanitize_http_filename(""), "download");
        assert_eq!(sanitize_http_filename("\r\nInjected: 1"), "Injected: 1");
    }

    #[test]
    fn attachment_disposition_is_valid_header() {
        let hv = attachment_disposition("my file.txt").unwrap();
        let s = hv.to_str().unwrap();
        assert!(s.starts_with("attachment; filename*=UTF-8''"));
        // Space must be encoded; the whole name is percent-encoded so it is
        // safe inside a header value.
        assert!(s.contains("my%20file"), "unexpected disposition: {s}");
    }

    #[test]
    fn non_ascii_filename_is_percent_encoded() {
        let hv = attachment_disposition("文档.pdf").unwrap();
        let s = hv.to_str().unwrap();
        // Must be pure ASCII to live in a header value.
        assert!(s.is_ascii(), "header value must be ASCII: {s}");
    }
}
