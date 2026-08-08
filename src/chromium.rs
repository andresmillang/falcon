//! Falcon→Chromium fallback backend (R33-R38). The classifier (escalation_reason)
//! decides whether a Falcon result warrants escalation; escalate() runs the
//! optional live Chromium backend over CDP. Off by default; standalone otherwise.

use crate::engine::PageResult;
use crate::Config;
use serde_json::{json, Value};

/// Decide whether this Falcon result is an explicit, classified escalation
/// condition (R35). Returns None when Falcon succeeded (never escalate success).
pub fn escalation_reason(r: &PageResult) -> Option<&'static str> {
    // Explicit unsupported markers pushed by the engine/shim.
    if r.page_errors.iter().any(|e| e.starts_with("UNSUPPORTED:module")) {
        return Some("unsupported_es_module_feature");
    }
    if r.page_errors.iter().any(|e| e.starts_with("UNSUPPORTED:api")) {
        return Some("unsupported_browser_api");
    }
    // Resource-limit / timeout terminations.
    if let Some(reason) = &r.limit_reason {
        return match reason.as_str() {
            "exec_timeout" | "wall_timeout" => Some("navigation_timeout"),
            "memory_limit" | "resource_limit" => Some("resource_limit"),
            "cancelled" => None, // an operator cancel is not an escalation
            _ => Some("resource_limit"),
        };
    }
    // A hard document fetch failure is a navigation failure Chromium may handle.
    if r.status == 0 {
        return Some("navigation_timeout");
    }
    // Uncaught JS errors.
    if !r.page_errors.is_empty() {
        return Some("javascript_failure");
    }
    // Rendered but empty — likely an unsupported render path.
    if r.status < 400 && r.text.trim().is_empty() && r.status != 204 {
        return Some("render_incomplete");
    }
    None
}

/// Escalate to the configured Chromium backend and return its result, preserving
/// Falcon's original diagnostics (R36). On backend failure, returns Falcon's
/// result honestly with fallback_used=false and a clear reason (R38c/R38e).
pub fn escalate(cfg: &Config, url: &str, base: Value, r: &PageResult, reason: &str) -> Value {
    let falcon_diag = falcon_diagnostics(r);
    match crate::cdp::render_via_chromium(&cfg.fallback.chromium_cdp, url, cfg.fallback.timeout_secs) {
        Ok(chrome) => {
            let mut v = base;
            v["engine_used"] = json!("chromium");
            v["falcon_status"] = json!(r.falcon_status());
            v["fallback_used"] = json!(true);
            v["fallback_reason"] = json!(reason);
            v["falcon_diagnostics"] = falcon_diag;
            // Overwrite the content fields with Chromium's result.
            v["html"] = json!(chrome.html);
            v["text"] = json!(chrome.text);
            v["title"] = json!(chrome.title);
            v["status"] = json!(chrome.status);
            v["final_url"] = json!(chrome.final_url);
            v["console_errors"] = json!(chrome.console_errors);
            v["page_errors"] = json!(chrome.page_errors);
            if let Some(m) = v.get_mut("metrics") {
                m["engine_used"] = json!("chromium");
                m["fallback_reason"] = json!(reason);
            }
            v
        }
        Err(e) => {
            // Backend unavailable/failed: honest Falcon result, not hidden.
            let mut v = base;
            v["fallback_used"] = json!(false);
            v["fallback_reason"] = json!(reason);
            v["falcon_status"] = json!(r.falcon_status());
            v["fallback_error"] = json!(format!("chromium backend unavailable: {e}"));
            v
        }
    }
}

fn falcon_diagnostics(r: &PageResult) -> Value {
    json!({
        "status": r.status,
        "text_len": r.text.chars().count(),
        "console_errors": r.console_errors,
        "page_errors": r.page_errors,
        "failed_requests": r.failed_requests,
        "limit_reason": r.limit_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::PageResult;

    fn pr() -> PageResult {
        PageResult {
            status: 200, final_url: String::new(), html: String::new(), text: "hi".into(),
            title: String::new(), console_errors: vec![], page_errors: vec![], failed_requests: vec![],
            responses: vec![], timing_ms: 0, limit_reason: None, request_count: 0, downloaded_bytes: 0,
            js_exec_ms: 0, dom_node_count: 0, timer_count: 0, rss_delta_bytes: 0,
        }
    }

    #[test]
    fn success_never_escalates() {
        assert_eq!(escalation_reason(&pr()), None);
    }

    #[test]
    fn classifies_conditions() {
        let mut r = pr(); r.limit_reason = Some("exec_timeout".into());
        assert_eq!(escalation_reason(&r), Some("navigation_timeout"));
        let mut r = pr(); r.limit_reason = Some("resource_limit".into());
        assert_eq!(escalation_reason(&r), Some("resource_limit"));
        let mut r = pr(); r.limit_reason = Some("cancelled".into());
        assert_eq!(escalation_reason(&r), None); // a cancel is not an escalation
        let mut r = pr(); r.page_errors = vec!["boom".into()];
        assert_eq!(escalation_reason(&r), Some("javascript_failure"));
        let mut r = pr(); r.status = 0;
        assert_eq!(escalation_reason(&r), Some("navigation_timeout"));
        let mut r = pr(); r.text = "".into();
        assert_eq!(escalation_reason(&r), Some("render_incomplete"));
        let mut r = pr(); r.page_errors = vec!["UNSUPPORTED:module:bare-specifier x".into()];
        assert_eq!(escalation_reason(&r), Some("unsupported_es_module_feature"));
    }
}
