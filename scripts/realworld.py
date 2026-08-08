#!/usr/bin/env python3
"""Real-world compatibility corpus (R43/D11). Runs Falcon against a set of public
sites (no bot-protection bypass), measures extraction/JS success, page errors,
timeout rate, memory, and wall-clock time, and categorizes failures by missing
capability. Informational — reports numbers, does not gate. Needs internet.
"""
import argparse
import json
import time
import urllib.request

SITES = [
    "https://example.com/",
    "https://www.rust-lang.org/",
    "https://news.ycombinator.com/",
    "https://www.wikipedia.org/",
    "https://httpbin.org/html",
    "https://www.gnu.org/",
    "https://developer.mozilla.org/en-US/",
    "https://blog.rust-lang.org/",
]


def extract(falcon, url):
    body = json.dumps({"url": url, "block": ["image", "font", "media"]}).encode()
    req = urllib.request.Request(falcon + "/v1/extract", body, {"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(req, timeout=45).read())


def categorize(d):
    if d.get("limit_reason"):
        return d["limit_reason"]
    if d["status"] == 0:
        return "navigation_failed"
    if d["status"] >= 400:
        return f"http_{d['status']}"
    if len(d["text"]) < 200:
        return "low_text"
    if d["page_errors"]:
        return "js_error"
    return "ok"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--falcon", default="http://127.0.0.1:8200")
    args = ap.parse_args()
    rows = []
    cats = {}
    for u in SITES:
        t0 = time.time()
        try:
            d = extract(args.falcon, u)
            cat = categorize(d)
            rows.append((u, d["status"], len(d["text"]), len(d["page_errors"]),
                         d.get("metrics", {}).get("rss_delta_bytes", 0), int((time.time() - t0) * 1000), cat))
        except Exception as e:
            cat = "timeout_or_error"
            rows.append((u, 0, 0, 0, 0, int((time.time() - t0) * 1000), cat))
        cats[cat] = cats.get(cat, 0) + 1

    print(f"{'site':<45} {'status':>6} {'text':>7} {'perr':>5} {'ms':>6}  category")
    for u, st, tl, pe, rss, ms, cat in rows:
        print(f"{u[:44]:<45} {st:>6} {tl:>7} {pe:>5} {ms:>6}  {cat}")
    ok = sum(1 for r in rows if r[6] == "ok")
    extracted = sum(1 for r in rows if r[1] == 200 and r[2] >= 200)
    print()
    print(f"REAL-WORLD: {len(rows)} sites | full-success={ok} extracted(status200,text>=200)={extracted}")
    print("failure categories (highest-return capability gaps first):",
          json.dumps({k: v for k, v in sorted(cats.items(), key=lambda x: -x[1]) if k != "ok"}))
    return 0


if __name__ == "__main__":
    import sys
    sys.exit(main())
