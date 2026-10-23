use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metadata persisted alongside each uploaded file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub filename: String,
    pub size: u64,
    pub content_type: Option<String>,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
}

/// DTO returned by `GET /api/files`.
#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub id: String,
    pub filename: String,
    pub size: u64,
    pub size_human: String,
    pub content_type: Option<String>,
    pub created_at: String,
    pub modified_at: String,
}

impl From<FileRecord> for FileEntry {
    fn from(rec: FileRecord) -> Self {
        Self {
            id: rec.id,
            filename: sanitize_for_display(&rec.filename),
            size: rec.size,
            size_human: human_size(rec.size),
            content_type: rec.content_type,
            created_at: rec.created_at.to_rfc3339(),
            modified_at: rec.modified_at.to_rfc3339(),
        }
    }
}

/// DTO returned by `POST /api/upload`.
#[derive(Debug, Serialize)]
pub struct UploadResult {
    pub id: String,
    pub filename: String,
    pub size: u64,
}

/// Error body returned to clients.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: u16,
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.2} {}", size, UNITS[unit])
    }
}

/// Strip control characters and null bytes from a filename destined for
/// display/JSON. This does not touch the on-disk storage path (which is a UUID).
pub fn sanitize_for_display(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_control() || ch == '\0' {
            continue;
        }
        out.push(ch);
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        "(unknown)".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_bytes() {
        let raw = "hello\x00\x7f\x01world";
        assert_eq!(sanitize_for_display(raw), "helloworld");
        assert_eq!(sanitize_for_display("   "), "(unknown)");
        assert_eq!(sanitize_for_display(""), "(unknown)");
    }

    #[test]
    fn human_size_formats_correctly() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.00 KiB");
        assert_eq!(human_size(1_048_576), "1.00 MiB");
    }
}
