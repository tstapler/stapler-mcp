//! Per-session network capture log, fed by the CDP `Network.*` listeners in
//! `browser.rs`. Pure state-machine logic lives here so the cap, binary-skip,
//! truncation, filter and clear rules are unit-testable without a browser.

use std::collections::HashMap;

use stapler_mcp_core::ports::{NetworkEntry, NetworkRequestsResult};

/// Hard cap on completed entries per session; later entries are dropped and
/// `truncated` is set.
pub const MAX_NETWORK_ENTRIES: usize = 1000;
/// Per-body cap, and the ceiling a caller's `max_body_bytes` is clamped to.
pub const MAX_BODY_BYTES: usize = 16_384;

/// `true` for content types whose bodies are never buffered.
pub fn is_binary_content_type(content_type: &str) -> bool {
    let ct = content_type.trim().to_ascii_lowercase();
    ["image/", "font/", "audio/", "video/"]
        .iter()
        .any(|p| ct.starts_with(p))
        || ct.starts_with("application/octet-stream")
}

/// Cuts `s` to at most `max` bytes on a UTF-8 boundary; the bool is `true`
/// if anything was removed.
pub fn truncate_utf8(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

struct Partial {
    url: String,
    method: String,
    started_at_ms: u64,
    started_monotonic_s: f64,
    status: Option<u16>,
    content_type: Option<String>,
    response_seen: bool,
}

#[derive(Default)]
pub struct NetworkLog {
    partial: HashMap<String, Partial>,
    entries: Vec<NetworkEntry>,
    truncated: bool,
}

impl NetworkLog {
    /// Records the start of a request. A repeated `request_id` is a redirect
    /// hop: the previous hop is finalised first with `redirect_status`.
    pub fn on_request(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        started_at_ms: u64,
        monotonic_s: f64,
        redirect_status: Option<u16>,
    ) {
        if let Some(prev) = self.partial.remove(request_id) {
            let end = monotonic_s;
            self.push(Self::finish(prev, end, redirect_status, None));
        }
        self.partial.insert(
            request_id.to_string(),
            Partial {
                url: url.to_string(),
                method: method.to_string(),
                started_at_ms,
                started_monotonic_s: monotonic_s,
                status: None,
                content_type: None,
                response_seen: false,
            },
        );
    }

    pub fn on_response(&mut self, request_id: &str, status: Option<u16>, content_type: &str) {
        if let Some(p) = self.partial.get_mut(request_id) {
            p.status = status;
            p.response_seen = true;
            if !content_type.is_empty() {
                p.content_type = Some(content_type.to_string());
            }
        }
    }

    /// Whether `responseReceived` has been recorded for `request_id`. The
    /// `Network.*` events arrive on separate streams, so a `loadingFinished`
    /// handler uses this to wait for its response to land first.
    pub fn has_response(&self, request_id: &str) -> bool {
        self.partial
            .get(request_id)
            .is_some_and(|p| p.response_seen)
    }

    /// Whether `Network.getResponseBody` is worth calling for `request_id`:
    /// the request is known, the cap has room, and the type isn't binary.
    pub fn wants_body(&self, request_id: &str) -> bool {
        if self.entries.len() >= MAX_NETWORK_ENTRIES {
            return false;
        }
        self.partial.get(request_id).is_some_and(|p| {
            p.content_type
                .as_deref()
                .is_some_and(|ct| !is_binary_content_type(ct))
        })
    }

    /// Finalises `request_id` into an entry. `body` is ignored for binary
    /// content types.
    pub fn complete(&mut self, request_id: &str, monotonic_s: f64, body: Option<String>) {
        if let Some(p) = self.partial.remove(request_id) {
            let status = p.status;
            self.push(Self::finish(p, monotonic_s, status, body));
        }
    }

    /// Finalises a request that never produced a response (network error,
    /// blocked, cancelled).
    pub fn on_failed(&mut self, request_id: &str, monotonic_s: f64) {
        if let Some(p) = self.partial.remove(request_id) {
            let status = p.status;
            self.push(Self::finish(p, monotonic_s, status, None));
        }
    }

    fn finish(p: Partial, end_s: f64, status: Option<u16>, body: Option<String>) -> NetworkEntry {
        let binary = p
            .content_type
            .as_deref()
            .is_some_and(is_binary_content_type);
        let (body, body_truncated) = match body {
            Some(b) if !binary => {
                let (b, t) = truncate_utf8(&b, MAX_BODY_BYTES);
                (Some(b), t)
            }
            _ => (None, false),
        };
        let timing_ms = if end_s >= p.started_monotonic_s {
            Some(((end_s - p.started_monotonic_s) * 1000.0).round() as u64)
        } else {
            None
        };
        NetworkEntry {
            url: p.url,
            method: p.method,
            status,
            content_type: p.content_type,
            body,
            body_truncated,
            timing_ms,
            started_at_ms: p.started_at_ms,
        }
    }

    fn push(&mut self, entry: NetworkEntry) {
        if self.entries.len() >= MAX_NETWORK_ENTRIES {
            self.truncated = true;
            return;
        }
        self.entries.push(entry);
    }

    /// Drops everything, including in-flight requests; called on `navigate`.
    pub fn reset(&mut self) {
        self.partial.clear();
        self.entries.clear();
        self.truncated = false;
    }

    /// Filtered, body-capped copy of the log. With `clear`, the list is
    /// emptied after copying — one `&mut self` call, so atomic to callers.
    pub fn query(
        &mut self,
        filter_url: Option<&str>,
        max_body_bytes: Option<usize>,
        clear: bool,
    ) -> NetworkRequestsResult {
        let cap = max_body_bytes.map_or(MAX_BODY_BYTES, |m| m.min(MAX_BODY_BYTES));
        let entries = self
            .entries
            .iter()
            .filter(|e| filter_url.is_none_or(|f| e.url.contains(f)))
            .cloned()
            .map(|mut e| {
                if let Some(b) = e.body.take() {
                    let (b, cut) = truncate_utf8(&b, cap);
                    e.body = Some(b);
                    e.body_truncated |= cut;
                }
                e
            })
            .collect();
        let result = NetworkRequestsResult {
            entries,
            truncated: self.truncated,
        };
        if clear {
            self.entries.clear();
            self.truncated = false;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(log: &mut NetworkLog, id: &str, url: &str, ct: &str, body: Option<&str>) {
        log.on_request(id, url, "GET", 1_000, 1.0, None);
        log.on_response(id, Some(200), ct);
        log.complete(id, 1.25, body.map(String::from));
    }

    #[test]
    fn is_binary_content_type_should_match_image_font_audio_video_and_octet_stream() {
        for ct in [
            "image/png",
            "font/woff2",
            "audio/mpeg",
            "video/mp4",
            "application/octet-stream",
            "Image/PNG",
        ] {
            assert!(is_binary_content_type(ct), "{ct}");
        }
        for ct in ["application/json", "text/html; charset=utf-8", "text/css"] {
            assert!(!is_binary_content_type(ct), "{ct}");
        }
    }

    #[test]
    fn complete_should_record_status_content_type_body_and_timing_when_response_seen() {
        let mut log = NetworkLog::default();
        done(
            &mut log,
            "1",
            "http://x/api",
            "application/json",
            Some("{\"a\":1}"),
        );
        let r = log.query(None, None, false);
        assert_eq!(r.entries.len(), 1);
        let e = &r.entries[0];
        assert_eq!(e.status, Some(200));
        assert_eq!(e.content_type.as_deref(), Some("application/json"));
        assert_eq!(e.body.as_deref(), Some("{\"a\":1}"));
        assert_eq!(e.timing_ms, Some(250));
        assert_eq!(e.started_at_ms, 1_000);
        assert!(!r.truncated);
    }

    #[test]
    fn wants_body_should_be_false_for_binary_types_and_true_for_json() {
        let mut log = NetworkLog::default();
        log.on_request("1", "http://x/a.png", "GET", 0, 0.0, None);
        log.on_response("1", Some(200), "image/png");
        log.on_request("2", "http://x/api", "GET", 0, 0.0, None);
        log.on_response("2", Some(200), "application/json");
        assert!(!log.wants_body("1"));
        assert!(log.wants_body("2"));
        assert!(!log.wants_body("unknown"));
    }

    #[test]
    fn complete_should_store_null_body_for_binary_type_even_if_bytes_supplied() {
        let mut log = NetworkLog::default();
        done(
            &mut log,
            "1",
            "http://x/a.png",
            "image/png",
            Some("PNGDATA"),
        );
        let e = &log.query(None, None, false).entries[0];
        assert_eq!(e.body, None);
        assert!(!e.body_truncated);
    }

    #[test]
    fn complete_should_truncate_body_at_cap_on_utf8_boundary() {
        let mut log = NetworkLog::default();
        // 3-byte chars: the cap (16384) isn't a multiple of 3, so the cut
        // must back up to a boundary.
        let big = "€".repeat(MAX_BODY_BYTES);
        done(&mut log, "1", "http://x/big", "text/plain", Some(&big));
        let e = &log.query(None, None, false).entries[0];
        let body = e.body.as_ref().unwrap();
        assert!(body.len() <= MAX_BODY_BYTES);
        assert_eq!(body.len() % 3, 0);
        assert!(e.body_truncated);
    }

    #[test]
    fn query_should_apply_smaller_max_body_bytes_but_clamp_larger_to_cap() {
        let mut log = NetworkLog::default();
        done(
            &mut log,
            "1",
            "http://x/a",
            "text/plain",
            Some("abcdefghij"),
        );
        let small = log.query(None, Some(4), false);
        assert_eq!(small.entries[0].body.as_deref(), Some("abcd"));
        assert!(small.entries[0].body_truncated);
        let big = log.query(None, Some(10_000_000), false);
        assert_eq!(big.entries[0].body.as_deref(), Some("abcdefghij"));
        assert!(!big.entries[0].body_truncated);
    }

    #[test]
    fn query_should_narrow_to_urls_containing_filter() {
        let mut log = NetworkLog::default();
        done(&mut log, "1", "http://x/api/data", "application/json", None);
        done(&mut log, "2", "http://x/index.html", "text/html", None);
        let r = log.query(Some("/api/"), None, false);
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].url, "http://x/api/data");
    }

    #[test]
    fn query_with_clear_should_return_entries_then_leave_log_empty() {
        let mut log = NetworkLog::default();
        done(&mut log, "1", "http://x/a", "text/plain", None);
        let first = log.query(None, None, true);
        assert_eq!(first.entries.len(), 1);
        assert!(log.query(None, None, false).entries.is_empty());
    }

    #[test]
    fn query_without_clear_should_leave_log_intact() {
        let mut log = NetworkLog::default();
        done(&mut log, "1", "http://x/a", "text/plain", None);
        log.query(None, None, false);
        assert_eq!(log.query(None, None, false).entries.len(), 1);
    }

    #[test]
    fn reset_should_drop_entries_partials_and_truncation_flag() {
        let mut log = NetworkLog::default();
        done(&mut log, "1", "http://x/a", "text/plain", None);
        log.on_request("2", "http://x/b", "GET", 0, 0.0, None);
        log.reset();
        assert!(!log.wants_body("2"));
        let r = log.query(None, None, false);
        assert!(r.entries.is_empty());
        assert!(!r.truncated);
    }

    #[test]
    fn push_should_stop_at_cap_and_set_truncated() {
        let mut log = NetworkLog::default();
        for i in 0..(MAX_NETWORK_ENTRIES + 5) {
            done(
                &mut log,
                &i.to_string(),
                &format!("http://x/{i}"),
                "text/plain",
                None,
            );
        }
        let r = log.query(None, None, false);
        assert_eq!(r.entries.len(), MAX_NETWORK_ENTRIES);
        assert!(r.truncated);
        assert_eq!(r.entries.last().unwrap().url, "http://x/999");
    }

    #[test]
    fn wants_body_should_be_false_once_cap_reached() {
        let mut log = NetworkLog::default();
        for i in 0..MAX_NETWORK_ENTRIES {
            done(&mut log, &i.to_string(), "http://x/a", "text/plain", None);
        }
        log.on_request("late", "http://x/late", "GET", 0, 0.0, None);
        log.on_response("late", Some(200), "application/json");
        assert!(!log.wants_body("late"));
    }

    #[test]
    fn on_request_should_finalise_previous_hop_when_request_id_repeats_on_redirect() {
        let mut log = NetworkLog::default();
        log.on_request("1", "http://x/old", "GET", 0, 1.0, None);
        log.on_request("1", "http://x/new", "GET", 0, 1.1, Some(302));
        log.on_response("1", Some(200), "text/html");
        log.complete("1", 1.2, None);
        let r = log.query(None, None, false);
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].url, "http://x/old");
        assert_eq!(r.entries[0].status, Some(302));
        assert_eq!(r.entries[1].status, Some(200));
    }

    #[test]
    fn has_response_should_be_true_only_after_on_response() {
        let mut log = NetworkLog::default();
        log.on_request("1", "http://x/a", "GET", 0, 0.0, None);
        assert!(!log.has_response("1"));
        log.on_response("1", None, "text/plain");
        assert!(log.has_response("1"));
    }

    #[test]
    fn on_failed_should_record_entry_without_status() {
        let mut log = NetworkLog::default();
        log.on_request("1", "http://x/down", "GET", 0, 1.0, None);
        log.on_failed("1", 1.5);
        let e = &log.query(None, None, false).entries[0];
        assert_eq!(e.status, None);
        assert_eq!(e.body, None);
    }
}
