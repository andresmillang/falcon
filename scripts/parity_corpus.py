#!/usr/bin/env python3
"""Chromium-parity corpus runner (R41/R42). Serves tests/corpus, runs every
fixture through Falcon and chrome-headless-shell (CDP), compares normalized
text / title / error categories / verdict, and classifies each as
PASS / KNOWN DIFFERENCE / FALCON BUG. Emits a summary and non-zero exit if any
FALCON BUG is found.

Usage: parity_corpus.py --falcon http://127.0.0.1:8200
(the runner serves the corpus itself and launches chromium)
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import HTTPServer, SimpleHTTPRequestHandler

import websocket

CORPUS = os.path.join(os.path.dirname(__file__), "..", "tests", "corpus")

# Fixtures where a Falcon/Chromium difference is expected and acceptable
# (documented, not a bug). name -> reason.
KNOWN_DIFFERENCES = {
    # R6 mandates that module fetch failures are reported in Falcon's page-error
    # report; Chromium instead surfaces a failed import as a network + console
    # error and not an uncaught runtime exception. Both engines agree the
    # dependent module does not run and both flag the failed request.
    "module_failimport": "R6: module fetch failure is a Falcon page_error; Chromium reports it as a network/console error",
}


def norm(s):
    return re.sub(r"\s+", " ", (s or "")).strip()


def marker(s):
    # Extract the uppercase MARKER token(s) both engines should agree on.
    toks = re.findall(r"[A-Z_]{3,}(?:=[^\s]*)?", s or "")
    return " ".join(toks)


# ---------- serve corpus ----------
class Quiet(SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def end_headers(self):
        if self.path.endswith(".js"):
            self.send_header("Content-Type", "text/javascript")
        super().end_headers()


def serve_corpus(port):
    os.chdir(CORPUS)
    httpd = HTTPServer(("127.0.0.1", port), Quiet)
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    return httpd


# ---------- chromium via CDP ----------
def find_chrome():
    p = subprocess.run(["which", "chrome-headless-shell"], capture_output=True, text=True)
    if p.returncode == 0:
        return p.stdout.strip()
    c = glob.glob(os.path.expanduser("~/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-linux64/chrome-headless-shell"))
    return sorted(c)[-1]


class Chrome:
    def __init__(self):
        self.port = 9455
        self.proc = subprocess.Popen(
            [find_chrome(), f"--remote-debugging-port={self.port}", "--headless=new",
             "--remote-allow-origins=*", "--disable-gpu", "--no-sandbox",
             "--disable-dev-shm-usage", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.base_ws = None
        for _ in range(50):
            try:
                v = json.loads(urllib.request.urlopen(f"http://localhost:{self.port}/json/version", timeout=1).read())
                self.base_ws = v["webSocketDebuggerUrl"]
                break
            except Exception:
                time.sleep(0.2)

    def verdict(self, url):
        ws = websocket.create_connection(self.base_ws, max_size=None)
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

        events = []

        def drain(sec):
            ws.settimeout(0.25)
            end = time.time() + sec
            while time.time() < end:
                try:
                    d = json.loads(ws.recv())
                    if "method" in d:
                        events.append(d)
                except Exception:
                    pass

        t = send("Target.createTarget", {"url": "about:blank"})["targetId"]
        s = send("Target.attachToTarget", {"targetId": t, "flatten": True})["sessionId"]
        for dom in ("Page", "Runtime", "Log", "Network"):
            send(f"{dom}.enable", {}, session=s)
        events.clear()
        send("Page.navigate", {"url": url}, session=s)
        drain(1.2)
        ce = pe = fr = False
        for ev in events:
            if ev.get("sessionId") != s:
                continue
            m = ev["method"]; p = ev.get("params", {})
            if m == "Runtime.consoleAPICalled" and p.get("type") == "error":
                ce = True
            if m == "Runtime.exceptionThrown":
                pe = True
            if m == "Log.entryAdded" and p.get("entry", {}).get("level") == "error":
                if p.get("entry", {}).get("source") == "network":
                    fr = True
                else:
                    ce = True
            if m == "Network.responseReceived" and p.get("response", {}).get("status", 0) >= 400:
                fr = True
            if m == "Network.loadingFailed":
                fr = True
        text = send("Runtime.evaluate", {"expression": "document.body?document.body.innerText:''", "returnByValue": True}, session=s).get("result", {}).get("value", "")
        title = send("Runtime.evaluate", {"expression": "document.title", "returnByValue": True}, session=s).get("result", {}).get("value", "")
        send("Target.closeTarget", {"targetId": t})
        ws.close()
        return {"marker": marker(text), "title": norm(title), "console_error": ce, "page_error": pe, "failed_request": fr}

    def close(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


def falcon_verdict(falcon, url):
    body = json.dumps({"url": url}).encode()
    req = urllib.request.Request(falcon + "/v1/extract", body, {"content-type": "application/json"})
    d = json.loads(urllib.request.urlopen(req, timeout=30).read())
    return {"marker": marker(d["text"]), "title": norm(d["title"]),
            "console_error": len(d["console_errors"]) > 0,
            "page_error": len(d["page_errors"]) > 0,
            "failed_request": len(d["failed_requests"]) > 0}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--falcon", default="http://127.0.0.1:8200")
    ap.add_argument("--port", type=int, default=8500)
    ap.add_argument("--report", default="")
    args = ap.parse_args()

    manifest = json.load(open(os.path.join(CORPUS, "manifest.json")))
    httpd = serve_corpus(args.port)
    base = f"http://127.0.0.1:{args.port}"
    chrome = Chrome()
    if not chrome.base_ws:
        print("chromium did not start", file=sys.stderr)
        return 2

    rows = []
    counts = {"PASS": 0, "KNOWN DIFFERENCE": 0, "FALCON BUG": 0}
    try:
        for m in manifest:
            url = f"{base}/{m['file']}"
            try:
                f = falcon_verdict(args.falcon, url)
                c = chrome.verdict(url)
            except Exception as e:
                rows.append((m["file"], m["category"], "FALCON BUG", f"error: {e}"))
                counts["FALCON BUG"] += 1
                continue
            same = (f["marker"] == c["marker"] and f["title"] == c["title"]
                    and f["console_error"] == c["console_error"]
                    and f["page_error"] == c["page_error"]
                    and f["failed_request"] == c["failed_request"])
            if same:
                verdict = "PASS"
                detail = f["marker"][:40]
            elif m["file"].replace(".html", "") in KNOWN_DIFFERENCES:
                verdict = "KNOWN DIFFERENCE"
                detail = KNOWN_DIFFERENCES[m["file"].replace(".html", "")]
            else:
                verdict = "FALCON BUG"
                detail = f"falcon={f}  chromium={c}"
            counts[verdict] += 1
            rows.append((m["file"], m["category"], verdict, detail))
    finally:
        chrome.close()
        httpd.shutdown()

    for file, cat, verdict, detail in rows:
        tag = {"PASS": "  ", "KNOWN DIFFERENCE": "~ ", "FALCON BUG": "XX"}[verdict]
        if verdict != "PASS":
            print(f"{tag} [{verdict}] {file} ({cat}): {detail}")
    print()
    print(f"CORPUS: {len(rows)} fixtures | PASS={counts['PASS']} "
          f"KNOWN_DIFFERENCE={counts['KNOWN DIFFERENCE']} FALCON_BUG={counts['FALCON BUG']}")
    if args.report:
        json.dump({"counts": counts, "rows": [{"file": r[0], "category": r[1], "verdict": r[2]} for r in rows]},
                  open(args.report, "w"), indent=1)
    return 0 if counts["FALCON BUG"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
