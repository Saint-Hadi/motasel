use std::path::PathBuf;

use crate::errors::Result;

/// All tunable application configuration, sourced entirely from environment
/// variables so that the binary is self-contained and requires no external
/// config files at runtime.
#[derive(Debug, Clone)]
pub struct Config {
    /// Local IPv4 bind address. Always `0.0.0.0` so LAN peers can reach us.
    pub bind_addr: String,
    /// TCP port to listen on.
    pub port: u16,
    /// Directory in which uploaded files are persisted.
    pub storage_dir: PathBuf,
    /// Maximum accepted body size for a single upload (bytes).
    pub max_upload_bytes: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let port = std::env::var("FILESHARE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(8080);

        let storage_dir = std::env::var("FILESHARE_STORAGE_DIR")
            .ok()
            .unwrap_or_else(|| "storage".to_string());
        let storage_dir = PathBuf::from(storage_dir);

        let max_upload_bytes = std::env::var("FILESHARE_MAX_UPLOAD_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1_073_741_824); // 1 GiB

        Ok(Self {
            bind_addr: "0.0.0.0".to_string(),
            port,
            storage_dir,
            max_upload_bytes,
        })
    }

    /// `127.0.0.1:PORT` is **not** used; we return `0.0.0.0:PORT` so LAN peers
    /// can connect.
    pub fn listen_addr(&self) -> String {
        format!("{}:{}", self.bind_addr, self.port)
    }
}
