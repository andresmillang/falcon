#!/usr/bin/env python3
"""A5 resource harness: drive a 100-page fixture tour through falcon with 4-way
concurrency and measure steady-state RSS + growth; then measure chrome-headless-
shell's process-tree RSS serving the same tour, and assert falcon uses >=3x less.

Usage: resources.py --falcon http://127.0.0.1:8200 --fixtures http://127.0.0.1:8300
falcon + fixtures must already be running (falcon --max-jobs 4).
"""
import argparse
import concurrent.futures as cf
import glob
import json
import os
import subprocess
import sys
import time
import urllib.request

import websocket

N_PAGES = 100
CONCURRENCY = 4
# A mix so the tour isn't a single cached page.
ROUTES = ["/", "/jsdom", "/fetchrender", "/xhr", "/deferred"]


def falcon_rss_bytes(falcon):
    txt = urllib.request.urlopen(falcon + "/metrics", timeout=5).read().decode()
    for line in txt.splitlines():
        if line.startswith("falcon_rss_bytes "):
            return int(float(line.split()[1]))
    return 0


def falcon_extract(falcon, url):
    body = json.dumps({"url": url}).encode()
    req = urllib.request.Request(falcon + "/v1/extract", body,
                                 {"content-type": "application/json"})
    urllib.request.urlopen(req, timeout=30).read()


def _batch(falcon, urls):
    with cf.ThreadPoolExecutor(max_workers=CONCURRENCY) as ex:
        list(ex.map(lambda u: falcon_extract(falcon, u), urls))


def run_falcon_tour(falcon, fixtures):
    urls = [fixtures + ROUTES[i % len(ROUTES)] for i in range(N_PAGES)]
    # Warm to steady state, then compare a SECOND 100-page batch against the
    # first: cold-start allocator growth is excluded, so any delta here is a
    # genuine monotonic leak. This is the faithful "no growth" test.
    for u in urls[:25]:
        falcon_extract(falcon, u)
    _batch(falcon, urls)          # first full batch → steady state
    time.sleep(0.5)
    warm = falcon_rss_bytes(falcon)
    _batch(falcon, urls)          # second full batch
    time.sleep(0.5)
    final = falcon_rss_bytes(falcon)
    return warm, final


def find_chrome():
    p = subprocess.run(["which", "chrome-headless-shell"], capture_output=True, text=True)
    if p.returncode == 0:
        return p.stdout.strip()
    cands = glob.glob(os.path.expanduser(
        "~/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-linux64/chrome-headless-shell"))
    return sorted(cands)[-1]


def proc_tree_rss_bytes(pid):
    pids = {pid}
    # collect descendants
    changed = True
    while changed:
        changed = False
        try:
            out = subprocess.run(["ps", "-eo", "pid,ppid"], capture_output=True, text=True).stdout
        except Exception:
            break
        for line in out.splitlines()[1:]:
            parts = line.split()
            if len(parts) < 2:
                continue
            p, pp = int(parts[0]), int(parts[1])
            if pp in pids and p not in pids:
                pids.add(p)
                changed = True
    total = 0
    for p in pids:
        try:
            with open(f"/proc/{p}/status") as fh:
                for l in fh:
                    if l.startswith("VmRSS:"):
                        total += int(l.split()[1]) * 1024
        except Exception:
            pass
    return total


def run_chromium_tour(fixtures):
    chrome = find_chrome()
    port = 9444
    proc = subprocess.Popen(
        [chrome, f"--remote-debugging-port={port}", "--headless=new",
         "--remote-allow-origins=*", "--disable-gpu", "--no-sandbox",
         "--disable-dev-shm-usage", "about:blank"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        base_ws = None
        for _ in range(50):
            try:
                v = json.loads(urllib.request.urlopen(
                    f"http://localhost:{port}/json/version", timeout=1).read())
                base_ws = v["webSocketDebuggerUrl"]
                break
            except Exception:
                time.sleep(0.2)
        if not base_ws:
            print("chromium did not start", file=sys.stderr)
            return None, None
        ws = websocket.create_connection(base_ws, max_size=None)
        mid = [0]

        def send(method, params=None, session=None):
            mid[0] += 1
            m = {"id": mid[0], "method": method, "params": params or {}}
            if session:
                m["sessionId"] = session
            ws.send(json.dumps(m))
            while True:
                d = json.loads(ws.recv())
                if d.get("id") == mid[0]:
                    return d.get("result", {})

        # A pool of 4 tabs reused across the tour (fair vs falcon's 4 workers).
        tabs = []
        for _ in range(CONCURRENCY):
            t = send("Target.createTarget", {"url": "about:blank"})["targetId"]
            s = send("Target.attachToTarget", {"targetId": t, "flatten": True})["sessionId"]
            send("Page.enable", {}, session=s)
            tabs.append(s)

        urls = [fixtures + ROUTES[i % len(ROUTES)] for i in range(N_PAGES)]
        for i, u in enumerate(urls):
            s = tabs[i % CONCURRENCY]
            send("Page.navigate", {"url": u}, session=s)
            time.sleep(0.02)
        time.sleep(1.0)
        rss = proc_tree_rss_bytes(proc.pid)
        ws.close()
        return rss, chrome
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()


def mb(b):
    return b / (1024 * 1024)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--falcon", default="http://127.0.0.1:8200")
    ap.add_argument("--fixtures", default="http://127.0.0.1:8300")
    args = ap.parse_args()

    warm, final = run_falcon_tour(args.falcon, args.fixtures)
    print(f"falcon RSS: warmup={mb(warm):.1f}MB  final={mb(final):.1f}MB "
          f"(after {N_PAGES} pages, {CONCURRENCY}-way)")
    chrome_rss, _ = run_chromium_tour(args.fixtures)
    print(f"chromium process-tree RSS after same tour: {mb(chrome_rss):.1f}MB")

    ok = True
    if mb(final) >= 150:
        print(f"FAIL: falcon steady-state RSS {mb(final):.1f}MB >= 150MB"); ok = False
    else:
        print(f"PASS: falcon steady-state RSS {mb(final):.1f}MB < 150MB")

    grow = (final - warm) / warm * 100 if warm else 0
    if final > warm * 1.15:
        print(f"FAIL: RSS grew {grow:.1f}% (>15%) warmup->final"); ok = False
    else:
        print(f"PASS: RSS growth {grow:.1f}% (<=15%) — no monotonic leak")

    if chrome_rss and final * 3 <= chrome_rss:
        print(f"PASS: falcon RSS is {chrome_rss/final:.1f}x smaller than Chromium (>=3x)")
    else:
        r = (chrome_rss / final) if (chrome_rss and final) else 0
        print(f"FAIL: falcon RSS only {r:.1f}x smaller than Chromium (<3x)"); ok = False

    print("\nRESOURCE PASS" if ok else "\nRESOURCE FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
