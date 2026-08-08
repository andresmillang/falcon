#!/usr/bin/env python3
"""Performance benchmark (R44/D12). Reports Falcon's idle/peak/steady RSS, memory
growth across repeated batches, startup latency, per-page latency, and
throughput under concurrency. Informational — the only hard bars (checked in the
acceptance suite) are 'dramatically lighter than Chromium' and 'no leak growth'.
"""
import argparse
import concurrent.futures as cf
import json
import statistics
import subprocess
import time
import urllib.request


def rss_bytes(falcon):
    txt = urllib.request.urlopen(falcon + "/metrics", timeout=5).read().decode()
    for line in txt.splitlines():
        if line.startswith("falcon_rss_bytes "):
            return int(float(line.split()[1]))
    return 0


def extract(falcon, url):
    body = json.dumps({"url": url}).encode()
    req = urllib.request.Request(falcon + "/v1/extract", body, {"content-type": "application/json"})
    t0 = time.time()
    urllib.request.urlopen(req, timeout=30).read()
    return (time.time() - t0) * 1000


def mb(b):
    return b / (1024 * 1024)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--falcon", default="http://127.0.0.1:8200")
    ap.add_argument("--fixtures", default="http://127.0.0.1:8300")
    ap.add_argument("--binary", default="./target/release/falcon")
    args = ap.parse_args()
    url = args.fixtures + "/fetchrender"

    # Startup latency: launch a throwaway instance and time until /healthz.
    port = 8590
    t0 = time.time()
    proc = subprocess.Popen([args.binary, "--bind", f"127.0.0.1:{port}"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    startup_ms = None
    for _ in range(200):
        try:
            if urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1).read() == b"ok":
                startup_ms = (time.time() - t0) * 1000
                break
        except Exception:
            time.sleep(0.02)
    idle_rss = rss_bytes(f"http://127.0.0.1:{port}") if startup_ms else 0
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except Exception:
        proc.kill()

    # Per-page latency (single-threaded) against the main instance.
    warm = [extract(args.falcon, url) for _ in range(5)]
    lat = [extract(args.falcon, url) for _ in range(30)]

    # Throughput under 4-way concurrency (100 pages).
    t0 = time.time()
    with cf.ThreadPoolExecutor(max_workers=4) as ex:
        list(ex.map(lambda _: extract(args.falcon, url), range(100)))
    dur = time.time() - t0
    throughput = 100 / dur

    # Peak + steady + leak across two batches.
    b1_start = rss_bytes(args.falcon)
    with cf.ThreadPoolExecutor(max_workers=4) as ex:
        list(ex.map(lambda _: extract(args.falcon, url), range(100)))
    steady = rss_bytes(args.falcon)
    with cf.ThreadPoolExecutor(max_workers=4) as ex:
        list(ex.map(lambda _: extract(args.falcon, url), range(100)))
    after2 = rss_bytes(args.falcon)
    growth = (after2 - steady) / steady * 100 if steady else 0

    print("=== Falcon performance (R44) ===")
    print(f"startup latency:      {startup_ms:.0f} ms" if startup_ms else "startup: FAILED")
    print(f"idle RSS:             {mb(idle_rss):.1f} MB")
    print(f"steady-state RSS:     {mb(steady):.1f} MB")
    print(f"peak RSS (after 2nd): {mb(after2):.1f} MB")
    print(f"memory growth batch2: {growth:.1f}%  ({'no leak' if growth <= 15 else 'REGRESSION'})")
    print(f"page latency p50/p95: {statistics.median(lat):.0f} / {sorted(lat)[int(len(lat)*0.95)]:.0f} ms")
    print(f"throughput (4-way):   {throughput:.0f} pages/sec")
    return 0


if __name__ == "__main__":
    import sys
    sys.exit(main())
