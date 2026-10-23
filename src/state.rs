//! Shared application state.

use std::path::Path;
use std::sync::Arc;

use crate::config::Config;
use crate::errors::Result;
use crate::storage::Storage;

/// State shared by every request handler, cloned cheaply via `Arc`.
///
/// Constructed once at startup and handed to Axum through `with_state`.
#[derive(Clone)]
pub struct AppState {
    /// File storage backend.
    pub storage: Arc<Storage>,
    /// Configured maximum upload size, mirrored here so handlers can reject
    /// oversized bodies before touching the filesystem.
    pub max_upload_bytes: u64,
}

impl AppState {
    /// Build state from configuration, initializing the storage directory.
    pub fn init(config: Config) -> Result<Self> {
        let max_upload_bytes = config.max_upload_bytes;
        let storage = Arc::new(Storage::init(&config.storage_dir, max_upload_bytes)?);
        Ok(Self {
            storage,
            max_upload_bytes,
        })
    }

    /// The canonicalized storage directory. Never exposed through the API.
    pub fn storage_dir(&self) -> &Path {
        self.storage.storage_dir()
    }
}
