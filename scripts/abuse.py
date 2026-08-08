#!/usr/bin/env python3
"""Resource-abuse suite (R25/D4). Drives all six pathological pages through
Falcon and asserts each terminates with the correct classified reason in a
bounded time, the process survives, and (R23) CPU-bound cases finish within a
few seconds. Also checks a concurrent normal job completes while jobs are stuck
(D5). Requires the fixture server (with abuse routes) and a Falcon instance
started with a tight exec budget for a fast demonstration.
"""
import argparse
import concurrent.futures as cf
import json
import time
import urllib.request

# fixture path -> acceptable classified reasons
CASES = {
    "infinite": {"exec_timeout", "wall_timeout"},
    "hugealloc": {"exec_timeout", "memory_limit"},
    "fetchloop": {"resource_limit"},
    "recursivetimer": {"resource_limit"},
    "hugehtml": {"resource_limit"},          # tested with a response cap
    "redirectloop": {None},                  # bounded failed request, not a hang
}


def extract(falcon, path, fixtures, extra=None):
    body = {"url": f"{fixtures}/{path}"}
    if extra:
        body.update(extra)
    req = urllib.request.Request(falcon + "/v1/extract", json.dumps(body).encode(),
                                 {"content-type": "application/json"})
    t0 = time.time()
    d = json.loads(urllib.request.urlopen(req, timeout=70).read())
    return d, time.time() - t0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--falcon", default="http://127.0.0.1:8200")
    ap.add_argument("--fixtures", default="http://127.0.0.1:8301")
    args = ap.parse_args()

    fails = 0
    for path, ok_reasons in CASES.items():
        extra = {"limits": {"max_response_bytes": 500000}} if path == "hugehtml" else None
        d, dt = extract(args.falcon, path, args.fixtures, extra)
        reason = d.get("metrics", {}).get("limit_reason")
        alive = urllib.request.urlopen(args.falcon + "/healthz", timeout=3).read() == b"ok"
        # redirectloop: reason None but must be a bounded failed navigation
        ok = (reason in ok_reasons) and alive and dt < 65
        if path == "redirectloop":
            ok = alive and dt < 65 and (d["status"] == 0 or d.get("failed_requests"))
        # R23: CPU-bound cases should be fast
        fast = dt < 20 if path in ("infinite", "hugealloc", "fetchloop", "recursivetimer") else True
        status = "PASS" if (ok and fast) else "FAIL"
        if status == "FAIL":
            fails += 1
        print(f"  {status}: {path:<16} reason={reason} time={dt:.1f}s alive={alive}")

    # D5: concurrency — start two stuck jobs, ensure a normal tour completes fast.
    print("  --- concurrency (D5) ---")
    with cf.ThreadPoolExecutor(max_workers=3) as ex:
        stuck1 = ex.submit(extract, args.falcon, "infinite", args.fixtures)
        stuck2 = ex.submit(extract, args.falcon, "recursivetimer", args.fixtures)
        time.sleep(1)
        t0 = time.time()
        normal, _ = extract(args.falcon, "ping", args.fixtures) if False else ({}, 0)
        # use a real content page for the normal job
        body = json.dumps({"url": f"{args.fixtures}/fetchrender"}).encode()
        req = urllib.request.Request(args.falcon + "/v1/extract", body, {"content-type": "application/json"})
        nd = json.loads(urllib.request.urlopen(req, timeout=20).read())
        ndt = time.time() - t0
        conc_ok = ("fetched-content-marker" in nd["text"]) and ndt < 15
        print(f"  {'PASS' if conc_ok else 'FAIL'}: concurrent normal job completed in {ndt:.1f}s while 2 jobs stuck")
        if not conc_ok:
            fails += 1
        stuck1.result(); stuck2.result()

    print()
    print("ABUSE SUITE: ALL PASS" if fails == 0 else f"ABUSE SUITE: {fails} FAILED")
    return 0 if fails == 0 else 1


if __name__ == "__main__":
    import sys
    sys.exit(main())
