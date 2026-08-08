//! HTTP surface (F1/F2/F6/R36/R40): axum handlers dispatching jobs to the pool.

use crate::engine::{self, Limits};
use crate::pool::{self, ExtractReq, Job, JobKind, LoginReq, Metrics, TourReq};
use crate::Config;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use crossbeam_channel::Sender;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Optional Falcon→Chromium fallback backend (R33). Off by default.
#[derive(Clone)]
pub struct FallbackConfig {
    pub enabled: bool,
    pub chromium_cdp: String,
    pub timeout_secs: u64,
}

#[derive(Clone)]
struct AppState {
    tx: Sender<Job>,
    metrics: Arc<Metrics>,
    cfg: Config,
}

pub async fn serve(bind: &str, tx: Sender<Job>, metrics: Arc<Metrics>, cfg: Config) {
    let state = AppState { tx, metrics, cfg };
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .route("/v1/extract", post(extract))
        .route("/v1/tour", post(tour))
        .route("/v1/cancel", post(cancel))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .unwrap_or_else(|e| panic!("bind {bind}: {e}"));
    eprintln!("falcon listening on {bind}");
    axum::serve(listener, app).await.expect("server");
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn metrics_handler(State(st): State<AppState>) -> impl IntoResponse {
    let m = &st.metrics;
    let rss = read_rss_bytes();
    let body = format!(
        concat!(
            "# HELP falcon_rss_bytes Resident set size in bytes\n# TYPE falcon_rss_bytes gauge\nfalcon_rss_bytes {}\n",
            "# HELP falcon_jobs_served_total Jobs served\n# TYPE falcon_jobs_served_total counter\nfalcon_jobs_served_total {}\n",
            "# HELP falcon_jobs_failed_total Jobs that panicked/failed internally\n# TYPE falcon_jobs_failed_total counter\nfalcon_jobs_failed_total {}\n",
            "# HELP falcon_jobs_inflight Jobs currently executing\n# TYPE falcon_jobs_inflight gauge\nfalcon_jobs_inflight {}\n",
            "# HELP falcon_pool_recycles_total Worker recycle events\n# TYPE falcon_pool_recycles_total counter\nfalcon_pool_recycles_total {}\n",
            "# HELP falcon_fallback_used_total Jobs escalated to Chromium\n# TYPE falcon_fallback_used_total counter\nfalcon_fallback_used_total {}\n",
            "# HELP falcon_limit_terminations_total Jobs terminated by a resource limit\n# TYPE falcon_limit_terminations_total counter\nfalcon_limit_terminations_total {}\n",
        ),
        rss,
        m.served.load(Ordering::Relaxed),
        m.failed.load(Ordering::Relaxed),
        m.in_flight.load(Ordering::Relaxed),
        m.recycles.load(Ordering::Relaxed),
        m.fallback_used.load(Ordering::Relaxed),
        m.limit_terminations.load(Ordering::Relaxed),
    );
    ([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}

/// Per-request limit overrides (R21). Any omitted field falls back to config.
#[derive(Deserialize, Default)]
struct LimitBody {
    max_exec_ms: Option<u64>,
    max_response_bytes: Option<u64>,
    max_requests: Option<u32>,
    max_redirects: Option<usize>,
    max_timers: Option<u32>,
    max_nodes: Option<u64>,
}

#[derive(Deserialize)]
struct ExtractBody {
    url: String,
    #[serde(default)]
    wait_ms: Option<u64>,
    #[serde(default)]
    block: Vec<String>,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    insecure_tls: bool,
    #[serde(default)]
    limits: Option<LimitBody>,
    #[serde(default)]
    wall_cap_secs: Option<u64>,
    #[serde(default)]
    job_id: Option<String>,
}

fn merge_limits(base: &Limits, over: &Option<LimitBody>) -> Limits {
    let o = over.as_ref();
    Limits {
        max_exec_ms: o.and_then(|x| x.max_exec_ms).unwrap_or(base.max_exec_ms),
        max_response_bytes: o.and_then(|x| x.max_response_bytes).unwrap_or(base.max_response_bytes),
        max_requests: o.and_then(|x| x.max_requests).unwrap_or(base.max_requests),
        max_redirects: o.and_then(|x| x.max_redirects).unwrap_or(base.max_redirects),
        max_timers: o.and_then(|x| x.max_timers).unwrap_or(base.max_timers),
        max_nodes: o.and_then(|x| x.max_nodes).unwrap_or(base.max_nodes),
    }
}

async fn extract(State(st): State<AppState>, Json(b): Json<ExtractBody>) -> impl IntoResponse {
    let limits = merge_limits(&st.cfg.limits, &b.limits);
    let wall = b.wall_cap_secs.unwrap_or(st.cfg.wall_cap_secs);
    let req = ExtractReq {
        url: b.url,
        wait_ms: b.wait_ms,
        block: b.block,
        headers: b.headers.into_iter().collect(),
        insecure_tls: b.insecure_tls,
        limits,
        wall_cap_secs: wall,
        job_id: b.job_id,
    };
    dispatch(&st, JobKind::Extract(req)).await
}

#[derive(Deserialize)]
struct CancelBody {
    id: String,
}

async fn cancel(Json(b): Json<CancelBody>) -> impl IntoResponse {
    let found = engine::cancel_job(&b.id);
    (StatusCode::OK, Json(json!({ "cancelled": found, "id": b.id })))
}

#[derive(Deserialize)]
struct LoginBody {
    path: String,
    user: String,
    pass: String,
    user_sel: String,
    pass_sel: String,
    #[serde(default)]
    submit_sel: String,
}

#[derive(Deserialize)]
struct TourBody {
    base: String,
    pages: Vec<String>,
    #[serde(default = "default_min_text")]
    min_text: usize,
    #[serde(default)]
    block: Vec<String>,
    #[serde(default)]
    insecure_tls: bool,
    #[serde(default)]
    login: Option<LoginBody>,
}
fn default_min_text() -> usize {
    50
}

async fn tour(State(st): State<AppState>, Json(b): Json<TourBody>) -> impl IntoResponse {
    let req = TourReq {
        base: b.base,
        pages: b.pages,
        min_text: b.min_text,
        block: b.block,
        insecure_tls: b.insecure_tls,
        login: b.login.map(|l| LoginReq {
            path: l.path,
            user: l.user,
            pass: l.pass,
            user_sel: l.user_sel,
            pass_sel: l.pass_sel,
            submit_sel: l.submit_sel,
        }),
    };
    dispatch(&st, JobKind::Tour(req)).await
}

async fn dispatch(st: &AppState, kind: JobKind) -> (StatusCode, Json<Value>) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel::<Value>();
    let job = Job { kind, reply: reply_tx };
    if st.tx.send(job).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "worker pool unavailable"})));
    }
    match reply_rx.await {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "worker dropped job"}))),
    }
}

/// Falcon→Chromium fallback (R33-R38). Runs on the worker thread with the
/// Falcon result already in hand. Only escalates on a classified condition,
/// only when enabled, never on success; always preserves Falcon diagnostics and
/// emits the structured per-job log line (R39).
pub fn maybe_fallback(cfg: &Config, url: &str, r: engine::PageResult) -> Value {
    let base = pool::page_json(&r);
    let reason = crate::chromium::escalation_reason(&r);
    let value = if let Some(reason) = reason {
        if cfg.fallback.enabled {
            crate::chromium::escalate(cfg, url, base, &r, reason)
        } else {
            // Enabled == false: return Falcon's result honestly (R34/R38d).
            let mut v = base;
            v["falcon_status"] = json!(r.falcon_status());
            v["fallback_reason"] = json!(reason);
            v["fallback_used"] = json!(false);
            v
        }
    } else {
        base
    };
    log_job(url, &value);
    value
}

/// One structured JSON log line per job (R39).
fn log_job(url: &str, v: &Value) {
    let m = v.get("metrics").cloned().unwrap_or(json!({}));
    let line = json!({
        "event": "job",
        "url": url,
        "engine_used": v.get("engine_used"),
        "falcon_status": v.get("falcon_status"),
        "fallback_used": v.get("fallback_used"),
        "fallback_reason": v.get("fallback_reason"),
        "metrics": m,
    });
    println!("{line}");
}

fn read_rss_bytes() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
        let mut it = s.split_whitespace();
        let _total = it.next();
        if let Some(pages) = it.next().and_then(|r| r.parse::<u64>().ok()) {
            return pages * 4096;
        }
    }
    0
}
