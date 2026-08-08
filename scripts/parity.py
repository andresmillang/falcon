#!/usr/bin/env python3
"""A4 parity harness: run the same fixture tour through Chromium (chrome-
headless-shell via CDP) and through falcon, and assert identical ok/fail
verdicts and identical error-category sets per page.

Usage: parity.py --falcon http://127.0.0.1:8200 --fixtures http://127.0.0.1:8300
Requires: chrome-headless-shell on PATH or in the ms-playwright cache; the
websocket-client package (see .venv). falcon + fixtures already running.
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.request
import glob

import websocket  # from .venv

# Pages to compare and the marker text each healthy page must contain.
PAGES = ["/", "/jsdom", "/fetchrender", "/xhr", "/deferred",
         "/consoleerror", "/exception", "/badsub"]
MIN_TEXT = 50


def find_chrome():
    for c in ["chrome-headless-shell", "chromium-headless-shell"]:
        p = subprocess.run(["which", c], capture_output=True, text=True)
        if p.returncode == 0:
            return p.stdout.strip()
    cands = glob.glob(os.path.expanduser(
        "~/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-linux64/chrome-headless-shell"))
    if cands:
        return sorted(cands)[-1]
    print("chrome-headless-shell not found", file=sys.stderr)
    sys.exit(2)


class CDP:
    def __init__(self, ws_url):
        self.ws = websocket.create_connection(ws_url, max_size=None)
        self.id = 0
        self.events = []

    def send(self, method, params=None, session=None):
        self.id += 1
        mid = self.id
        msg = {"id": mid, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        self.ws.send(json.dumps(msg))
        # read until we see the response with this id, buffering events
        while True:
            data = json.loads(self.ws.recv())
            if data.get("id") == mid:
                return data.get("result", {})
            if "method" in data:
                self.events.append(data)

    def drain(self, seconds):
        end = time.time() + seconds
        self.ws.settimeout(0.3)
        while time.time() < end:
            try:
                data = json.loads(self.ws.recv())
            except Exception:
                continue
            if "method" in data:
                self.events.append(data)

    def close(self):
        try:
            self.ws.close()
        except Exception:
            pass


def chromium_verdict(base_ws, url):
    cdp = CDP(base_ws)
    target = cdp.send("Target.createTarget", {"url": "about:blank"})
    tid = target["targetId"]
    sess = cdp.send("Target.attachToTarget", {"targetId": tid, "flatten": True})["sessionId"]
    for dom in ("Page", "Runtime", "Log", "Network"):
        cdp.send(f"{dom}.enable", {}, session=sess)
    cdp.events.clear()
    cdp.send("Page.navigate", {"url": url}, session=sess)
    cdp.drain(3.0)

    console_error = page_error = failed_request = False
    doc_status = 0
    for ev in cdp.events:
        if ev.get("sessionId") != sess:
            continue
        m = ev["method"]
        p = ev.get("params", {})
        if m == "Runtime.consoleAPICalled" and p.get("type") == "error":
            console_error = True
        if m == "Log.entryAdded" and p.get("entry", {}).get("level") == "error":
            # network 404s also surface as Log errors; keep as failed_request signal
            src = p.get("entry", {}).get("source")
            if src == "network":
                failed_request = True
            else:
                console_error = True
        if m == "Runtime.exceptionThrown":
            page_error = True
        if m == "Network.responseReceived":
            r = p.get("response", {})
            if p.get("type") == "Document":
                doc_status = r.get("status", 0)
            if r.get("status", 0) >= 400:
                failed_request = True
        if m == "Network.loadingFailed":
            failed_request = True

    tl = cdp.send("Runtime.evaluate",
                  {"expression": "document.body?document.body.innerText.length:0",
                   "returnByValue": True}, session=sess)
    text_len = tl.get("result", {}).get("value", 0) or 0
    cdp.send("Target.closeTarget", {"targetId": tid})
    cdp.close()
    ok = doc_status < 400 and not page_error and text_len >= MIN_TEXT
    return {"ok": ok, "console_error": console_error,
            "page_error": page_error, "failed_request": failed_request}


def falcon_verdict(falcon, url):
    body = json.dumps({"url": url}).encode()
    req = urllib.request.Request(falcon + "/v1/extract", body,
                                 {"content-type": "application/json"})
    d = json.loads(urllib.request.urlopen(req, timeout=30).read())
    ok = d["status"] < 400 and not d["page_errors"] and len(d["text"]) >= MIN_TEXT
    return {"ok": ok,
            "console_error": len(d["console_errors"]) > 0,
            "page_error": len(d["page_errors"]) > 0,
            "failed_request": len(d["failed_requests"]) > 0}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--falcon", default="http://127.0.0.1:8200")
    ap.add_argument("--fixtures", default="http://127.0.0.1:8300")
    args = ap.parse_args()

    chrome = find_chrome()
    port = 9333
    proc = subprocess.Popen(
        [chrome, f"--remote-debugging-port={port}", "--headless=new",
         "--remote-allow-origins=*",
         "--disable-gpu", "--no-sandbox", "--disable-dev-shm-usage", "about:blank"],
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
            return 2

        mismatches = 0
        print(f"{'page':<16} {'chromium(ok/c/p/f)':<22} {'falcon(ok/c/p/f)':<22} match")
        for pg in PAGES:
            url = args.fixtures + pg
            c = chromium_verdict(base_ws, url)
            f = falcon_verdict(args.falcon, url)
            same = (c["ok"] == f["ok"]
                    and c["console_error"] == f["console_error"]
                    and c["page_error"] == f["page_error"]
                    and c["failed_request"] == f["failed_request"])
            if not same:
                mismatches += 1

            def fmt(v):
                return f"{int(v['ok'])}/{int(v['console_error'])}/{int(v['page_error'])}/{int(v['failed_request'])}"
            print(f"{pg:<16} {fmt(c):<22} {fmt(f):<22} {'OK' if same else 'MISMATCH'}")

        print()
        if mismatches == 0:
            print("PARITY PASS: all pages agree on verdict and error categories")
            return 0
        print(f"PARITY FAIL: {mismatches} page(s) disagree")
        return 1
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
