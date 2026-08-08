# falcon — lightweight agent-first headless browser (Rust) — MVP spec

## Context and scope

We run headless Chromium for two classes of work. **Class A** — stealth logged-in
browser bridges (anti-bot fingerprint required) — is OUT OF SCOPE FOREVER and
stays on Chromium; falcon must never claim to replace it. **Class B** — page-tour
error checking, HTML/text extraction, public-page scraping, JSON-API-backed SPA
reads — is falcon's target: correct DOM + JS + network at a fraction of
Chromium's CPU/memory cost.

falcon is a single Rust binary exposing a REST API. No pixel rendering, no CDP,
no stealth. It identifies itself honestly (`falcon/<version>` in its default
User-Agent and `Server` header) and never impersonates Chrome to defeat bot
checks.

## Functional requirements

### F1 — `POST /v1/extract`
Request JSON: `{url, wait_ms?, block?: ["image","font","media"], headers?, insecure_tls?}`.
Behavior: fetch the page, parse HTML into a DOM, execute JavaScript (F4),
wait for network-idle (no in-flight requests for 500 ms) or `wait_ms`
(whichever bound is hit first; `wait_ms` default 10000 is a hard cap), then respond:
`{status, final_url, html, text, title, console_errors[], page_errors[],
failed_requests[], responses: [{url, status, method}], timing_ms}`.
- `html` = serialized post-JS DOM; `text` = concatenated visible text content
  (script/style excluded); `title` = document.title after JS.
- `console_errors` = console.error calls; `page_errors` = uncaught JS
  exceptions/rejections; `failed_requests` = network errors + HTTP ≥ 400,
  each as a human-readable string containing the URL.
- `block` suppresses fetching of subresources by type (by extension +
  Content-Type sniff on `<img>`, `<link rel=stylesheet>` for font/css files,
  media elements). Blocked requests are not counted as failed.
- `insecure_tls: true` accepts invalid/self-signed certificates.
- Redirects followed (≤10); `final_url` reflects the landing URL.

### F2 — `POST /v1/tour`
Request JSON: `{base, pages: [paths], min_text?, login?: {path, user, pass,
user_sel, pass_sel, submit_sel}, block?, insecure_tls?}`.
Behavior: optionally perform the login flow first (load `base+path`, set the
value of the elements matching `user_sel`/`pass_sel`, dispatch a submit on the
form containing `submit_sel` or click `submit_sel`, follow the resulting
navigation, carrying cookies for the rest of the tour). Then visit each
`base+page` with F1 semantics and return
`{pages: [{page, ok, status, text_len, console_errors, page_errors,
failed_requests, timing_ms}], summary: {visited, failed}}`.
A page is `ok:false` when status ≥ 400, any page_error occurred, or
`text_len < min_text` (default 50).

### F3 — DOM implementation
Parse with html5ever into an owned DOM tree. JS-visible API surface (bound into
V8) must include at least: `document.querySelector/querySelectorAll` (CSS
selectors: tag, `#id`, `.class`, attribute `[a=v]`, descendant and child
combinators), `getElementById`, `createElement`, `createTextNode`,
`appendChild`/`removeChild`/`insertBefore`, `innerHTML` (get/set with re-parse),
`outerHTML` (get), `textContent` (get/set), `getAttribute`/`setAttribute`/
`removeAttribute`, `classList` (add/remove/contains/toggle), `dataset` (read),
`parentNode`/`children`/`childNodes`/`firstChild`/`nextSibling`, `tagName`,
`id`/`className` (get/set), `value` (get/set on inputs/textareas/selects),
`style` (write-accepting no-op object is acceptable), `addEventListener` +
`dispatchEvent` with working synthetic `click`/`input`/`submit` events
(bubbling through ancestors), `document.title` (get/set),
`document.body`/`document.documentElement`/`document.head`,
`document.createElement`-built nodes insertable into the live tree,
`window.location` (href/origin/pathname/search, read), `navigator.userAgent`.
Unsupported DOM/CSS APIs must degrade safely (defined no-op or sane default),
never crash the process. `getComputedStyle` returns an object whose property
reads return `""`; `offsetWidth`/`offsetHeight`/`getBoundingClientRect` return
zeros.

### F4 — JavaScript execution
Use the `v8` crate (rusty_v8). One isolate per page job, taken from a pool.
Must execute: inline `<script>`, external `<script src>` (fetched, classic
scripts; `type=module` may be skipped with a console warning recorded),
scripts injected via DOM after load. Provide: `setTimeout`/`clearTimeout`/
`setInterval`/`clearInterval`/`queueMicrotask`, Promises integrated with the
event loop, `fetch()` (Response with `ok/status/text()/json()/headers.get`),
`XMLHttpRequest` (open/send/onreadystatechange/onload/status/responseText,
async), `console.log/warn/error` (error captured per F1), `JSON`, `atob`/`btoa`,
`encodeURIComponent` family. Uncaught exceptions and unhandled rejections are
captured as page_errors, never process-fatal.

### F5 — Networking
`reqwest` + rustls. Per-job cookie jar (shared across a tour, isolated between
jobs). Subresource and fetch/XHR requests: honor redirects, gzip/br, respect
the `block` filter, log every request outcome for `responses`/
`failed_requests`. Same-origin policy is NOT enforced (server-side tool);
document this. Per-request timeout 15 s; per-job hard wall-clock cap 60 s.

### F6 — Service endpoints
`GET /healthz` → 200 `ok`. `GET /metrics` → Prometheus text format with at
least: process RSS bytes, jobs served counter, jobs failed counter, in-flight
jobs gauge, isolate pool recycle counter. Bind address/port via `--bind`
(default `127.0.0.1:8200`). Structured request logging to stdout.

### F7 — Resource governance ("leak firewall")
Isolates carry a V8 heap limit (default 128 MB, `--js-heap-mb`). A page job
whose heap limit is hit, or that exceeds the 60 s wall cap (including infinite
JS loops — enforce via isolate termination from a watchdog thread), fails with
a structured error while other concurrent jobs continue. Pool isolates are
recycled (dropped and recreated) after N jobs (default 20, `--recycle-after`).
A panic in one job must not kill the process (catch at the job boundary).
Concurrency: `--max-jobs` (default 4); excess requests queue.

## Non-functional requirements

- **N1**: `cargo build --release` succeeds on stable Rust, Linux x86_64. The
  release binary runs standalone (glibc dynamic linking acceptable for MVP;
  musl is a stretch goal, not required).
- **N2**: `cargo clippy --all-targets -- -D warnings` clean; `cargo test` green.
- **N3**: Unit + integration tests: DOM/selector tests, JS-binding tests, and
  an integration suite that starts falcon against a bundled local fixture
  server (see A-tests) — runnable offline.
- **N4**: `Dockerfile` producing a runnable image (multi-stage, release build,
  non-root). Building the image is environment-dependent: if no docker daemon
  is available, the Dockerfile is delivered and image build is a manual
  verification item.
- **N5**: Workspace layout with clear module boundaries (dom / js / net /
  server — single crate with modules is acceptable at MVP; document the split).
- **N6**: README stating what falcon is and is NOT (the Class A/B scoping from
  Context, verbatim in spirit), API docs for F1/F2 with curl examples, and a
  `PARITY.md` listing every knowingly-unsupported web API and why skipping it
  is safe for Class B work.
- **N7**: No `unsafe` outside the V8 binding layer.

## Acceptance tests (deterministic, runnable on this machine)

Fixture server: a small bundled test server (Rust, part of the repo, e.g.
`falcon-fixtures` bin or test harness) serving pages that cover:
static HTML; JS-built DOM (script creates elements + text after load);
fetch-then-render (JSON endpoint + DOM update); setTimeout-deferred render;
console.error page; uncaught-exception page; 404 subresource page;
login form (cookie session, /private 302→login when unauthenticated);
infinite-loop script page; huge-allocation script page.

- **A1**: `/v1/extract` on the static fixture returns its text/title/html;
  `responses` lists the document request.
- **A2**: extract on JS-built-DOM, fetch-then-render, and setTimeout fixtures
  returns the JS-produced text (proves F3+F4 end-to-end); on the console.error /
  exception / 404-subresource fixtures, the corresponding arrays are non-empty
  and correctly attributed.
- **A3**: `/v1/tour` with `login` on the fixture site reaches `/private`
  authenticated (its marker text present, `ok:true`) and reports the
  unauthenticated marker absent.
- **A4 (parity)**: a provided script (`scripts/parity.py`, may use the locally
  cached Playwright headless-shell Chromium) runs the same fixture tour in
  Chromium and in falcon and asserts: identical ok/fail verdict per fixture
  page and same *sets* of error categories (console error present/absent,
  page error present/absent, failed request present/absent). Textual equality
  of messages is NOT required.
- **A5 (resources)**: a provided script measures falcon RSS after a 100-page
  sequential fixture tour with 4-way concurrency bursts: steady-state RSS
  < 150 MB and no monotonic growth (final RSS within 15% of RSS after warmup);
  and falcon's RSS is ≥ 3× smaller than the Chromium headless-shell process
  tree serving the same tour via the parity script.
- **A6 (containment)**: while a tour is running, an extract of the
  infinite-loop fixture and one of the huge-allocation fixture both return
  structured errors in < 65 s, the process stays alive, and the concurrent
  tour completes correctly.
- **A7 (real world, informational — not a PASS gate)**: extract of 3 public
  URLs is attempted and results recorded in the review report. Network
  variability means this cannot gate PASS; failures here are reported, not
  fixed-looped.

## Out of scope for this MVP (do not build)

CDP/WebSocket protocol, screenshots/layout/paint, WebGL/video/audio,
`type=module` scripts, Service Workers, WebSockets in-page, cargo-fuzz targets,
musl static linking, multi-arch images, anti-bot/stealth of any kind,
persistent login profiles across jobs.
