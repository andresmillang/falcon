//! falcon — lightweight agent-first headless browser. REST server (F1/F2/F6).

mod cdp;
mod chromium;
mod dom;
mod engine;
mod login;
mod net;
mod pool;
mod server;

use clap::Parser;
use std::sync::Arc;

#[derive(Clone)]
pub struct Config {
    pub ua: String,
    pub js_heap_mb: usize,
    pub wall_cap_secs: u64,
    pub default_wait_ms: u64,
    pub max_jobs: usize,
    pub recycle_after: u64,
    pub limits: engine::Limits,
    pub fallback: server::FallbackConfig,
}

#[derive(Parser)]
#[command(name = "falcon", version, about = "Lightweight agent-first headless browser (Class B tasks only)")]
struct Cli {
    /// Bind address, e.g. 127.0.0.1:8200
    #[arg(long, default_value = "127.0.0.1:8200")]
    bind: String,
    /// V8 heap cap per page (MB)
    #[arg(long, default_value_t = 128)]
    js_heap_mb: usize,
    /// Per-job wall-clock cap (seconds)
    #[arg(long, default_value_t = 60)]
    wall_cap_secs: u64,
    /// Default/max JS wait window (ms)
    #[arg(long, default_value_t = 10000)]
    default_wait_ms: u64,
    /// Concurrent worker threads
    #[arg(long, default_value_t = 4)]
    max_jobs: usize,
    /// Recycle a worker thread after this many jobs
    #[arg(long, default_value_t = 20)]
    recycle_after: u64,
    /// User-Agent string (identifies honestly as falcon)
    #[arg(long, default_value = "falcon/0.1 (+https://localhost; agent-first headless)")]
    user_agent: String,
    /// Continuous JS-execution budget per job (ms) — bounds CPU-bound pages (R23)
    #[arg(long, default_value_t = 8000)]
    max_exec_ms: u64,
    /// Max single-response download size (bytes)
    #[arg(long, default_value_t = 33_554_432)]
    max_response_bytes: u64,
    /// Max network requests per job
    #[arg(long, default_value_t = 500)]
    max_requests: u32,
    /// Max redirect depth
    #[arg(long, default_value_t = 10)]
    max_redirects: usize,
    /// Max timer fires per job
    #[arg(long, default_value_t = 20000)]
    max_timers: u32,
    /// Max DOM node count per job
    #[arg(long, default_value_t = 1_000_000)]
    max_nodes: u64,
    /// Enable Falcon→Chromium fallback (off by default; standalone otherwise) (R33)
    #[arg(long, default_value_t = false)]
    enable_fallback: bool,
    /// Chromium backend for fallback: a CDP ws URL, or a chrome-headless-shell binary path to launch
    #[arg(long, default_value = "")]
    chromium_cdp: String,
    /// Fallback (Chromium) timeout (seconds)
    #[arg(long, default_value_t = 30)]
    fallback_timeout_secs: u64,
}

fn main() {
    let cli = Cli::parse();
    engine::init_v8();

    let cfg = Config {
        ua: cli.user_agent.clone(),
        js_heap_mb: cli.js_heap_mb,
        wall_cap_secs: cli.wall_cap_secs,
        default_wait_ms: cli.default_wait_ms,
        max_jobs: cli.max_jobs.max(1),
        recycle_after: cli.recycle_after.max(1),
        limits: engine::Limits {
            max_exec_ms: cli.max_exec_ms,
            max_response_bytes: cli.max_response_bytes,
            max_requests: cli.max_requests,
            max_redirects: cli.max_redirects,
            max_timers: cli.max_timers,
            max_nodes: cli.max_nodes,
        },
        fallback: server::FallbackConfig {
            enabled: cli.enable_fallback,
            chromium_cdp: cli.chromium_cdp.clone(),
            timeout_secs: cli.fallback_timeout_secs,
        },
    };

    let (tx, rx) = crossbeam_channel::unbounded::<pool::Job>();
    let metrics = Arc::new(pool::Metrics::new());
    pool::spawn_pool(cfg.clone(), rx, metrics.clone());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        server::serve(&cli.bind, tx, metrics, cfg).await;
    });
}
