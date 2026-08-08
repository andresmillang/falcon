# PARITY.md — knowingly-unsupported web APIs

falcon targets **Class B** work (page-tour error checking, HTML/text extraction,
public-page and JSON-API-backed SPA reads). The following are intentionally
unsupported. Each entry states why skipping it is safe for Class B.

## Rendering / visual
- **Layout & paint** (`getBoundingClientRect`, `offsetWidth/Height` return 0;
  `getComputedStyle` returns empty values). Class B reads the DOM and text, not
  geometry. Pages that branch on measured layout are rare and out of scope.
- **Screenshots / canvas raster / WebGL / video / audio.** Class B never needs
  pixels. This is the single largest cost falcon avoids versus Chromium.
- **CSS application.** Stylesheets are fetched (so 404s are reported) but not
  applied. Visibility-by-CSS is not modeled; text extraction includes all
  non-script/style text.

## Scripting surface
- **ES modules (`<script type=module>`, dynamic `import()`).** Recorded as a
  console note and skipped. Most server-rendered and classic-bundle SPAs still
  run; module-only apps are a known gap. Adding a module loader is the most
  likely future extension.
- **Service Workers, Web Workers, `SharedWorker`.** No background threads. Class
  B reads the main document; offline/caching layers are irrelevant to a one-shot
  fetch.
- **WebSockets, EventSource (SSE) in-page.** falcon returns after network-idle;
  long-lived streams are not consumed. Class B extraction does not depend on
  them.
- **`MutationObserver`** is a safe no-op stub; **`IntersectionObserver`/
  `ResizeObserver`** are stubbed. These drive lazy-rendering/animation, not the
  initial content Class B reads. `requestAnimationFrame` is mapped to a timer.

## Networking / security
- **Same-origin policy / CORS are not enforced.** falcon is a server-side tool
  driven by a trusted operator, not a user agent protecting a logged-in human.
  All fetches run; this is intentional and documented.
- **Anti-bot stealth / TLS-JA3 / HTTP2 fingerprint shaping.** Explicitly out of
  scope — that is Class A (Chromium). falcon identifies as `falcon/<version>`.
- **Persistent auth across jobs.** Each `/v1/extract` job is isolated. A tour
  shares one cookie jar for its own pages only.

## Storage / device
- **localStorage / sessionStorage / IndexedDB / cookies-via-`document.cookie`**
  read as empty/no-ops (HTTP cookies are still handled by the network layer for
  tours). Class B reads content, not client-persisted state.
- **Geolocation, notifications, clipboard, media devices, WebRTC.** Stubbed or
  absent. No Class B task uses them.

## Parity evidence
`scripts/parity.py` runs the fixture tour through real Chromium
(chrome-headless-shell via CDP) and through falcon and asserts identical
ok/fail verdicts and identical error-category sets (console-error, page-error,
failed-request present/absent) per page. Textual equality of error *messages* is
not required — only that both agree a page is healthy or broken, and why.
