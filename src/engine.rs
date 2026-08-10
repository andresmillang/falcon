//! V8 engine (F4) + the Rust-driven event loop that enforces network-idle,
//! timers, and the resource firewall (F7). The DOM lives in the JS shim; Rust
//! supplies host primitives and drives async ordering.

use crate::dom;
use crate::net::NetClient;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

static V8_INIT: Once = Once::new();

/// Load ICU common data before V8 starts. Without it, any Intl-backed call a
/// page makes (`toLocaleTimeString`, `Intl.DateTimeFormat`, …) is a V8 FATAL
/// that aborts the whole process (kernel `trap int3`) — a job must never be
/// able to do that. Data path: $FALCON_ICU_DATA, else the vendored file.
/// Missing/mismatched data degrades gracefully: we log and continue, and only
/// Intl-dependent pages fail (as they did before), instead of crashing.
fn init_icu() {
    let path = std::env::var("FALCON_ICU_DATA").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/third_party/icu/icudt74l.dat").to_string()
    });
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("falcon: ICU data not loaded ({path}: {e}); Intl-dependent pages will error");
            return;
        }
    };
    // ICU requires the data to be 16-aligned; Vec<u8> gives no such guarantee,
    // so copy into an explicitly aligned, leaked buffer (one-time, process-wide).
    let layout = std::alloc::Layout::from_size_align(raw.len(), 16).expect("icu layout");
    let data: &'static [u8] = unsafe {
        let ptr = std::alloc::alloc(layout);
        assert!(!ptr.is_null(), "icu alloc");
        std::ptr::copy_nonoverlapping(raw.as_ptr(), ptr, raw.len());
        std::slice::from_raw_parts(ptr, raw.len())
    };
    match v8::icu::set_common_data_74(data) {
        Ok(()) => eprintln!("falcon: ICU data loaded from {path}"),
        Err(code) => eprintln!("falcon: ICU data rejected (code {code}, {path}); Intl-dependent pages will error"),
    }
}

pub fn init_v8() {
    V8_INIT.call_once(|| {
        init_icu();
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

thread_local! {
    static HEAP_HIT: Cell<bool> = const { Cell::new(false) };
    static LIMIT_HIT: Cell<bool> = const { Cell::new(false) };
    static TERM_HANDLE: RefCell<Option<v8::IsolateHandle>> = const { RefCell::new(None) };
}

extern "C" fn near_heap_limit_cb(_data: *mut std::ffi::c_void, current: usize, _initial: usize) -> usize {
    // Secondary catch: fires for transient allocation blowups. Retaining loops
    // are caught by the RSS-delta watchdog instead (V8 does not reliably invoke
    // this callback mid-script for monotonically retained memory).
    HEAP_HIT.with(|h| h.set(true));
    TERM_HANDLE.with(|t| {
        if let Some(handle) = t.borrow().as_ref() {
            handle.terminate_execution();
        }
    });
    current + 16 * 1024 * 1024
}

/// Resident set size of this process in kB (Linux /proc/self/statm).
fn read_rss_kb() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
        let mut it = s.split_whitespace();
        let _total = it.next();
        if let Some(pages) = it.next().and_then(|r| r.parse::<u64>().ok()) {
            return pages * 4; // 4 KB pages
        }
    }
    0
}

/// Why a job was force-terminated.
const REASON_NONE: u8 = 0;
const REASON_WALL: u8 = 1;
const REASON_MEM: u8 = 2;
const REASON_EXEC: u8 = 3;
const REASON_LIMIT: u8 = 4;
const REASON_CANCEL: u8 = 5;

/// A network request queued by JS fetch()/XHR.
struct PendingReq {
    id: i64,
    url: String,
    method: String,
    headers_json: String,
    body: String,
    is_xhr: bool,
}

/// A scheduled timer.
struct Timer {
    id: i64,
    due_ms: u64,
    cleared: bool,
}

/// Host state shared with V8 callbacks via an isolate slot.
struct Host {
    net: NetClient,
    console_errors: Vec<String>,
    page_errors: Vec<String>,
    pending: VecDeque<PendingReq>,
    timers: Vec<Timer>,
    virtual_ms: u64,
    timer_fires: u32,
    aborted: std::collections::HashSet<i64>,
    request_count: u32,
    downloaded_bytes: u64,
}

type HostRc = Rc<RefCell<Host>>;

pub struct PageResult {
    pub status: u16,
    pub final_url: String,
    pub html: String,
    pub text: String,
    pub title: String,
    pub console_errors: Vec<String>,
    pub page_errors: Vec<String>,
    pub failed_requests: Vec<String>,
    pub responses: Vec<(String, u16, String)>,
    pub timing_ms: u128,
    // Observability (R39) + containment classification.
    pub limit_reason: Option<String>,
    pub request_count: u32,
    pub downloaded_bytes: u64,
    pub js_exec_ms: u64,
    pub dom_node_count: u64,
    pub timer_count: u32,
    pub rss_delta_bytes: u64,
}

impl PageResult {
    /// Classify Falcon's own outcome (R36).
    pub fn falcon_status(&self) -> &'static str {
        match &self.limit_reason {
            Some(x) if x == "cancelled" => "cancelled",
            Some(x) if x.contains("timeout") => "timeout",
            Some(_) => "resource_limit",
            None if self.status == 0 => "navigation_failed",
            None if !self.page_errors.is_empty() => "javascript_failure",
            None => "ok",
        }
    }
}

/// Per-job resource limits (R21). Configurable via CLI/request.
#[derive(Clone)]
pub struct Limits {
    pub max_exec_ms: u64,       // continuous JS execution budget (R23)
    pub max_response_bytes: u64, // per-response download cap (R21)
    pub max_requests: u32,       // network requests per job (R21)
    pub max_redirects: usize,    // redirect depth (R21/R26)
    pub max_timers: u32,         // timer fires per job (R21)
    pub max_nodes: u64,          // DOM node count (R21)
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_exec_ms: 8000,
            max_response_bytes: 32 * 1024 * 1024,
            max_requests: 500,
            max_redirects: 10,
            max_timers: 20_000,
            max_nodes: 1_000_000,
        }
    }
}

pub struct RunOptions {
    pub url: String,
    pub wait_ms: u64,
    pub block: Vec<String>,
    pub extra_headers: Vec<(String, String)>,
    pub insecure_tls: bool,
    pub ua: String,
    pub js_heap_mb: usize,
    pub wall_cap: Duration,
    pub limits: Limits,
    pub job_id: String,
}

/// JS-execution clock: distinguishes continuous JS execution (subject to the
/// exec budget) from Rust-side blocking (network), so pathological CPU-bound
/// pages terminate within the budget without penalizing slow network pages.
pub struct JsClock {
    pub in_js: AtomicBool,
    pub start_ms: std::sync::atomic::AtomicU64,
    pub total_ms: std::sync::atomic::AtomicU64,
}

thread_local! {
    static JS_GUARD: RefCell<Option<(Arc<JsClock>, Instant)>> = const { RefCell::new(None) };
}

fn enter_js() {
    JS_GUARD.with(|g| {
        if let Some((c, start)) = &*g.borrow() {
            c.start_ms
                .store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
            c.in_js.store(true, Ordering::Relaxed);
        }
    });
}
fn exit_js() {
    JS_GUARD.with(|g| {
        if let Some((c, start)) = &*g.borrow()
            && c.in_js.swap(false, Ordering::Relaxed) {
                let now = start.elapsed().as_millis() as u64;
                let began = c.start_ms.load(Ordering::Relaxed);
                if now > began {
                    c.total_ms.fetch_add(now - began, Ordering::Relaxed);
                }
            }
    });
}

/// RAII: marks JS execution finished even if the call unwinds/early-returns.
struct ExitJsGuard;
impl Drop for ExitJsGuard {
    fn drop(&mut self) {
        exit_js();
    }
}

/// Global cancellation registry (R24): job_id -> (isolate handle, cancelled).
type CancelEntry = (v8::IsolateHandle, Arc<AtomicBool>);
static CANCEL_REGISTRY: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, CancelEntry>>,
> = std::sync::OnceLock::new();

fn cancel_registry() -> &'static std::sync::Mutex<std::collections::HashMap<String, CancelEntry>> {
    CANCEL_REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Request cancellation of an in-flight job by id. Returns true if found.
pub fn cancel_job(job_id: &str) -> bool {
    if let Ok(map) = cancel_registry().lock()
        && let Some((handle, flag)) = map.get(job_id) {
            flag.store(true, Ordering::Relaxed);
            handle.terminate_execution();
            return true;
        }
    false
}

const SHIM: &str = include_str!("dom_shim.js");

/// Fetch + render one page, running its JavaScript. `client` (optional) shares a
/// cookie jar across a tour.
pub fn render(
    opts: &RunOptions,
    client: Option<reqwest::blocking::Client>,
) -> PageResult {
    let start = Instant::now();
    let job_start_rss = read_rss_kb();
    HEAP_HIT.with(|h| h.set(false));
    LIMIT_HIT.with(|l| l.set(false));

    // Initial document fetch (Rust side) — carries cookies for the tour.
    let mut net = match client {
        Some(c) => NetClient::with_client(c, &opts.url, &opts.ua, opts.block.clone()),
        None => NetClient::with_client(
            crate::net::build_client(&opts.ua, opts.insecure_tls, opts.limits.max_redirects),
            &opts.url,
            &opts.ua,
            opts.block.clone(),
        ),
    };
    net.set_max_bytes(opts.limits.max_response_bytes);
    let doc = net.fetch(&opts.url, "GET", &opts.extra_headers, None);
    let final_url = doc.final_url.clone();
    let status = doc.status;
    let html = doc.body.clone();
    let mut downloaded: u64 = doc.bytes;

    // Reset base to final_url so relative resources resolve against redirects,
    // and use it as the Referer for subsequent requests (R30).
    net.set_base(&final_url);
    net.set_referrer(&final_url);

    // If the document itself exceeded the size cap (R21/E11), reject fast — do
    // not parse or render the (truncated) body.
    let doc_too_large = doc.error.as_deref() == Some("response too large");
    if doc_too_large {
        LIMIT_HIT.with(|l| l.set(true));
    }

    // Parse scripts + subresources (html5ever), unless the doc was too large.
    let (scripts, subs) = if doc_too_large {
        (Vec::new(), Vec::new())
    } else {
        dom::collect_resources(&html)
    };

    // Load block-filtered subresources (img/link) and record outcomes.
    for s in &subs {
        let kind = if s.kind == "style" { "font" } else { &s.kind };
        if net.is_blocked_kind(kind) {
            continue;
        }
        let out = net.fetch(&s.url, "GET", &[], None);
        downloaded += out.bytes;
    }

    let host: HostRc = Rc::new(RefCell::new(Host {
        net,
        console_errors: Vec::new(),
        page_errors: Vec::new(),
        pending: VecDeque::new(),
        timers: Vec::new(),
        virtual_ms: 0,
        timer_fires: 0,
        aborted: std::collections::HashSet::new(),
        request_count: 0,
        downloaded_bytes: 0,
    }));

    // ---- isolate ----
    let max_heap = opts.js_heap_mb * 1024 * 1024;
    let init_heap = (max_heap / 4).max(1024 * 1024);
    let params = v8::CreateParams::default().heap_limits(init_heap, max_heap);
    let mut isolate = v8::Isolate::new(params);
    isolate.add_near_heap_limit_callback(near_heap_limit_cb, std::ptr::null_mut());
    isolate.set_host_import_module_dynamically_callback(dynamic_import_callback);
    let iso_handle = isolate.thread_safe_handle();
    TERM_HANDLE.with(|t| *t.borrow_mut() = Some(iso_handle.clone()));

    // Register for external cancellation (R24) and set up the JS-execution clock
    // (R23) so pathological CPU-bound pages die within the exec budget.
    let cancelled = Arc::new(AtomicBool::new(false));
    if !opts.job_id.is_empty()
        && let Ok(mut m) = cancel_registry().lock() {
            m.insert(opts.job_id.clone(), (iso_handle.clone(), cancelled.clone()));
        }
    let js_clock = Arc::new(JsClock {
        in_js: AtomicBool::new(false),
        start_ms: std::sync::atomic::AtomicU64::new(0),
        total_ms: std::sync::atomic::AtomicU64::new(0),
    });
    JS_GUARD.with(|g| *g.borrow_mut() = Some((js_clock.clone(), start)));

    // Watchdog: enforces the wall-clock cap, an RSS-delta memory ceiling, AND a
    // continuous-JS-execution budget. Cross-thread terminate_execution reliably
    // interrupts JS, including retaining loops the near-heap callback misses.
    let done = Arc::new(AtomicBool::new(false));
    let reason = Arc::new(std::sync::atomic::AtomicU8::new(REASON_NONE));
    let watchdog = {
        let handle = iso_handle.clone();
        let done = done.clone();
        let reason = reason.clone();
        let clock = js_clock.clone();
        let cap = opts.wall_cap;
        let exec_budget = opts.limits.max_exec_ms;
        let start_instant = start;
        let start_rss = read_rss_kb();
        let mem_ceiling_kb = ((opts.js_heap_mb as u64) * 6).min(1536) * 1024;
        std::thread::spawn(move || {
            let step = Duration::from_millis(25);
            let mut waited = Duration::ZERO;
            while waited < cap {
                if done.load(Ordering::Relaxed) {
                    return;
                }
                // JS-execution budget: JS has been running continuously too long.
                if clock.in_js.load(Ordering::Relaxed) {
                    let now = start_instant.elapsed().as_millis() as u64;
                    let js_start = clock.start_ms.load(Ordering::Relaxed);
                    if now > js_start && now - js_start > exec_budget {
                        reason.store(REASON_EXEC, Ordering::Relaxed);
                        handle.terminate_execution();
                        return;
                    }
                }
                let rss = read_rss_kb();
                if rss > start_rss && rss - start_rss > mem_ceiling_kb {
                    reason.store(REASON_MEM, Ordering::Relaxed);
                    handle.terminate_execution();
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            if !done.load(Ordering::Relaxed) {
                reason.store(REASON_WALL, Ordering::Relaxed);
                handle.terminate_execution();
            }
        })
    };

    isolate.set_slot(host.clone());

    let collected = if doc_too_large {
        String::new()
    } else {
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        install_host_functions(scope, context);

        // Install the shim with UA/location substituted.
        let loc = location_object(&final_url);
        let shim = SHIM
            .replace("__UA__", &json_str(&opts.ua))
            .replace("__LOCATION__", &loc);
        run_script(scope, &shim, "shim.js", &host);

        // Build the document from html5ever's parse tree.
        let tree = dom::parse_document_json(&html);
        let tree_str = tree.to_string();
        let build_arg = str_val(scope, &tree_str);
        call_global(scope, context, "__build_document", &[build_arg], &host);

        // DOM node-count limit (R21/E11): reject a huge HTML page.
        if js_node_count(scope, context, &host) > opts.limits.max_nodes {
            LIMIT_HIT.with(|l| l.set(true));
            scope.terminate_execution();
        }

        // Execute classic scripts in document order (modules are deferred).
        let mut module_count = 0usize;
        for s in &scripts {
            if s.is_module {
                continue;
            }
            let code = if let Some(src) = &s.src {
                let out = host.borrow_mut().net.fetch(src, "GET", &[], None);
                if !out.ok {
                    continue;
                }
                out.body
            } else {
                s.code.clone()
            };
            if code.trim().is_empty() {
                continue;
            }
            run_script(scope, &code, s.src.as_deref().unwrap_or("inline"), &host);
            if terminated() {
                break;
            }
        }

        // Then evaluate module scripts, deferred, in document order (R1/R18).
        MODULE_GRAPH.with(|m| *m.borrow_mut() = ModuleGraph::default());
        if !terminated() && !scope.is_execution_terminating() {
            for s in &scripts {
                if !s.is_module || terminated() || scope.is_execution_terminating() {
                    continue;
                }
                let mut errs = Vec::new();
                if let Some(src) = &s.src {
                    let out = host.borrow_mut().net.fetch(src, "GET", &[], None);
                    if !out.ok {
                        errs.push(format!("module fetch failed: {} ({})", src, out.status));
                    } else {
                        let mod_url = out.final_url.clone();
                        eval_module(scope, &mod_url, &out.body, &host, &mut errs);
                    }
                } else {
                    // Inline module: synthetic URL so relative imports resolve
                    // against the document.
                    module_count += 1;
                    let mod_url = format!("{final_url}#inline-module-{module_count}");
                    eval_module(scope, &mod_url, &s.code, &host, &mut errs);
                }
                host.borrow_mut().page_errors.extend(errs);
            }
        }
        // Release module globals for this job.
        MODULE_GRAPH.with(|m| *m.borrow_mut() = ModuleGraph::default());

        // Fire lifecycle events, then pump the event loop.
        if !terminated() {
            call_global(scope, context, "__fire_lifecycle", &[], &host);
        }
        if !terminated() {
            pump(scope, context, &host, opts, start);
        }

        // Read back the rendered result (best-effort; may be empty if terminated).
        if !terminated() {
            match call_global(scope, context, "__collect", &[], &host) {
                Some(v) => v.to_rust_string_lossy(scope),
                None => String::new(),
            }
        } else {
            String::new()
        }
    };

    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();
    TERM_HANDLE.with(|t| *t.borrow_mut() = None);
    JS_GUARD.with(|g| *g.borrow_mut() = None);
    if !opts.job_id.is_empty()
        && let Ok(mut m) = cancel_registry().lock() {
            m.remove(&opts.job_id);
        }

    // Classify the termination reason (R22): a single classified message per
    // job, most-specific first.
    let limit_reason: Option<String> = if cancelled.load(Ordering::Relaxed) {
        Some("cancelled".into())
    } else if LIMIT_HIT.with(|l| l.get()) {
        Some("resource_limit".into())
    } else {
        match reason.load(Ordering::Relaxed) {
            REASON_MEM => Some("memory_limit".into()),
            REASON_EXEC => Some("exec_timeout".into()),
            REASON_WALL => Some("wall_timeout".into()),
            REASON_LIMIT => Some("resource_limit".into()),
            REASON_CANCEL => Some("cancelled".into()),
            _ if HEAP_HIT.with(|h| h.get()) => Some("memory_limit".into()),
            _ => None,
        }
    };

    let mut h = host.borrow_mut();
    if let Some(r) = &limit_reason {
        h.page_errors.push(format!("job terminated: {r}"));
    }

    let node_count = parse_collected_nodes(&collected);
    let (rhtml, rtext, rtitle) = parse_collected(&collected);
    let responses = h
        .net
        .responses
        .iter()
        .map(|r| (r.url.clone(), r.status, r.method.clone()))
        .collect();
    let end_rss = read_rss_kb();
    let rss_delta = end_rss.saturating_sub(job_start_rss) * 1024;

    PageResult {
        status,
        final_url,
        html: if rhtml.is_empty() { html } else { rhtml },
        text: rtext,
        title: rtitle,
        console_errors: std::mem::take(&mut h.console_errors),
        page_errors: std::mem::take(&mut h.page_errors),
        failed_requests: h.net.failed.clone(),
        responses,
        timing_ms: start.elapsed().as_millis(),
        limit_reason,
        request_count: h.request_count,
        downloaded_bytes: h.downloaded_bytes.max(downloaded),
        js_exec_ms: js_clock.total_ms.load(Ordering::Relaxed),
        dom_node_count: node_count,
        timer_count: h.timer_fires,
        rss_delta_bytes: rss_delta,
    }
}

fn terminated() -> bool {
    HEAP_HIT.with(|h| h.get())
}

// ================= ES modules (R1-R8) =================

thread_local! {
    static MODULE_GRAPH: RefCell<ModuleGraph> = RefCell::new(ModuleGraph::default());
}

#[derive(Default)]
struct ModuleGraph {
    id_to_url: std::collections::HashMap<i32, String>,
    url_to_module: std::collections::HashMap<String, v8::Global<v8::Module>>,
}

/// Resolve a module specifier against the importing module's URL. Bare
/// specifiers (no ./ ../ / or scheme) are unsupported (no import maps) — Err(()).
fn resolve_module_url(base: &str, spec: &str) -> Result<String, ()> {
    let relative = spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with('/')
        || spec.starts_with("http://")
        || spec.starts_with("https://");
    if !relative {
        return Err(());
    }
    url::Url::parse(base)
        .ok()
        .and_then(|b| b.join(spec).ok())
        .map(|u| u.to_string())
        .ok_or(())
}

fn module_origin<'s>(
    scope: &mut v8::HandleScope<'s>,
    name: v8::Local<'s, v8::String>,
) -> v8::ScriptOrigin<'s> {
    v8::ScriptOrigin::new(
        scope,
        name.into(),
        0,
        0,
        false,
        0,
        None,
        false,
        false,
        true, // is_module
        None,
    )
}

/// Recursively compile `url`'s module and all its static dependencies, filling
/// MODULE_GRAPH. Records fetch/compile failures in `errors` (R6/R7).
fn load_module_graph(
    scope: &mut v8::HandleScope,
    url: &str,
    code: &str,
    host: &HostRc,
    errors: &mut Vec<String>,
) {
    if MODULE_GRAPH.with(|m| m.borrow().url_to_module.contains_key(url)) {
        return;
    }
    let module = {
        let src_str = match v8::String::new(scope, code) {
            Some(s) => s,
            None => return,
        };
        let name = v8::String::new(scope, url).unwrap();
        let origin = module_origin(scope, name);
        let mut source = v8::script_compiler::Source::new(src_str, Some(&origin));
        let tc = &mut v8::TryCatch::new(scope);
        match v8::script_compiler::compile_module(tc, &mut source) {
            Some(m) => m,
            None => {
                if let Some(ex) = tc.exception() {
                    errors.push(format!("module compile error {url}: {}", ex.to_rust_string_lossy(tc)));
                }
                return;
            }
        }
    };
    let hash = module.get_identity_hash().get();
    let global = v8::Global::new(scope, module);
    MODULE_GRAPH.with(|m| {
        let mut g = m.borrow_mut();
        g.id_to_url.insert(hash, url.to_string());
        g.url_to_module.insert(url.to_string(), global);
    });

    // Resolve + fetch static dependencies.
    let requests = module.get_module_requests();
    let mut deps: Vec<String> = Vec::new();
    for i in 0..requests.length() {
        if let Some(data) = requests.get(scope, i)
            && let Ok(req) = v8::Local::<v8::ModuleRequest>::try_from(data) {
                let spec = req.get_specifier().to_rust_string_lossy(scope);
                match resolve_module_url(url, &spec) {
                    Ok(dep_url) => deps.push(dep_url),
                    Err(()) => errors.push(format!("UNSUPPORTED:module:bare-specifier {spec}")),
                }
            }
    }
    for dep_url in deps {
        if MODULE_GRAPH.with(|m| m.borrow().url_to_module.contains_key(&dep_url)) {
            continue;
        }
        let out = host.borrow_mut().net.fetch(&dep_url, "GET", &[], None);
        if !out.ok {
            errors.push(format!("module fetch failed: {} ({})", dep_url, out.status));
            continue;
        }
        load_module_graph(scope, &dep_url, &out.body, host, errors);
    }
}

/// Resolve callback used during instantiation (looks up MODULE_GRAPH).
fn resolve_module_callback<'a>(
    context: v8::Local<'a, v8::Context>,
    specifier: v8::Local<'a, v8::String>,
    _import_attributes: v8::Local<'a, v8::FixedArray>,
    referrer: v8::Local<'a, v8::Module>,
) -> Option<v8::Local<'a, v8::Module>> {
    // SAFETY (V8 binding layer): V8 invokes this resolve callback synchronously
    // on the isolate thread with a valid, entered context; CallbackScope::new is
    // the required binding API to obtain a HandleScope inside such a callback.
    let scope = &mut unsafe { v8::CallbackScope::new(context) };
    let spec = specifier.to_rust_string_lossy(scope);
    let ref_hash = referrer.get_identity_hash().get();
    let base = MODULE_GRAPH.with(|m| m.borrow().id_to_url.get(&ref_hash).cloned())?;
    let dep_url = resolve_module_url(&base, &spec).ok()?;
    let global = MODULE_GRAPH.with(|m| m.borrow().url_to_module.get(&dep_url).cloned())?;
    Some(v8::Local::new(scope, global))
}

/// Compile, instantiate, and evaluate one module (entry point) plus its graph.
fn eval_module(
    scope: &mut v8::HandleScope,
    url: &str,
    code: &str,
    host: &HostRc,
    errors: &mut Vec<String>,
) {
    load_module_graph(scope, url, code, host, errors);
    let global = match MODULE_GRAPH.with(|m| m.borrow().url_to_module.get(url).cloned()) {
        Some(g) => g,
        None => return,
    };
    let module = v8::Local::new(scope, global);
    // Instantiate.
    {
        let tc = &mut v8::TryCatch::new(scope);
        let ok = module.instantiate_module(tc, resolve_module_callback);
        if ok != Some(true) {
            if let Some(ex) = tc.exception() {
                errors.push(format!("module instantiation error {url}: {}", ex.to_rust_string_lossy(tc)));
            } else {
                errors.push(format!("module instantiation failed: {url}"));
            }
            return;
        }
    }
    // Evaluate.
    {
        let tc = &mut v8::TryCatch::new(scope);
        let result = module.evaluate(tc);
        if result.is_none() || module.get_status() == v8::ModuleStatus::Errored {
            let exc = module.get_exception();
            errors.push(format!("module evaluation error {url}: {}", exc.to_rust_string_lossy(tc)));
        }
    }
}

/// Dynamic import() host callback (R5).
fn dynamic_import_callback<'s>(
    scope: &mut v8::HandleScope<'s>,
    _host_defined: v8::Local<'s, v8::Data>,
    resource_name: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    _attrs: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let base = resource_name.to_rust_string_lossy(scope);
    let spec = specifier.to_rust_string_lossy(scope);
    let host = scope.get_slot::<HostRc>().cloned();

    let reject = |scope: &mut v8::HandleScope, resolver: v8::Local<v8::PromiseResolver>, msg: &str| {
        let m = v8::String::new(scope, msg).unwrap();
        let err = v8::Exception::error(scope, m);
        resolver.reject(scope, err);
    };

    let dep_url = match resolve_module_url(&base, &spec) {
        Ok(u) => u,
        Err(()) => {
            reject(scope, resolver, &format!("Cannot resolve module specifier: {spec}"));
            return Some(promise);
        }
    };
    let host = match host {
        Some(h) => h,
        None => {
            reject(scope, resolver, "no host");
            return Some(promise);
        }
    };
    // Fetch + load if not already present.
    if !MODULE_GRAPH.with(|m| m.borrow().url_to_module.contains_key(&dep_url)) {
        let out = host.borrow_mut().net.fetch(&dep_url, "GET", &[], None);
        if !out.ok {
            reject(scope, resolver, &format!("Failed to fetch module: {dep_url}"));
            return Some(promise);
        }
        let mut errs = Vec::new();
        load_module_graph(scope, &dep_url, &out.body, &host, &mut errs);
    }
    let global = match MODULE_GRAPH.with(|m| m.borrow().url_to_module.get(&dep_url).cloned()) {
        Some(g) => g,
        None => {
            reject(scope, resolver, &format!("Module not found: {dep_url}"));
            return Some(promise);
        }
    };
    let module = v8::Local::new(scope, global);
    let inst = module.instantiate_module(scope, resolve_module_callback);
    if inst != Some(true) {
        reject(scope, resolver, &format!("Failed to instantiate module: {dep_url}"));
        return Some(promise);
    }
    let _ = module.evaluate(scope);
    if module.get_status() == v8::ModuleStatus::Errored {
        reject(scope, resolver, &format!("Module evaluation failed: {dep_url}"));
        return Some(promise);
    }
    let ns = module.get_module_namespace();
    resolver.resolve(scope, ns);
    Some(promise)
}

/// The Rust-driven event loop: drain microtasks, service one queued network
/// request or one due timer per iteration, until idle or a cap is hit.
fn pump(
    scope: &mut v8::HandleScope,
    context: v8::Local<v8::Context>,
    host: &HostRc,
    opts: &RunOptions,
    start: Instant,
) {
    let max_fires: u32 = opts.limits.max_timers;
    let max_requests: u32 = opts.limits.max_requests;
    loop {
        scope.perform_microtask_checkpoint();
        if start.elapsed() >= opts.wall_cap || terminated() || scope.is_execution_terminating() {
            return;
        }

        // 1) service one pending network request
        let next_req = host.borrow_mut().pending.pop_front();
        if let Some(req) = next_req {
            // Skip requests aborted by AbortController (R15/R29).
            if host.borrow().aborted.contains(&req.id) {
                continue;
            }
            // Max requests per job (R21/E9): terminate the endless-fetch chain.
            if host.borrow().request_count >= max_requests {
                LIMIT_HIT.with(|l| l.set(true));
                scope.terminate_execution();
                return;
            }
            let headers = parse_headers(&req.headers_json);
            let body = if req.body.is_empty() { None } else { Some(req.body.as_str()) };
            let out = host
                .borrow_mut()
                .net
                .fetch(&req.url, &req.method, &headers, body);
            {
                let mut h = host.borrow_mut();
                h.request_count += 1;
                h.downloaded_bytes += out.bytes;
            }
            let hdr_json = headers_to_json(&out.headers);
            let args = [
                num_val(scope, req.id as f64),
                bool_val(scope, out.ok),
                num_val(scope, out.status as f64),
                str_val(scope, &hdr_json),
                str_val(scope, &out.body),
                str_val(scope, out.error.as_deref().unwrap_or("")),
                str_val(scope, &out.final_url),
            ];
            let _ = req.is_xhr;
            call_global(scope, context, "__resolve_request", &args, host);
            continue;
        }

        // 2) fire one due timer (advance virtual clock if needed)
        let fire_id = {
            let mut h = host.borrow_mut();
            h.timers.retain(|t| !t.cleared);
            if h.timer_fires >= max_fires {
                // Max timer fires (R21/E8): bound recursive timer creation.
                LIMIT_HIT.with(|l| l.set(true));
                scope.terminate_execution();
                return;
            }
            if h.timers.is_empty() {
                None
            } else {
                // earliest due timer
                let mut idx = 0usize;
                for i in 1..h.timers.len() {
                    if h.timers[i].due_ms < h.timers[idx].due_ms {
                        idx = i;
                    }
                }
                let due = h.timers[idx].due_ms;
                // Respect the wait_ms virtual budget: don't fire timers scheduled
                // beyond the wait window.
                if due > opts.wait_ms {
                    None
                } else {
                    if due > h.virtual_ms {
                        h.virtual_ms = due;
                    }
                    h.timer_fires += 1;
                    let id = h.timers[idx].id;
                    h.timers.remove(idx);
                    Some(id)
                }
            }
        };
        if let Some(id) = fire_id {
            let args = [num_val(scope, id as f64)];
            call_global(scope, context, "__fire_timer", &args, host);
            continue;
        }

        // idle: no pending requests, no due timers within budget
        return;
    }
}

// ---------- host function installation ----------

macro_rules! set_fn {
    ($scope:expr, $global:expr, $name:expr, $cb:expr) => {{
        let key = v8::String::new($scope, $name).unwrap();
        let f = v8::Function::new($scope, $cb).unwrap();
        $global.set($scope, key.into(), f.into());
    }};
}

fn install_host_functions(
    scope: &mut v8::HandleScope,
    context: v8::Local<v8::Context>,
) {
    let global = context.global(scope);
    set_fn!(scope, global, "__console", cb_console);
    set_fn!(scope, global, "__page_error", cb_page_error);
    set_fn!(scope, global, "__parse_html_fragment", cb_parse_fragment);
    set_fn!(scope, global, "__enqueue_request", cb_enqueue_request);
    set_fn!(scope, global, "__set_timer", cb_set_timer);
    set_fn!(scope, global, "__clear_timer", cb_clear_timer);
    set_fn!(scope, global, "__parse_url", cb_parse_url);
    set_fn!(scope, global, "__abort_request", cb_abort_request);
}

fn cb_parse_url(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let url = args.get(0).to_rust_string_lossy(scope);
    let base = args.get(1).to_rust_string_lossy(scope);
    let parsed = if base.is_empty() {
        url::Url::parse(&url)
    } else {
        match url::Url::parse(&base) {
            Ok(b) => b.join(&url),
            Err(e) => Err(e),
        }
    };
    let json = match parsed {
        Ok(u) => {
            let origin = u.origin().ascii_serialization();
            serde_json::json!({
                "href": u.as_str(),
                "protocol": format!("{}:", u.scheme()),
                "host": match u.port() { Some(p) => format!("{}:{}", u.host_str().unwrap_or(""), p), None => u.host_str().unwrap_or("").to_string() },
                "hostname": u.host_str().unwrap_or(""),
                "port": u.port().map(|p| p.to_string()).unwrap_or_default(),
                "pathname": u.path(),
                "search": u.query().map(|q| format!("?{q}")).unwrap_or_default(),
                "hash": u.fragment().map(|f| format!("#{f}")).unwrap_or_default(),
                "origin": origin,
                "username": u.username(),
                "password": u.password().unwrap_or(""),
            })
            .to_string()
        }
        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
    };
    let s = v8::String::new(scope, &json).unwrap();
    rv.set(s.into());
}

fn cb_abort_request(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = args.get(0).number_value(scope).unwrap_or(0.0) as i64;
    let host = host_from(scope);
    let mut h = host.borrow_mut();
    h.aborted.insert(id);
    h.pending.retain(|r| r.id != id);
}

fn host_from(scope: &mut v8::HandleScope) -> HostRc {
    scope.get_slot::<HostRc>().unwrap().clone()
}

fn cb_console(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let level = args.get(0).to_rust_string_lossy(scope);
    let msg = args.get(1).to_rust_string_lossy(scope);
    let host = host_from(scope);
    if level == "error" {
        host.borrow_mut().console_errors.push(msg);
    }
}

fn cb_page_error(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let msg = args.get(0).to_rust_string_lossy(scope);
    host_from(scope).borrow_mut().page_errors.push(msg);
}

fn cb_parse_fragment(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let html = args.get(0).to_rust_string_lossy(scope);
    let json = dom::parse_fragment_json(&html).to_string();
    let s = v8::String::new(scope, &json).unwrap();
    rv.set(s.into());
}

fn cb_enqueue_request(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = args.get(0).number_value(scope).unwrap_or(0.0) as i64;
    let url = args.get(1).to_rust_string_lossy(scope);
    let method = args.get(2).to_rust_string_lossy(scope);
    let headers_json = args.get(3).to_rust_string_lossy(scope);
    let body = args.get(4).to_rust_string_lossy(scope);
    let is_xhr = args.get(5).boolean_value(scope);
    host_from(scope).borrow_mut().pending.push_back(PendingReq {
        id,
        url,
        method,
        headers_json,
        body,
        is_xhr,
    });
}

fn cb_set_timer(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = args.get(0).number_value(scope).unwrap_or(0.0) as i64;
    let delay = args.get(1).number_value(scope).unwrap_or(0.0).max(0.0) as u64;
    let host = host_from(scope);
    let mut h = host.borrow_mut();
    let due = h.virtual_ms + delay;
    // replace existing timer with same id (interval re-arm) else push
    if let Some(t) = h.timers.iter_mut().find(|t| t.id == id) {
        t.due_ms = due;
        t.cleared = false;
    } else {
        h.timers.push(Timer { id, due_ms: due, cleared: false });
    }
}

fn cb_clear_timer(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = args.get(0).number_value(scope).unwrap_or(0.0) as i64;
    let host = host_from(scope);
    for t in host.borrow_mut().timers.iter_mut() {
        if t.id == id {
            t.cleared = true;
        }
    }
}

// ---------- v8 helpers ----------

fn run_script(scope: &mut v8::HandleScope, code: &str, name: &str, host: &HostRc) {
    enter_js();
    let _guard = ExitJsGuard;
    let scope = &mut v8::TryCatch::new(scope);
    let source = match v8::String::new(scope, code) {
        Some(s) => s,
        None => return,
    };
    let origin_name = v8::String::new(scope, name).unwrap();
    let origin = v8::ScriptOrigin::new(
        scope, origin_name.into(), 0, 0, false, 0, None, false, false, false, None,
    );
    let script = match v8::Script::compile(scope, source, Some(&origin)) {
        Some(s) => s,
        None => {
            report_trycatch(scope, host);
            return;
        }
    };
    if script.run(scope).is_none() {
        report_trycatch(scope, host);
    }
}

fn report_trycatch(scope: &mut v8::TryCatch<v8::HandleScope>, host: &HostRc) {
    if scope.has_terminated() {
        return;
    }
    if let Some(ex) = scope.exception() {
        let msg = ex.to_rust_string_lossy(scope);
        host.borrow_mut().page_errors.push(msg);
    }
}

fn call_global<'s>(
    scope: &mut v8::HandleScope<'s>,
    context: v8::Local<v8::Context>,
    name: &str,
    args: &[v8::Local<v8::Value>],
    host: &HostRc,
) -> Option<v8::Local<'s, v8::Value>> {
    enter_js();
    let _guard = ExitJsGuard;
    let global = context.global(scope);
    let key = v8::String::new(scope, name)?;
    let val = global.get(scope, key.into())?;
    let func = v8::Local::<v8::Function>::try_from(val).ok()?;
    let scope = &mut v8::TryCatch::new(scope);
    let recv = global.into();
    let result = func.call(scope, recv, args);
    if result.is_none() {
        report_trycatch(scope, host);
    }
    // SAFETY of lifetimes handled by v8; escape via the outer handle scope.
    result
}

fn str_val<'s>(scope: &mut v8::HandleScope<'s>, s: &str) -> v8::Local<'s, v8::Value> {
    v8::String::new(scope, s).unwrap().into()
}
fn num_val<'s>(scope: &mut v8::HandleScope<'s>, n: f64) -> v8::Local<'s, v8::Value> {
    v8::Number::new(scope, n).into()
}
fn bool_val<'s>(scope: &mut v8::HandleScope<'s>, b: bool) -> v8::Local<'s, v8::Value> {
    v8::Boolean::new(scope, b).into()
}

// ---------- misc helpers ----------

fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn location_object(url: &str) -> String {
    let parsed = url::Url::parse(url).ok();
    let (origin, pathname, search) = match &parsed {
        Some(u) => (
            u.origin().ascii_serialization(),
            u.path().to_string(),
            u.query().map(|q| format!("?{q}")).unwrap_or_default(),
        ),
        None => (String::new(), "/".into(), String::new()),
    };
    format!(
        "{{ href: {}, origin: {}, protocol: {}, host: {}, hostname: {}, pathname: {}, search: {}, hash: \"\", assign: function(){{}}, replace: function(){{}}, reload: function(){{}}, toString: function(){{ return {}; }} }}",
        json_str(url),
        json_str(&origin),
        json_str(parsed.as_ref().map(|u| format!("{}:", u.scheme())).unwrap_or_default().as_str()),
        json_str(parsed.as_ref().and_then(|u| u.host_str()).unwrap_or("")),
        json_str(parsed.as_ref().and_then(|u| u.host_str()).unwrap_or("")),
        json_str(&pathname),
        json_str(&search),
        json_str(url),
    )
}

fn parse_headers(json: &str) -> Vec<(String, String)> {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Object(m)) => m
            .into_iter()
            .map(|(k, v)| (k, v.as_str().unwrap_or("").to_string()))
            .collect(),
        _ => vec![],
    }
}

fn headers_to_json(headers: &[(String, String)]) -> String {
    let mut m = serde_json::Map::new();
    for (k, v) in headers {
        m.insert(k.to_lowercase(), serde_json::Value::String(v.clone()));
    }
    serde_json::Value::Object(m).to_string()
}

fn parse_collected(s: &str) -> (String, String, String) {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => (
            v.get("html").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ),
        Err(_) => (String::new(), String::new(), String::new()),
    }
}

fn parse_collected_nodes(s: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .and_then(|v| v.get("node_count").and_then(|x| x.as_u64()))
        .unwrap_or(0)
}

/// Query the shim's live node counter (R21 huge-HTML guard).
fn js_node_count(
    scope: &mut v8::HandleScope,
    context: v8::Local<v8::Context>,
    host: &HostRc,
) -> u64 {
    match call_global(scope, context, "__node_count", &[], host) {
        Some(v) => v.number_value(scope).unwrap_or(0.0) as u64,
        None => 0,
    }
}

#[cfg(test)]
mod v2_tests {
    use super::*;

    #[test]
    fn module_url_resolution() {
        assert_eq!(resolve_module_url("http://x/a/b.js", "./c.js").unwrap(), "http://x/a/c.js");
        assert_eq!(resolve_module_url("http://x/a/b.js", "../d.js").unwrap(), "http://x/d.js");
        assert_eq!(resolve_module_url("http://x/a/b.js", "/e.js").unwrap(), "http://x/e.js");
        assert_eq!(resolve_module_url("http://x/a/b.js", "https://y/f.js").unwrap(), "https://y/f.js");
        // bare specifiers are unsupported (no import maps)
        assert!(resolve_module_url("http://x/a/b.js", "lodash").is_err());
    }

    #[test]
    fn limits_default_sane() {
        let l = Limits::default();
        assert!(l.max_exec_ms > 0 && l.max_requests > 0 && l.max_nodes > 0);
    }
}
