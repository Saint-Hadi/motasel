//! File storage layer.
//!
//! On-disk layout inside a single canonicalized root directory:
//!   - `<uuid>`           — raw uploaded bytes
//!   - `<uuid>.meta.json` — JSON metadata sidecar
//!
//! Security properties enforced here:
//!   - Internal names are always freshly generated UUIDv4 strings, so an
//!     attacker-controlled filename can never influence a filesystem path.
//!   - Every id is parsed as a `Uuid` before being used to build a path, which
//!     rejects `..`, `/`, and any other traversal payload outright.
//!   - Resolved paths are canonicalized and verified to stay inside the root, so
//!     a symlink planted in the storage directory cannot escape it.
//!   - Uploads stream to a `.part` temp file and are renamed into place, so a
//!     failed or oversized upload never overwrites or half-writes a real file.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::task::spawn_blocking;
use uuid::Uuid;

use crate::errors::{AppError, Result};
use crate::models::FileRecord;

/// Length of a canonical hyphenated UUIDv4 string.
const UUID_STR_LEN: usize = 36;

/// Size of the copy buffer used while streaming an upload to disk.
const STREAM_BUF_LEN: usize = 64 * 1024;

/// Suffix of the in-progress upload temp file.
const PART_SUFFIX: &str = ".part";

/// Suffix of the metadata sidecar file.
const META_SUFFIX: &str = ".meta.json";

/// Suffix of the temp file used while atomically writing a sidecar.
const META_TMP_SUFFIX: &str = ".meta.tmp.json";

/// A validated handle to the storage root, safe to share across requests.
#[derive(Debug, Clone)]
pub struct Storage {
    root: PathBuf,
    max_bytes: u64,
}

impl Storage {
    /// Create (if missing) and canonicalize the storage root.
    ///
    /// Canonicalizing up front means every later containment comparison is
    /// made against a fully resolved, symlink-free path.
    pub fn init<P: AsRef<Path>>(dir: P, max_bytes: u64) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)
            .map_err(|e| AppError::StorageError(format!("cannot create storage dir: {e}")))?;
        let root = std::fs::canonicalize(dir)
            .map_err(|e| AppError::StorageError(format!("cannot canonicalize storage dir: {e}")))?;
        Ok(Self { root, max_bytes })
    }

    /// The canonicalized storage root. Never returned to clients.
    pub fn storage_dir(&self) -> &Path {
        &self.root
    }

    /// Configured maximum upload size in bytes.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Stream an upload to disk and persist its metadata sidecar.
    ///
    /// Bytes are written to a `.part` temp file while counting against
    /// `max_bytes`; on success the temp file is renamed into place, which is
    /// atomic on the same filesystem and prevents accidental overwriting.
    pub async fn save_upload<S>(
        &self,
        filename: String,
        content_type: Option<String>,
        mut stream: S,
    ) -> Result<FileRecord>
    where
        S: AsyncRead + Unpin + Send,
    {
        let id = Uuid::new_v4().to_string();
        let dest = self.root.join(&id);
        let tmp = self.root.join(format!("{id}{PART_SUFFIX}"));

        let limit = self.max_bytes;
        let written = {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .truncate(true)
                .open(&tmp)
                .await?;

            let mut written: u64 = 0;
            let mut buf = vec![0u8; STREAM_BUF_LEN];
            loop {
                let n = stream.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                written = written.saturating_add(n as u64);
                if written > limit {
                    drop(file);
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(AppError::PayloadTooLarge { limit });
                }
                file.write_all(&buf[..n]).await?;
            }
            file.flush().await?;
            file.shutdown().await?;
            written
        };

        let actual = match tokio::fs::metadata(&tmp).await {
            Ok(m) => m.len(),
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e.into());
            }
        };

        if actual > limit {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(AppError::PayloadTooLarge { limit });
        }

        if written != actual {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(AppError::StorageError("upload byte count mismatch".into()));
        }

        tokio::fs::rename(&tmp, &dest).await?;

        let now = Utc::now();
        let record = FileRecord {
            id,
            filename,
            size: actual,
            content_type,
            created_at: now,
            modified_at: now,
        };

        let meta_path = self.meta_path_for(&record.id);
        if let Err(e) = persist_metadata(&meta_path, &record).await {
            // Roll back the data file so we never leave an unlisted orphan.
            let _ = tokio::fs::remove_file(&dest).await;
            return Err(e);
        }

        Ok(record)
    }

    /// List every stored file, newest first.
    pub async fn list(&self) -> Result<Vec<FileRecord>> {
        let root = self.root.clone();
        let records = spawn_blocking(move || -> std::io::Result<Vec<FileRecord>> {
            let mut out = Vec::new();
            for entry in std::fs::read_dir(&root)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy().into_owned();

                if !is_data_file_name(&name) {
                    continue;
                }
                let path = entry.path();
                if !is_contained(&path, &root) {
                    continue;
                }
                let meta_path = root.join(format!("{name}{META_SUFFIX}"));
                if let Some(rec) = load_metadata(&meta_path, &root) {
                    out.push(rec);
                }
            }
            out.sort_by_key(|a| std::cmp::Reverse(a.modified_at));
            Ok(out)
        })
        .await
        .map_err(|e| AppError::StorageError(e.to_string()))?;

        records.map_err(AppError::from)
    }

    /// Open a stored file for streaming download.
    ///
    /// The returned record's `size` reflects the real on-disk length so callers
    /// can emit an accurate `Content-Length`.
    pub async fn open_for_download(&self, id_str: &str) -> Result<(File, FileRecord)> {
        let id = validate_id(id_str)?;
        let dest = self.root.join(id.to_string());

        if !dest.try_exists()? {
            return Err(AppError::NotFound);
        }
        if !is_contained(&dest, &self.root) {
            return Err(AppError::NotFound);
        }

        let mut record =
            load_metadata(&self.meta_path_for(id_str), &self.root).unwrap_or_else(|| FileRecord {
                id: id.to_string(),
                filename: "download.bin".to_string(),
                size: 0,
                content_type: None,
                created_at: Utc::now(),
                modified_at: Utc::now(),
            });

        let file = File::open(&dest).await?;
        record.size = file
            .metadata()
            .await
            .map(|m| m.len())
            .unwrap_or(record.size);
        Ok((file, record))
    }

    /// Delete a stored file and its sidecar.
    ///
    /// Returns the removed metadata, or `None` if no such file existed.
    pub async fn delete(&self, id_str: &str) -> Result<Option<FileRecord>> {
        let id = validate_id(id_str)?;
        let dest = self.root.join(id.to_string());

        if !dest.try_exists()? {
            return Err(AppError::NotFound);
        }
        if !is_contained(&dest, &self.root) {
            return Err(AppError::NotFound);
        }

        let meta_path = self.meta_path_for(id_str);
        let rec = load_metadata(&meta_path, &self.root);

        tokio::fs::remove_file(&dest).await?;
        let _ = tokio::fs::remove_file(&meta_path).await;

        Ok(rec)
    }

    fn meta_path_for(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}{META_SUFFIX}"))
    }
}

/// Parse and validate an externally supplied file id.
///
/// Requiring a well-formed UUID is the primary traversal defence: any id
/// containing `.`, `/`, `\`, or NUL fails to parse and yields `NotFound`.
fn validate_id(id_str: &str) -> Result<Uuid> {
    if id_str.len() != UUID_STR_LEN {
        return Err(AppError::NotFound);
    }
    Uuid::parse_str(id_str).map_err(|_| AppError::NotFound)
}

/// True if `name` is exactly a UUID (a stored data file, not a sidecar).
fn is_data_file_name(name: &str) -> bool {
    name.len() == UUID_STR_LEN && Uuid::parse_str(name).is_ok()
}

/// True if `target` resolves to a location inside `root`.
///
/// Uses `std::fs::metadata` for a non-following symlink check first, then
/// canonicalizes so that a symlink pointing outside the root is rejected.
fn is_contained(target: &Path, root: &Path) -> bool {
    // Reject symlinks outright: uploaded content is always a regular file.
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            return false;
        }
    }
    let Ok(canonical) = std::fs::canonicalize(target) else {
        return false;
    };
    canonical.starts_with(root)
}

async fn persist_metadata(path: &Path, rec: &FileRecord) -> Result<()> {
    let json = serde_json::to_string(rec)?;
    let tmp = path.with_file_name(format!("{}{}", rec.id, META_TMP_SUFFIX));
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .await?;
        f.write_all(json.as_bytes()).await?;
        f.flush().await?;
    }
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

fn load_metadata(path: &Path, root: &Path) -> Option<FileRecord> {
    if !is_contained(path, root) {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<FileRecord>(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("motasel-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn save_then_list_then_download_roundtrip() {
        let root = temp_root("roundtrip");
        let store = Storage::init(&root, 1024).unwrap();

        let rec = store
            .save_upload("hello.txt".into(), Some("text/plain".into()), &b"hello"[..])
            .await
            .expect("upload should succeed");

        assert_eq!(rec.size, 5);
        assert_eq!(rec.filename, "hello.txt");

        let listed = store.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, rec.id);
        assert_eq!(listed[0].filename, "hello.txt");

        let (mut file, dl) = store.open_for_download(&rec.id).await.unwrap();
        assert_eq!(dl.size, 5);
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn delete_removes_data_and_sidecar() {
        let root = temp_root("delete");
        let store = Storage::init(&root, 1024).unwrap();

        let rec = store
            .save_upload("gone.txt".into(), None, &b"data"[..])
            .await
            .unwrap();

        let removed = store.delete(&rec.id).await.unwrap();
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().filename, "gone.txt");

        assert!(store.list().await.unwrap().is_empty());
        assert!(matches!(
            store.open_for_download(&rec.id).await,
            Err(AppError::NotFound)
        ));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn delete_missing_id_is_not_found() {
        let root = temp_root("delete-missing");
        let store = Storage::init(&root, 1024).unwrap();
        let id = Uuid::new_v4().to_string();
        assert!(matches!(store.delete(&id).await, Err(AppError::NotFound)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn oversized_upload_is_rejected_and_leaves_no_partial_file() {
        let root = temp_root("oversize");
        let store = Storage::init(&root, 4).unwrap();

        let res = store
            .save_upload("big.bin".into(), None, &b"abcdefgh"[..])
            .await;
        assert!(matches!(res, Err(AppError::PayloadTooLarge { .. })));

        // Nothing at all should remain in the storage directory.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn exactly_at_limit_is_accepted() {
        let root = temp_root("limit-exact");
        let store = Storage::init(&root, 4).unwrap();
        let rec = store
            .save_upload("exact.bin".into(), None, &b"abcd"[..])
            .await
            .expect("4 bytes at a 4-byte limit should be accepted");
        assert_eq!(rec.size, 4);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn traversal_ids_are_rejected() {
        for evil in [
            "../secret",
            "../../etc/passwd",
            "/etc/passwd",
            "..%2f..%2fetc",
            "foo/bar",
            "\\..\\windows\\system32",
            "00000000-0000-0000-0000-000000000000/../../etc",
            "",
            ".",
            "..",
        ] {
            assert!(
                validate_id(evil).is_err(),
                "id {evil:?} should have been rejected"
            );
        }
    }

    #[test]
    fn valid_uuid_id_is_accepted() {
        let id = Uuid::new_v4().to_string();
        assert!(validate_id(&id).is_ok());
    }

    #[test]
    fn data_file_name_only_matches_uuids() {
        let id = Uuid::new_v4().to_string();
        assert!(is_data_file_name(&id));
        assert!(!is_data_file_name(&format!("{id}{META_SUFFIX}")));
        assert!(!is_data_file_name(&format!("{id}{PART_SUFFIX}")));
        assert!(!is_data_file_name("../escape"));
        assert!(!is_data_file_name("not-a-uuid"));
    }

    #[tokio::test]
    async fn symlink_inside_storage_cannot_escape_root() {
        let root = temp_root("symlink");
        let store = Storage::init(&root, 1024).unwrap();

        // A secret that lives *outside* the storage root.
        let outside = temp_root("outside-secret");
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();

        // Plant a symlink in the storage root named like a valid UUID file.
        let id = Uuid::new_v4().to_string();
        let link = root.join(&id);
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        // It must not be downloadable.
        assert!(matches!(
            store.open_for_download(&id).await,
            Err(AppError::NotFound)
        ));

        // And it must not show up in listings either.
        assert!(store.list().await.unwrap().is_empty());

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[tokio::test]
    async fn hostile_filename_is_stored_as_metadata_only() {
        let root = temp_root("hostile-name");
        let store = Storage::init(&root, 1024).unwrap();

        let hostile = "../../../../etc/passwd".to_string();
        let rec = store
            .save_upload(hostile.clone(), None, &b"payload"[..])
            .await
            .unwrap();

        // The metadata keeps whatever the client claimed, but the on-disk name
        // is a UUID, so no path outside the root was ever touched.
        assert_eq!(rec.filename, hostile);
        assert_eq!(rec.id.len(), UUID_STR_LEN);

        let entries: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.iter().all(|n| !n.contains("..")));
        assert!(entries.iter().all(|n| !n.contains('/')));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn sidecar_without_data_file_is_not_listed() {
        let root = temp_root("orphan-sidecar");
        let store = Storage::init(&root, 1024).unwrap();

        let id = Uuid::new_v4().to_string();
        let sidecar = root.join(format!("{id}{META_SUFFIX}"));
        std::fs::write(
            &sidecar,
            serde_json::to_string(&FileRecord {
                id: id.clone(),
                filename: "ghost.txt".into(),
                size: 1,
                content_type: None,
                created_at: Utc::now(),
                modified_at: Utc::now(),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(store.list().await.unwrap().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn corrupt_sidecar_is_skipped_not_fatal() {
        let root = temp_root("corrupt-sidecar");
        let store = Storage::init(&root, 1024).unwrap();

        let rec = store
            .save_upload("ok.txt".into(), None, &b"ok"[..])
            .await
            .unwrap();

        let id = Uuid::new_v4().to_string();
        std::fs::write(root.join(&id), b"real bytes").unwrap();
        std::fs::write(root.join(format!("{id}{META_SUFFIX}")), b"{not json").unwrap();

        let listed = store.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, rec.id);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn concurrent_uploads_get_distinct_ids() {
        let root = temp_root("concurrent");
        let store = Storage::init(&root, 1024).unwrap();

        let mut handles = Vec::new();
        for i in 0..8u8 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                s.save_upload(format!("f{i}.txt"), None, &[i; 16][..])
                    .await
                    .unwrap()
            }));
        }

        let mut ids = Vec::new();
        for h in handles {
            ids.push(h.await.unwrap().id);
        }
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8, "all uploads must have distinct ids");
        assert_eq!(store.list().await.unwrap().len(), 8);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
