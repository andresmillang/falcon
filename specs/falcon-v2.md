# Falcon v2 — production-grade lightweight browser runtime — Specification

> Status: Draft · Date: 2026-08-08 · Author: interview via /spec

## Objective

Evolve Falcon from its working MVP into a stronger production-grade **Class B**
runtime while protecting its core advantage (very low memory, fast startup,
isolated jobs, agent-focused). The product direction is explicit: **implement
the browser functionality machines actually need, and escalate everything else
to Chromium.** Falcon must never drift toward being a general-purpose browser.

Depth-first priorities for this version: land and Chromium-parity-verify ES
modules, high-value browser APIs, a hardened resource firewall, better
network/browser semantics, an optional live Falcon→Chromium fallback, and
per-job observability. A larger deterministic parity corpus, a real-world site
corpus, and performance benchmarks are produced as **evidence** and reported
honestly — they must not be weakened into box-checking, and they do not gate the
core verdict.

## Requirements

### Must have — Priority 1: ES module support

- **R1**: `<script type="module">` executes (MVP skipped it). Module scripts are
  deferred: they run after the document is parsed and after classic scripts,
  before `DOMContentLoaded`.
- **R2**: Static `import ... from "<spec>"` resolves the specifier against the
  importing module's own URL (relative like `./a.js`, `../b.js`, and absolute
  `http(s)://…`), fetches the dependency, and executes dependencies before
  dependents (topological order) across nested graphs of arbitrary depth.
- **R3**: Module identity/caching is by resolved URL: a module imported more
  than once within a single job is fetched and evaluated exactly once.
- **R4**: `import` supports default, named, and namespace (`* as ns`) bindings;
  imported values are readable in the importer (snapshot semantics are
  acceptable; live-binding mutation is not required).
- **R5**: Dynamic `import("<spec>")` returns a Promise that resolves to the
  module namespace object and integrates with the event loop; a failing dynamic
  import rejects.
- **R6**: A module fetch failure (HTTP ≥400 or network error) is reported in
  `page_errors` with the failing module URL, and modules depending on it do not
  execute; unrelated modules still run.
- **R7**: Module evaluation errors (throw at top level) are captured in
  `page_errors`, never fatal to the process.
- **R8**: Deterministic acceptance fixtures exist and are Chromium-compared for:
  single module, nested imports, multiple imports, failed import, dynamic
  import, a module that modifies the DOM, and a module that performs fetch/XHR.
  Comparison covers final text, key DOM state, title, network failures, console
  errors, uncaught exceptions, and the success/failure verdict.

### Must have — Priority 2: browser API compatibility

Each API below is implemented in the shim and gets a Chromium parity fixture.
Obscure APIs are explicitly NOT added.

- **R9**: `MutationObserver` — `observe(target, opts)` (childList, subtree,
  attributes), `disconnect()`, `takeRecords()`; callbacks fire as a microtask
  after DOM mutations with correct `MutationRecord` type/target/addedNodes/
  removedNodes.
- **R10**: `URL` and `URLSearchParams` — parse, `href/origin/protocol/host/
  hostname/pathname/search/hash`, and params `get/getAll/set/append/delete/has/
  toString` plus iteration.
- **R11**: `location` — `href/origin/protocol/host/hostname/pathname/search/
  hash`, `assign/replace/reload` as safe no-ops (no real navigation in a
  one-shot job).
- **R12**: `history.pushState/replaceState` — updates `location.pathname/search/
  hash` and `history.state`/`length`; `popstate` support is optional.
- **R13**: `localStorage` and `sessionStorage` — `getItem/setItem/removeItem/
  clear/key/length`, string coercion, per-job isolation, persistence within a
  job. Not shared across jobs.
- **R14**: `requestAnimationFrame`/`cancelAnimationFrame` — callback receives a
  timestamp; driven by the event loop; cancel works.
- **R15**: `AbortController`/`AbortSignal` — `abort(reason)` sets
  `signal.aborted`, fires an `abort` event; `fetch(url,{signal})` rejects with an
  AbortError-shaped error when aborted before/while pending.
- **R16**: Richer `Event`/`CustomEvent` — `detail`, `bubbles`, `cancelable`,
  `preventDefault`, `stopPropagation`, `stopImmediatePropagation`, and
  `addEventListener` `{once, capture}` options honored.
- **R17**: Form elements and submit — `form.elements`, input/textarea/select
  `value`, `form.submit()` and `requestSubmit()`, a `submit` event that bubbles
  and is cancelable; extracting a form's action/method/fields works (used by the
  tour login flow).
- **R18**: Lifecycle correctness — `document.readyState` transitions
  `loading → interactive → complete`; `DOMContentLoaded` fires once after parse
  + deferred/module scripts, `load` fires after; ordering matches the browser
  for classic-inline vs deferred/module scripts.
- **R19**: DOM traversal/manipulation — at least `append/prepend/before/after/
  replaceWith/replaceChildren`, `cloneNode(deep)`, `insertAdjacentHTML`,
  `getAttributeNames`, `children/childElementCount/firstElementChild/
  lastElementChild/nextElementSibling/previousElementSibling`, `matches`,
  `closest`, `contains`, plus the MVP surface.
- **R20**: Stronger CSS selectors — attribute operators `[a=v]`, `[a^=v]`,
  `[a$=v]`, `[a*=v]`, `[a~=v]`, `[a]`; `:not(simple)`; compound (`div.a.b`);
  descendant, child (`>`), adjacent-sibling (`+`), general-sibling (`~`)
  combinators; selector lists (`a, b`). Advanced pseudo-classes beyond `:not`
  are out of scope.

### Must have — Priority 3: resource containment

- **R21**: All of the following are configurable (CLI flag; per-request override
  where sensible): wall-clock timeout, JS-execution/CPU budget, memory ceiling,
  max response/download size, max network requests per job, max redirect depth,
  max timer fires per job, and max DOM node count.
- **R22**: A single job hitting any limit is terminated with a **classified**
  structured error naming the limit; other in-flight jobs are unaffected and the
  Falcon process always survives.
- **R23**: Worst-case termination time for pathological *CPU-bound* pages
  (infinite loop, recursive timers, huge allocation) is reduced substantially
  below the wall-clock cap via a distinct JS-execution watchdog — target: such
  pages terminate within a few seconds (default JS-exec budget ≤ ~8 s), while
  legitimately slow (network-bound) pages may still use the full wall-clock cap.
- **R24**: Job cancellation — an external caller can cancel an in-flight job by a
  job id (server-assigned, returned to the caller) via a cancel endpoint;
  cancellation terminates that job's isolate promptly and is reported as a
  classified reason.
- **R25**: Deterministic resource-abuse fixtures exist and pass (Falcon survives,
  job terminates with the correct classified reason, in a bounded time) for:
  infinite JS loop, recursive timer creation, huge allocation, huge HTML page,
  endless fetch chain, and redirect loop.

### Must have — Priority 4: network/browser semantics

- **R26**: Redirects — followed up to the configured max depth; `final_url`
  reflects the landing URL; a redirect loop terminates as a failed request, not
  a hang.
- **R27**: Cookies — `Set-Cookie` (with path/domain attributes) is honored by the
  per-job jar and sent on same-site subsequent requests within a job/tour; jobs
  are isolated from each other.
- **R28**: Compressed responses — gzip, deflate, brotli, and zstd are decoded
  transparently.
- **R29**: Request cancellation — `AbortController` cancels an in-flight
  `fetch`/XHR (per R15); a per-request timeout also applies.
- **R30**: Referrer — subresource and fetch/XHR requests send a `Referer` header
  derived from the document URL (default no-referrer-when-downgrade-ish; exact
  policy documented).
- **R31**: HTTP methods and headers — `fetch`/XHR honor method and
  request headers; response status and headers are exposed to JS
  (`Response.headers.get`, `XHR.getAllResponseHeaders`).
- **R32**: Relative resource URLs resolve against the document's final URL;
  failed subresources are reported in `failed_requests`; HTTP/2 is enabled via
  reqwest. CORS is intentionally not enforced (server-side tool) and documented;
  no fingerprint spoofing or anti-bot evasion is added.

### Must have — Priority 7: optional live Falcon→Chromium fallback

- **R33**: Fallback is **off by default** and configurable (e.g.
  `--enable-fallback` + `--chromium-cdp <ws-or-launch>`). With it disabled,
  Falcon is a fully standalone binary and Chromium is never contacted.
- **R34**: Chromium is **never** invoked for a successful Falcon job.
- **R35**: Escalation happens only for explicit, classified conditions, exposed
  as `fallback_reason`: `unsupported_es_module_feature`,
  `unsupported_browser_api`, `navigation_timeout`, `resource_limit`,
  `javascript_failure`, `render_incomplete`.
- **R36**: Every `/v1/extract` response carries `engine_used`
  (`"falcon"`|`"chromium"`), `falcon_status` (e.g. `ok`/`failed`/`unsupported`/
  `timeout`/`resource_limit`), `fallback_reason` (null or one of R35), and
  `fallback_used` (bool). Falcon's original errors/diagnostics are preserved
  under a `falcon_diagnostics` object even when Chromium succeeds — failures are
  never silently hidden.
- **R37**: The Chromium fallback path has its own timeout/resource limits (e.g.
  `--fallback-timeout-secs`) so it cannot hang Falcon.
- **R38**: Verified end-to-end scenarios: (a) Falcon success → no fallback,
  `engine_used=falcon`, `fallback_used=false`; (b) Falcon classified failure with
  fallback enabled + backend available → Chromium succeeds, `engine_used=chromium`,
  `fallback_used=true`, `fallback_reason` set, `falcon_diagnostics` preserved;
  (c) both fail → honest combined error, process survives; (d) fallback disabled
  → Falcon failure returned honestly, Chromium not contacted; (e) fallback
  enabled but backend unavailable → `fallback_used=false` with a clear reason,
  Falcon result/diagnostics still returned.

### Must have — Priority 8: observability

- **R39**: Each job records structured per-job metrics, returned in the response
  under `metrics` and emitted as one structured (JSON) log line to stdout:
  `job_duration_ms`, `rss_delta_bytes`, `request_count`, `downloaded_bytes`,
  `js_exec_ms`, `dom_node_count`, `timer_count`, `error_count`,
  `limit_reason` (null or classified), `engine_used`, `fallback_reason`.
- **R40**: `/metrics` (Prometheus) retains the MVP counters and adds at least a
  fallback-used counter and a limit-terminations counter.

### Evidence-building (produced and reported; NOT gating) — Priorities 5, 6, 9

- **R41 (Priority 5)**: A deterministic Chromium parity corpus of **≥ 50**
  strong fixtures (target trajectory 100+), spanning: HTML parsing edge cases,
  DOM manipulation, events, timers, promises/microtasks, fetch, XHR, redirects,
  cookies, storage, modules, malformed JavaScript, uncaught exceptions, failed
  network requests, dynamic content, forms, history/location, MutationObserver,
  async dependency chains, and resource-loading failures. The corpus is
  structured (a directory of fixtures + a manifest) so adding more later is
  trivial. A parity runner compares normalized final text, key DOM state, title,
  network failures, console errors, uncaught exceptions, and verdict — not
  byte-identical HTML — and emits a summary classifying each fixture as
  **PASS**, **KNOWN DIFFERENCE**, or **FALCON BUG**.
- **R42 (Priority 5)**: Every Falcon bug the corpus reveals gets a permanent
  regression fixture added to the corpus, and the bug is fixed (or, if a bug is
  intentionally out of scope, recorded as a documented KNOWN DIFFERENCE with
  rationale).
- **R43 (Priority 6)**: A real-world corpus runs Falcon against a set of public
  websites (no bot-protection bypass), measuring extraction success, JS-execution
  success, page errors, timeout rate, memory, and wall-clock time, and
  categorizes failures by missing capability. Reported as evidence; not a gate.
- **R44 (Priority 9)**: A benchmark reports idle RSS, peak RSS, steady-state RSS,
  memory growth across repeated batches, startup latency, page-processing
  latency, and throughput under concurrency, for Falcon and (where feasible)
  Chromium. Reported as evidence; the only hard bars are R-level: Falcon remains
  **dramatically lighter** than Chromium and shows **no meaningful leak growth**
  across repeated batches.

### Out of scope

- Making Falcon a general-purpose/Chrome-equivalent browser.
- Browser-fingerprint spoofing, TLS/JA3 shaping, or any anti-bot evasion.
- Screenshots, layout/paint, WebGL, video/audio, canvas raster.
- Service Workers, Web Workers, in-page WebSockets/SSE consumption.
- CORS/same-origin enforcement (documented non-enforcement).
- Live import bindings (snapshot import semantics are sufficient).
- Making Chromium a mandatory dependency or silently routing pages through it.
- Hitting an exact fixture count at the expense of fixture quality; hard
  performance thresholds beyond "dramatically lighter + no leak growth".

## Constraints

- Rust, single crate + bins, existing stack: `v8` (135), `html5ever`, `reqwest`
  (rustls), `axum`, `tokio`. Chromium fallback drives the locally available
  `chrome-headless-shell` via CDP (already used by the parity harness).
- Preserve the MVP architecture: html5ever parse → in-V8 DOM shim → Rust-driven
  event loop → isolate-per-job resource firewall. Do not regress it.
- `cargo clippy --all-targets -- -D warnings` must stay clean; no new `unsafe`
  outside the V8 binding layer unless absolutely necessary and explicitly
  justified in code.
- Falcon must remain usable and fully functional as a standalone binary with
  fallback disabled (the default).
- No compatibility improvement may introduce a major unexplained memory
  regression versus the MVP baseline.

## Edge Cases

- **E1**: Circular module imports (`a` imports `b` imports `a`) must not infinite-
  loop; the graph resolves once per URL (R3) and completes or reports an error.
- **E2**: A module importing a missing dependency (404) reports the failing URL
  in `page_errors` (R6) and does not execute dependents; sibling modules run.
- **E3**: `type=module` mixed with classic scripts: classic inline scripts run
  during parse; modules run deferred, after parse, before `DOMContentLoaded`
  (R1/R18) — verified against Chromium ordering.
- **E4**: Malformed JavaScript (syntax error) in a classic script or module is
  reported as a page error and does not abort other scripts or the process (R7).
- **E5**: `MutationObserver` observing a subtree receives records only for the
  configured mutation types; `disconnect()` stops further callbacks (R9).
- **E6**: `localStorage` set in one job is absent in a different job (isolation,
  R13); values are stringified (`setItem('k', 1)` → `getItem('k') === '1'`).
- **E7**: `AbortController.abort()` before a fetch resolves causes the fetch to
  reject and the `abort` event to fire; already-settled fetches are unaffected
  (R15/R29).
- **E8**: Recursive `setTimeout` (a timer that schedules another) is bounded by
  the max-timer-fires limit and the JS-exec/wall watchdog, terminating with the
  correct classified reason within the JS-exec budget (R23/R25).
- **E9**: An endless `fetch().then(fetch...)` chain is bounded by max-requests-
  per-job; the (N+1)th request fails with a classified limit error and the job
  ends bounded (R21/R25).
- **E10**: A redirect loop terminates at max-redirect-depth as a failed request,
  not a hang (R26).
- **E11**: A huge HTML response exceeding max-response-bytes is rejected/truncated
  with a classified reason; a document exceeding max-DOM-nodes terminates the job
  with a classified reason (R21).
- **E12**: Fallback enabled but the CDP backend is unreachable: the job returns
  `fallback_used=false`, a clear reason, and Falcon's own result/diagnostics —
  no crash, no hang (R38e).
- **E13**: Fallback disabled: a Falcon failure is returned honestly with
  `engine_used=falcon`, `fallback_used=false`, and diagnostics — Chromium is not
  contacted (R34/R38d).
- **E14**: A page that only Chromium can render (e.g. an unsupported feature)
  with fallback enabled returns Chromium's result while preserving Falcon's
  `falcon_diagnostics` and setting the correct `fallback_reason` (R36/R38b).
- **E15**: Dynamic `import()` of a missing module rejects; the rejection is
  observable to page JS and, if uncaught, recorded as a page error (R5/R7).

## Definition of Done

Gating items (must pass for v2 to be complete):

- **D1**: `cargo test` passes; `cargo clippy --all-targets -- -D warnings` is
  clean; `grep -rn unsafe src/` shows no new `unsafe` outside the V8 binding
  layer (any such `unsafe` is justified in a comment).
- **D2**: ES-module acceptance fixtures (R8) all pass in the deterministic suite
  and match Chromium on text/DOM/title/errors/network for: single, nested,
  multiple, failed-import, dynamic, DOM-modifying, and fetch/XHR modules.
- **D3**: Each Priority-2 API (R9–R20) has a fixture that passes and matches
  Chromium; run via the acceptance suite and listed in its output.
- **D4**: The resource-abuse suite (R25) passes: infinite loop, recursive timers,
  huge allocation, huge HTML, endless fetch chain, and redirect loop each
  terminate with the correct classified reason, the Falcon process stays alive
  (`/healthz` still `ok`), and CPU-bound cases terminate within the JS-exec
  budget (≤ ~8 s by default) — demonstrably faster than the MVP's 60 s.
- **D5**: A concurrency test shows unrelated jobs complete promptly while
  multiple pathological jobs are running (workers not exhausted).
- **D6**: The fallback scenarios (R38 a–e) are each demonstrated by a runnable
  check with observed `engine_used`/`falcon_status`/`fallback_reason`/
  `fallback_used` values, including preserved `falcon_diagnostics` on Chromium
  success. Default (fallback off) leaves Falcon standalone.
- **D7**: Per-job `metrics` (R39) appear in the `/v1/extract` response and as a
  structured stdout log line; `/metrics` exposes the new counters (R40).
- **D8**: The MVP acceptance suite (A1–A7 equivalent) still passes unchanged —
  no regression of existing behavior.
- **D9**: Repeated-batch memory test shows steady-state RSS with no meaningful
  growth (final within ~15% of a warmed baseline across a second batch) and
  Falcon RSS remains ≥3× smaller than Chromium on the same workload.

Evidence items (produced and reported honestly; not gating):

- **D10**: The parity runner produces a summary over the ≥50-fixture corpus
  (R41) classifying each fixture PASS / KNOWN DIFFERENCE / FALCON BUG, with the
  totals reported. All discovered FALCON BUGs are either fixed with a regression
  fixture added (R42) or recorded as documented KNOWN DIFFERENCEs.
- **D11**: The real-world corpus (R43) is run and its results (success rate,
  failure categories, RSS, latency) reported.
- **D12**: The benchmark (R44) is run and its numbers reported.

Environment-permitting verification (report result or why not runnable):

- **D13**: `insecure_tls:true` verified against a local self-signed HTTPS
  endpoint (200 instead of a TLS error).
- **D14**: Docker image builds and a container answers `/healthz` with `ok`
  (if a Docker daemon is available).

## Open Questions

- **Referrer policy exactness** (R30): default assumption — send the document's
  final URL as `Referer` on same-scheme requests, omit on downgrade to a less
  secure scheme. Document the chosen policy; do not implement the full
  Referrer-Policy matrix.
- **Job-id transport for cancellation** (R24): default assumption — the server
  assigns a job id returned in a response header/body and cancellation is
  `POST /v1/cancel {id}`; a client may also pass its own id. Exact wire shape
  chosen at build time and documented.
