//! Integration tests exercising the full HTTP stack in-process.
//!
//! Requests are driven through `tower::ServiceExt::oneshot` against the real
//! router, so routing, extractors, middleware, and handlers are all covered
//! without binding a socket.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use motasel::{build_app, AppState, Config};

const BOUNDARY: &str = "----motaselIntegrationBoundary";

/// Build a router backed by a fresh, unique temporary storage directory.
fn app(max_upload_bytes: u64) -> (axum::Router, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("motasel-it-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let config = Config {
        bind_addr: "0.0.0.0".to_string(),
        port: 0,
        storage_dir: dir.clone(),
        max_upload_bytes,
    };
    let state = AppState::init(config).unwrap();
    (build_app(state), dir)
}

/// Build a `multipart/form-data` body containing one `file` field.
fn multipart_body(filename: &str, content_type: &str, content: &[u8]) -> Body {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    Body::from(body)
}

fn upload_request(filename: &str, content_type: &str, content: &[u8]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/upload")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(multipart_body(filename, content_type, content))
        .unwrap()
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn body_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

/// Upload a file and return its id.
async fn upload_ok(router: &axum::Router, filename: &str, content: &[u8]) -> (String, Value) {
    let response = router
        .clone()
        .oneshot(upload_request(filename, "text/plain", content))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "upload should succeed"
    );
    let json = body_json(response).await;
    let id = json["id"].as_str().unwrap().to_string();
    (id, json)
}

#[tokio::test]
async fn index_serves_embedded_html() {
    let (router, dir) = app(1024 * 1024);
    let response = router
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );

    let html = String::from_utf8(body_bytes(response).await).unwrap();
    assert!(html.contains("motasel"), "UI should be served");
    assert!(
        html.contains(r#"id="dz""#),
        "UI should include the dropzone"
    );
    assert!(
        html.contains(r#"id="picker""#),
        "UI should include the file picker"
    );
    // The UI composes every endpoint from a single base constant; assert the
    // exact call sites so a broken asset fails the build rather than the LAN.
    assert!(
        html.contains(r#"const API = "/api""#),
        "UI should target the /api base"
    );
    for call_site in [
        r#"API + "/files""#,
        r#"API + "/upload""#,
        r#"API + "/download/""#,
    ] {
        assert!(
            html.contains(call_site),
            "UI is missing the call site {call_site}"
        );
    }
    // Delete must be a DELETE request, and it must be confirmed first.
    assert!(
        html.contains(r#"{ method: "DELETE" }"#),
        "UI should delete via the DELETE method"
    );
    // Delete confirmation must be present before anything is removed.
    assert!(
        html.contains("Delete this file?"),
        "UI should confirm before deleting"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn empty_file_list_is_empty_array() {
    let (router, dir) = app(1024 * 1024);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json, serde_json::json!([]));

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn upload_list_download_delete_roundtrip() {
    let (router, dir) = app(1024 * 1024);
    let content = b"hello from the LAN";

    let (id, json) = upload_ok(&router, "notes.txt", content).await;
    assert_eq!(json["filename"], "notes.txt");
    assert_eq!(json["size"], content.len() as u64);

    // List
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(response).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let entry = &list[0];
    assert_eq!(entry["id"], id);
    assert_eq!(entry["filename"], "notes.txt");
    assert_eq!(entry["size"], content.len() as u64);
    assert!(entry["size_human"].as_str().unwrap().contains('B'));
    assert!(entry["modified_at"].as_str().unwrap().contains('T'));

    // Download
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/plain");
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        content.len().to_string()
    );
    let disposition = response.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(disposition.contains("notes"), "{disposition}");
    assert_eq!(body_bytes(response).await, content);

    // Delete
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/files/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Gone
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(response).await, serde_json::json!([]));

    // And not downloadable any more
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn binary_content_survives_roundtrip_intact() {
    let (router, dir) = app(1024 * 1024);
    // Every byte value, including NUL and high bytes.
    let content: Vec<u8> = (0..=255u8).cycle().take(4096).collect();

    let (id, _) = upload_ok(&router, "binary.bin", &content).await;

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_bytes(response).await, content);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn large_upload_exceeding_default_axum_limit_works() {
    // 4 MiB is well past axum's 2 MiB default body limit; it must still work
    // because the app raises the limit to its configured maximum.
    let (router, dir) = app(16 * 1024 * 1024);
    let content = vec![0xABu8; 4 * 1024 * 1024];

    let (id, json) = upload_ok(&router, "big.bin", &content).await;
    assert_eq!(json["size"].as_u64().unwrap(), content.len() as u64);

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, content);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn oversized_upload_returns_413_json_error() {
    let (router, dir) = app(16);
    let content = vec![b'x'; 4096];

    let response = router
        .clone()
        .oneshot(upload_request(
            "too-big.bin",
            "application/octet-stream",
            &content,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let json = body_json(response).await;
    assert_eq!(json["code"], 413);
    assert!(json["error"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("size"));

    // Storage must remain empty; no partial file leaked.
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(response).await, serde_json::json!([]));

    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn upload_without_file_field_returns_400() {
    let (router, dir) = app(1024 * 1024);

    let body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nnope\r\n--{BOUNDARY}--\r\n"
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert_eq!(json["code"], 400);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn upload_with_non_multipart_body_returns_4xx() {
    let (router, dir) = app(1024 * 1024);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from("not multipart"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response.status().is_client_error(),
        "expected 4xx, got {}",
        response.status()
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn download_missing_id_returns_404_json() {
    let (router, dir) = app(1024 * 1024);
    let missing = uuid::Uuid::new_v4().to_string();

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{missing}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let json = body_json(response).await;
    assert_eq!(json["code"], 404);
    assert!(json["error"].is_string());

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn delete_missing_id_returns_404_json() {
    let (router, dir) = app(1024 * 1024);
    let missing = uuid::Uuid::new_v4().to_string();

    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/files/{missing}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await["code"], 404);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn path_traversal_ids_return_404_and_never_touch_disk() {
    let (router, dir) = app(1024 * 1024);

    // A real secret living outside the storage directory.
    let outside = dir
        .parent()
        .unwrap()
        .join(format!("motasel-secret-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"do not read").unwrap();

    let traversals = [
        "..%2f..%2fetc%2fpasswd",
        "..%2Fsecret.txt",
        "%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        "....//....//etc/passwd",
        "not-a-uuid",
        ".",
        "-",
    ];

    for payload in traversals {
        for method in ["GET", "DELETE"] {
            let uri = if method == "GET" {
                format!("/api/download/{payload}")
            } else {
                format!("/api/files/{payload}")
            };
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(&uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                response.status() == StatusCode::NOT_FOUND
                    || response.status() == StatusCode::BAD_REQUEST,
                "{method} {uri} -> expected 404/400, got {}",
                response.status()
            );
            assert_ne!(
                response.status(),
                StatusCode::OK,
                "{method} {uri} must not succeed"
            );
        }
    }

    // The secret is untouched.
    assert_eq!(
        std::fs::read(outside.join("secret.txt")).unwrap(),
        b"do not read"
    );

    std::fs::remove_dir_all(&outside).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn hostile_filename_does_not_escape_storage_dir() {
    let (router, dir) = app(1024 * 1024);

    let hostile = "../../../../tmp/motasel-pwned-escape.txt";
    let (id, json) = upload_ok(&router, hostile, b"payload").await;
    assert_eq!(json["id"].as_str().unwrap(), id);

    // Nothing was written outside the storage root.
    assert!(!std::path::Path::new("/tmp/motasel-pwned-escape.txt").exists());

    // Only the UUID data file and its sidecar exist in storage.
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries.len(), 2, "entries: {entries:?}");
    assert!(entries.iter().all(|n| n.starts_with(&id)));

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn api_never_leaks_server_filesystem_paths() {
    let (router, dir) = app(1024 * 1024);
    let (id, _) = upload_ok(&router, "leak-check.txt", b"data").await;

    let dir_str = dir.to_string_lossy().into_owned();

    // Collect every byte the API can emit for this file.
    let mut payloads = Vec::new();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    payloads.push(body_bytes(response).await);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    for value in response.headers().values() {
        payloads.push(value.as_bytes().to_vec());
    }
    payloads.push(body_bytes(response).await);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{}", uuid::Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    payloads.push(body_bytes(response).await);

    for payload in payloads {
        let text = String::from_utf8_lossy(&payload);
        assert!(
            !text.contains(&dir_str),
            "storage path leaked in response: {text}"
        );
        assert!(
            !text.contains("/tmp/"),
            "absolute temp path leaked in response: {text}"
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn duplicate_filenames_coexist_without_overwriting() {
    let (router, dir) = app(1024 * 1024);

    let (id1, _) = upload_ok(&router, "same.txt", b"first").await;
    let (id2, _) = upload_ok(&router, "same.txt", b"second").await;
    assert_ne!(id1, id2, "each upload must get its own id");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(response).await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    // Both payloads are intact and independent.
    for (id, expected) in [(&id1, &b"first"[..]), (&id2, &b"second"[..])] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/download/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_bytes(response).await, expected);
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn non_ascii_filename_is_preserved_in_metadata() {
    let (router, dir) = app(1024 * 1024);

    let name = "文档-ünïcødé 🎉.pdf";
    let (id, json) = upload_ok(&router, name, b"pdf-bytes").await;
    assert_eq!(json["filename"].as_str().unwrap(), name);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(response).await;
    assert_eq!(list[0]["filename"].as_str().unwrap(), name);

    // Content-Disposition must stay pure ASCII.
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/download/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let disposition = response.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.is_ascii(), "{disposition}");

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn unknown_api_route_returns_404() {
    let (router, dir) = app(1024 * 1024);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn wrong_method_on_files_returns_405() {
    let (router, dir) = app(1024 * 1024);

    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn multiple_files_are_ordered_newest_first() {
    let (router, dir) = app(1024 * 1024);

    let mut ids = Vec::new();
    for i in 0..3 {
        let (id, _) = upload_ok(&router, &format!("f{i}.txt"), b"x").await;
        ids.push(id);
        // Ensure distinct mtimes so ordering is deterministic.
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }

    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/files")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(response).await;
    let listed: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(listed.len(), 3);
    // Newest first => reverse of upload order.
    let mut expected = ids.clone();
    expected.reverse();
    assert_eq!(listed, expected);

    std::fs::remove_dir_all(dir).unwrap();
}
