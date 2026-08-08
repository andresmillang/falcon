# falcon

A lightweight, agent-first headless browser in Rust. It fetches a page, parses
HTML with html5ever, executes the page's JavaScript in a V8 isolate, and returns
the rendered DOM, visible text, and a per-page error report over a small REST
API — at a fraction of headless Chromium's CPU and memory.

falcon exists to run **Class B** browser work cheaply. It is deliberately **not**
a Chromium replacement for **Class A** work.

## What falcon is for (Class B) — and what it is NOT for (Class A)

**Class A — stays on Chromium, out of scope forever.** Stealth logged-in browser
bridges that must be indistinguishable from real Chrome: persistent authenticated
sessions, anti-bot TLS/JS fingerprinting, human-in-the-loop captcha/2FA solving.
falcon does not attempt these and never spoofs Chrome to defeat bot checks — it
identifies itself honestly as `falcon/<version>`.

**Class B — falcon's target.** Correct DOM + JS + network at minimal cost:

- Page-tour error checking (HTTP ≥400, JS exceptions, console errors, failed
  requests, blank/low-text pages).
- HTML / visible-text extraction from server-rendered and client-rendered pages.
- Reading JSON-API-backed SPAs (fetch/XHR execute and mutate the DOM).
- Simple form-login tours over non-bot-protected sites.

If a task needs stealth, a persistent login, video/WebGL, or pixel screenshots,
use Chromium — not falcon.

## Build & run

```bash
cargo build --release
./target/release/falcon --bind 127.0.0.1:8200
```

Flags: `--js-heap-mb 128`, `--wall-cap-secs 60`, `--default-wait-ms 10000`,
`--max-jobs 4`, `--recycle-after 20`, `--user-agent '<ua>'`.

## API

### `POST /v1/extract`

Fetch one page, run its JavaScript, return the rendered result.

```bash
curl -s -X POST localhost:8200/v1/extract -H 'content-type: application/json' -d '{
  "url": "https://example.com/",
  "wait_ms": 10000,
  "block": ["image", "font", "media"],
  "headers": {"accept-language": "en-US"},
  "insecure_tls": false
}'
```

Response:

```json
{
  "status": 200,
  "final_url": "https://example.com/",
  "html": "<html>…post-JS DOM…</html>",
  "text": "visible text …",
  "title": "Example",
  "console_errors": [],
  "page_errors": [],
  "failed_requests": [],
  "responses": [{"url": "…", "status": 200, "method": "GET"}],
  "timing_ms": 42
}
```

- `text` is the concatenated visible text (script/style excluded).
- `console_errors` = `console.error(...)`; `page_errors` = uncaught JS
  exceptions/rejections and forced terminations; `failed_requests` = network
  errors and HTTP ≥ 400.
- `block` suppresses fetching subresources of the given kinds.
- `wait_ms` bounds the JS wait window; the default (10 s) is also the hard cap.

### `POST /v1/tour`

Optionally log in, then visit a list of pages and report each.

```bash
curl -s -X POST localhost:8200/v1/tour -H 'content-type: application/json' -d '{
  "base": "https://app.example.com",
  "pages": ["/", "/dashboard", "/settings"],
  "min_text": 50,
  "login": {
    "path": "/login",
    "user": "alice", "pass": "secret",
    "user_sel": "[name=username]", "pass_sel": "[name=password]",
    "submit_sel": "#submit"
  }
}'
```

Cookies from the login are carried through the whole tour. A page is `ok:false`
when its status ≥ 400, any page error occurred, or its text is shorter than
`min_text`.

### `GET /healthz` · `GET /metrics`

`/healthz` returns `ok`. `/metrics` is Prometheus text: `falcon_rss_bytes`,
`falcon_jobs_served_total`, `falcon_jobs_failed_total`, `falcon_jobs_inflight`,
`falcon_pool_recycles_total`.

## Architecture

- **HTML parsing** — html5ever parses documents and `innerHTML` fragments into a
  normalized tree (spec-correct implied tags and error recovery).
- **DOM + JS** — the live, mutable DOM and the CSS-selector/event engine run as a
  hand-written shim **inside** the V8 isolate (`src/dom_shim.js`). Rust supplies
  host primitives: html parsing, `fetch`/`XMLHttpRequest`, timers, `console`.
- **Event loop** — Rust drives it: drain microtasks → service one queued request
  or one due timer → repeat until network-idle or a cap. This makes network-idle
  detection and the resource firewall enforceable.
- **Resource firewall** — one V8 isolate per page job (stronger than
  recycle-after-N); a per-job wall-clock cap and an RSS-delta watchdog terminate
  runaway pages while other jobs continue; a job panic never kills the process.
  Worker threads are also recycled after `--recycle-after` jobs.

See `PARITY.md` for the list of intentionally unsupported web APIs.

## Tests

- `cargo test` — pure-Rust unit tests (HTML parsing, resource collection, login
  selector matching) plus a V8 heap-callback wiring test.
- `scripts/run_acceptance.sh` — the end-to-end integration + acceptance suite
  (A1–A7): starts a bundled fixture server and falcon, checks JS/DOM/network
  behavior, runs a **parity** comparison against real Chromium
  (`scripts/parity.py`), and a **resource** comparison (`scripts/resources.py`).

## Scope

falcon builds only Class B. It does not implement screenshots, layout/paint,
WebGL/video, service workers, or any anti-bot/stealth behavior. (v2 adds ES
modules and an optional, off-by-default Chromium fallback — see below.)

---

# Falcon v2 additions

Falcon v2 keeps the Class-B/standalone core and adds the browser functionality
machines most need, escalating the rest to Chromium.

## ES modules (R1-R8)
`<script type="module">`, static `import` (relative + absolute), nested/multiple
dependency graphs, per-job module caching, and dynamic `import()`. Module fetch
failures are reported in `page_errors` and block only their dependents.

## Browser APIs (R9-R20)
MutationObserver, URL/URLSearchParams, history pushState/replaceState,
localStorage/sessionStorage, requestAnimationFrame, AbortController/AbortSignal,
richer Event/CustomEvent (detail, once, stopImmediatePropagation), form
elements + submit, correct DOMContentLoaded/load + readyState lifecycle, a
broad DOM traversal/manipulation surface, and a stronger CSS selector engine
(`^= $= *= ~=`, `:not()`, `+`/`~` combinators, selector lists).

## Resource containment (R21-R25)
Per-job, configurable via CLI flag or a request `limits` object:
`--max-exec-ms` (continuous-JS budget — CPU-bound pages die in seconds, not the
wall cap), `--max-response-bytes`, `--max-requests`, `--max-redirects`,
`--max-timers`, `--max-nodes`, plus `--wall-cap-secs` and `--js-heap-mb`.
Every termination is classified (`exec_timeout`, `resource_limit`,
`memory_limit`, `wall_timeout`, `cancelled`). Jobs can be cancelled via
`POST /v1/cancel {id}` (supply your own `job_id` on the extract request).

## Falcon→Chromium fallback (R33-R38) — optional, off by default
Enable with `--enable-fallback --chromium-cdp <chrome-headless-shell|ws-url>`.
Chromium is contacted only on a classified condition (`unsupported_es_module_feature`,
`unsupported_browser_api`, `navigation_timeout`, `resource_limit`,
`javascript_failure`, `render_incomplete`) and never on success. Every
`/v1/extract` response carries `engine_used`, `falcon_status`, `fallback_used`,
`fallback_reason`, and preserves Falcon's original errors under
`falcon_diagnostics`. With fallback disabled Falcon is fully standalone.

## Observability (R39-R40)
Each response includes a `metrics` object (job_duration_ms, rss_delta_bytes,
request_count, downloaded_bytes, js_exec_ms, dom_node_count, timer_count,
error_count, limit_reason, engine_used, fallback_reason) and one structured JSON
log line per job. `/metrics` adds `falcon_fallback_used_total` and
`falcon_limit_terminations_total`.

## Test corpus & suites
- `scripts/gen_corpus.py` writes `tests/corpus/` (64 deterministic fixtures across
  18 categories) with a manifest; `scripts/parity_corpus.py` runs each through
  Falcon and Chromium and classifies PASS / KNOWN DIFFERENCE / FALCON BUG.
- `scripts/abuse.py` — the six resource-abuse cases (R25).
- `scripts/fallback_test.sh` — the five fallback scenarios (R38).
- `scripts/realworld.py` — public-site compatibility (informational).
- `scripts/benchmark.py` — RSS / startup / latency / throughput (informational).
