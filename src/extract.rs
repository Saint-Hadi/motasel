//! Upload size limiting.
//!
//! The authoritative per-file cap lives in [`crate::storage::Storage::save_upload`],
//! which counts bytes as they stream to disk and aborts with
//! `AppError::PayloadTooLarge` (HTTP 413) when exceeded. This module adds a
//! cheap fast-fail so a client that *declares* an oversized `Content-Length` is
//! rejected before we parse the multipart boundary at all.

use axum::http::{header, HeaderMap};

/// Reject declared bodies above 2 GiB regardless of configuration, so a bogus
/// `Content-Length` can never ask us to buffer an absurd amount.
const SANITY_CEILING: u64 = 2 * 1024 * 1024 * 1024;

/// True when the request's declared `Content-Length` already exceeds the
/// configured maximum (or the hard sanity ceiling).
pub fn content_length_exceeds(headers: &HeaderMap, max_bytes: u64) -> bool {
    let Some(len) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    else {
        // Chunked transfer without a declared length: let the streaming
        // counter in `save_upload` enforce the limit.
        return false;
    };

    len > max_bytes || len > SANITY_CEILING
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_len(len: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_str(len).unwrap());
        h
    }

    #[test]
    fn under_limit_is_allowed() {
        assert!(!content_length_exceeds(&headers_with_len("1024"), 2048));
        assert!(!content_length_exceeds(&headers_with_len("2048"), 2048));
    }

    #[test]
    fn over_limit_is_rejected() {
        assert!(content_length_exceeds(&headers_with_len("2049"), 2048));
        assert!(content_length_exceeds(&headers_with_len("999999999"), 100));
    }

    #[test]
    fn missing_content_length_is_allowed_through() {
        assert!(!content_length_exceeds(&HeaderMap::new(), 100));
    }

    #[test]
    fn malformed_content_length_is_allowed_through() {
        // An unparseable length cannot be trusted; the streaming counter
        // remains the real guard.
        assert!(!content_length_exceeds(&headers_with_len("abc"), 100));
    }

    #[test]
    fn sanity_ceiling_applies_even_with_huge_config() {
        let huge = u64::MAX;
        assert!(content_length_exceeds(
            &headers_with_len("3000000000"),
            huge
        ));
    }
}
