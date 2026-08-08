//! Worker pool (F7). Each worker thread owns V8 isolates (created per page job —
//! strictly stronger than recycle-after-N for the leak firewall) and is itself
//! recycled after `recycle_after` jobs to shed any thread-local accumulation.

use crate::engine::{self, RunOptions};
use crate::login::{self, LoginSpec};
use crate::net;
use crate::Config;
use crossbeam_channel::Receiver;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Metrics {
    pub served: AtomicU64,
    pub failed: AtomicU64,
    pub in_flight: AtomicI64,
    pub recycles: AtomicU64,
    pub fallback_used: AtomicU64,
    pub limit_terminations: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Metrics {
            served: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            in_flight: AtomicI64::new(0),
            recycles: AtomicU64::new(0),
            fallback_used: AtomicU64::new(0),
            limit_terminations: AtomicU64::new(0),
        }
    }
}

pub enum JobKind {
    Extract(ExtractReq),
    Tour(TourReq),
}

pub struct Job {
    pub kind: JobKind,
    pub reply: tokio::sync::oneshot::Sender<Value>,
}

#[derive(Clone)]
pub struct ExtractReq {
    pub url: String,
    pub wait_ms: Option<u64>,
    pub block: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub insecure_tls: bool,
    /// Effective per-job limits (request overrides already merged over config).
    pub limits: engine::Limits,
    /// Per-job wall-clock cap seconds (request override or config default).
    pub wall_cap_secs: u64,
    /// Optional client-supplied job id (enables cancellation before completion).
    pub job_id: Option<String>,
}

static JOB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn next_job_id() -> String {
    format!("job-{}", JOB_SEQ.fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone)]
pub struct TourReq {
    pub base: String,
    pub pages: Vec<String>,
    pub min_text: usize,
    pub block: Vec<String>,
    pub insecure_tls: bool,
    pub login: Option<LoginReq>,
}

#[derive(Clone)]
pub struct LoginReq {
    pub path: String,
    pub user: String,
    pub pass: String,
    pub user_sel: String,
    pub pass_sel: String,
    pub submit_sel: String,
}

pub fn spawn_pool(cfg: Config, rx: Receiver<Job>, metrics: Arc<Metrics>) {
    for _ in 0..cfg.max_jobs {
        let rx = rx.clone();
        let metrics = metrics.clone();
        let cfg = cfg.clone();
        std::thread::spawn(move || worker_supervisor(cfg, rx, metrics));
    }
}

/// Keeps a worker thread alive; the actual loop exits every `recycle_after`
/// jobs and we respawn it (recycle) to defend against V8 thread-local buildup.
fn worker_supervisor(cfg: Config, rx: Receiver<Job>, metrics: Arc<Metrics>) {
    loop {
        let processed = worker_loop(&cfg, &rx, &metrics);
        if processed == 0 {
            // channel closed
            return;
        }
        metrics.recycles.fetch_add(1, Ordering::Relaxed);
    }
}

fn worker_loop(cfg: &Config, rx: &Receiver<Job>, metrics: &Arc<Metrics>) -> u64 {
    let mut count = 0u64;
    while count < cfg.recycle_after {
        let job = match rx.recv() {
            Ok(j) => j,
            Err(_) => return count.max(if count == 0 { 0 } else { count }),
        };
        count += 1;
        metrics.in_flight.fetch_add(1, Ordering::Relaxed);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &job.kind {
            JobKind::Extract(req) => do_extract(cfg, req),
            JobKind::Tour(req) => do_tour(cfg, req),
        }));
        metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
        let value = match result {
            Ok(v) => {
                metrics.served.fetch_add(1, Ordering::Relaxed);
                if v.get("fallback_used").and_then(|x| x.as_bool()) == Some(true) {
                    metrics.fallback_used.fetch_add(1, Ordering::Relaxed);
                }
                if v.get("metrics").and_then(|m| m.get("limit_reason")).map(|r| !r.is_null()) == Some(true) {
                    metrics.limit_terminations.fetch_add(1, Ordering::Relaxed);
                }
                v
            }
            Err(_) => {
                metrics.served.fetch_add(1, Ordering::Relaxed);
                metrics.failed.fetch_add(1, Ordering::Relaxed);
                json!({ "error": "internal panic during job", "status": 0 })
            }
        };
        let _ = job.reply.send(value);
    }
    count
}

#[allow(clippy::too_many_arguments)]
fn run_opts(
    cfg: &Config,
    url: &str,
    wait_ms: u64,
    block: Vec<String>,
    headers: Vec<(String, String)>,
    insecure: bool,
    limits: engine::Limits,
    wall_cap_secs: u64,
    job_id: String,
) -> RunOptions {
    RunOptions {
        url: url.to_string(),
        wait_ms,
        block,
        extra_headers: headers,
        insecure_tls: insecure,
        ua: cfg.ua.clone(),
        js_heap_mb: cfg.js_heap_mb,
        wall_cap: Duration::from_secs(wall_cap_secs),
        limits,
        job_id,
    }
}

fn do_extract(cfg: &Config, req: &ExtractReq) -> Value {
    // Effective wait window: requested value, capped at the default hard cap.
    let wait = req.wait_ms.unwrap_or(cfg.default_wait_ms).min(cfg.default_wait_ms);
    let job_id = req.job_id.clone().unwrap_or_else(next_job_id);
    let opts = run_opts(
        cfg, &req.url, wait, req.block.clone(), req.headers.clone(), req.insecure_tls,
        req.limits.clone(), req.wall_cap_secs, job_id,
    );
    let r = engine::render(&opts, None);
    // Falcon→Chromium fallback (R33-R38): only on a classified condition, only
    // when enabled, never on success. Preserves Falcon diagnostics.
    crate::server::maybe_fallback(cfg, &req.url, r)
}

fn do_tour(cfg: &Config, req: &TourReq) -> Value {
    // One cookie-sharing client for the whole tour.
    let client = net::build_client(&cfg.ua, req.insecure_tls, cfg.limits.max_redirects);

    if let Some(l) = &req.login {
        let spec = LoginSpec {
            url: l.path.clone(),
            user: l.user.clone(),
            pass: l.pass.clone(),
            user_sel: l.user_sel.clone(),
            pass_sel: l.pass_sel.clone(),
            submit_sel: l.submit_sel.clone(),
        };
        if let Err(e) = login::perform(&client, &req.base, &spec) {
            return json!({ "error": format!("login failed: {e}"), "pages": [], "summary": { "visited": 0, "failed": 0 } });
        }
    }

    let base = req.base.trim_end_matches('/').to_string();
    let mut pages = Vec::new();
    let mut failed = 0usize;
    for p in &req.pages {
        let url = if p.starts_with("http") { p.clone() } else { format!("{}{}", base, if p.starts_with('/') { p.clone() } else { format!("/{p}") }) };
        let opts = run_opts(
            cfg, &url, cfg.default_wait_ms, req.block.clone(), vec![], req.insecure_tls,
            cfg.limits.clone(), cfg.wall_cap_secs, next_job_id(),
        );
        let r = engine::render(&opts, Some(client.clone()));
        let text_len = r.text.chars().count();
        let ok = r.status < 400 && r.page_errors.is_empty() && text_len >= req.min_text;
        if !ok {
            failed += 1;
        }
        pages.push(json!({
            "page": p,
            "ok": ok,
            "status": r.status,
            "text_len": text_len,
            "console_errors": r.console_errors,
            "page_errors": r.page_errors,
            "failed_requests": r.failed_requests,
            "timing_ms": r.timing_ms,
        }));
    }
    json!({
        "pages": pages,
        "summary": { "visited": req.pages.len(), "failed": failed },
    })
}

pub fn page_json(r: &engine::PageResult) -> Value {
    let falcon_status = r.falcon_status();
    json!({
        "status": r.status,
        "final_url": r.final_url,
        "html": r.html,
        "text": r.text,
        "title": r.title,
        "console_errors": r.console_errors,
        "page_errors": r.page_errors,
        "failed_requests": r.failed_requests,
        "responses": r.responses.iter().map(|(u, s, m)| json!({"url": u, "status": s, "method": m})).collect::<Vec<_>>(),
        "timing_ms": r.timing_ms,
        // Engine/fallback (default falcon standalone; server may overwrite on escalation)
        "engine_used": "falcon",
        "falcon_status": falcon_status,
        "fallback_used": false,
        "fallback_reason": Value::Null,
        // Observability (R39)
        "metrics": {
            "job_duration_ms": r.timing_ms,
            "rss_delta_bytes": r.rss_delta_bytes,
            "request_count": r.request_count,
            "downloaded_bytes": r.downloaded_bytes,
            "js_exec_ms": r.js_exec_ms,
            "dom_node_count": r.dom_node_count,
            "timer_count": r.timer_count,
            "error_count": r.console_errors.len() + r.page_errors.len(),
            "limit_reason": r.limit_reason,
            "engine_used": "falcon",
            "fallback_reason": Value::Null,
        },
    })
}
