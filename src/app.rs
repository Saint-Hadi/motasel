//! Router and middleware wiring.

use axum::extract::DefaultBodyLimit;
use axum::http::Method;
use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::handlers::{delete_file, download_file, index, list_files, upload_file};
use crate::state::AppState;

/// Allowance for `multipart/form-data` framing (boundaries and per-part
/// headers) on top of the configured maximum file size.
const MULTIPART_OVERHEAD: u64 = 1024 * 1024;

/// Build the application router.
///
/// Routes match the documented API surface:
///   - `GET    /`                 embedded UI
///   - `GET    /api/files`        list files
///   - `POST   /api/upload`       multipart upload
///   - `GET    /api/download/:id` stream a file
///   - `DELETE /api/files/:id`    delete a file
///
/// CORS is permissive because every origin on the LAN is the server's own IP
/// and there is no cross-site state to protect; the app is explicitly a
/// trusted-network tool.
///
/// Axum caps request bodies at 2 MiB by default. That limit is replaced so
/// large uploads actually work, and is given headroom for multipart framing:
/// the authoritative per-file cap lives in `Storage::save_upload`, which counts
/// only the file part's bytes and answers with a proper `413` when exceeded.
///
/// Without this headroom Axum's own body limiter would reject an oversized
/// upload first with an opaque `400`, and the documented maximum upload size
/// could never actually be reached.
pub fn build_app(state: AppState) -> Router {
    let body_limit =
        DefaultBodyLimit::max(state.max_upload_bytes.saturating_add(MULTIPART_OVERHEAD) as usize);
    Router::new()
        .route("/", get(index))
        .route("/api/files", get(list_files))
        .route("/api/upload", post(upload_file))
        .route("/api/download/{id}", get(download_file))
        .route("/api/files/{id}", delete(delete_file))
        .layer(body_limit)
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST, Method::DELETE])
                .allow_origin(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}
