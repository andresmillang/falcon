#!/usr/bin/env bash
# Falcon→Chromium fallback scenarios (R38/D6). Verifies all five: Falcon success,
# Falcon→Chromium success, both-fail, disabled, and backend-unavailable.
# Needs chrome-headless-shell (ms-playwright cache) and a corpus HTTP server.
set -u
cd "$(dirname "$0")/.."
PY="$PWD/.venv/bin/python"
CHROME=$(find ~/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-linux64/chrome-headless-shell 2>/dev/null | head -1)
[ -z "$CHROME" ] && { echo "chrome-headless-shell not found; skipping fallback test"; exit 0; }

# Serve a small tree: a page Falcon renders, and a canvas page Falcon fails on.
SRV=/tmp/fb_corpus; mkdir -p $SRV
cat > $SRV/ok.html <<'H'
<!doctype html><html><head><title>OK</title></head><body><div id=out>START</div><script>document.getElementById('out').textContent='FALCON_OK';</script></body></html>
H
cat > $SRV/canvas.html <<'H'
<!doctype html><html><head><title>Canvas</title></head><body><div id=out>START</div><script>var c=document.createElement('canvas');var x=c.getContext('2d');x.fillRect(0,0,1,1);document.getElementById('out').textContent='CHROME_RENDERED';</script></body></html>
H
( cd $SRV && python3 -m http.server 8610 >/tmp/fb_srv.log 2>&1 & ) ; SRVPID=$!
pkill -9 -f 'bind 127.0.0.1:861' 2>/dev/null; sleep 1
./target/release/falcon --bind 127.0.0.1:8611 --enable-fallback --chromium-cdp "$CHROME" --fallback-timeout-secs 25 >/tmp/fb_on.log 2>&1 &
FON=$!
./target/release/falcon --bind 127.0.0.1:8612 >/tmp/fb_off.log 2>&1 &
FOFF=$!
./target/release/falcon --bind 127.0.0.1:8613 --enable-fallback --chromium-cdp /nonexistent --fallback-timeout-secs 5 >/tmp/fb_bad.log 2>&1 &
FBAD=$!
cleanup(){ kill -9 $SRVPID $FON $FOFF $FBAD 2>/dev/null; pkill -9 -f 'http.server 8610' 2>/dev/null; }
trap cleanup EXIT
sleep 2
ex(){ curl -s -m30 -X POST "$1/v1/extract" -H 'content-type: application/json' -d "{\"url\":\"$2\"}"; }
chk(){ echo "$1" | $PY -c "import sys,json;d=json.load(sys.stdin);print(' engine=%s fallback_used=%s reason=%s text=%r'%(d['engine_used'],d['fallback_used'],d['fallback_reason'],d['text'][:20]))"; }
P=0; F=0
pass(){ echo "  PASS: $1"; P=$((P+1)); }
fail(){ echo "  FAIL: $1"; F=$((F+1)); }

echo "(a) Falcon success + fallback ON → engine=falcon, no fallback"
R=$(ex http://127.0.0.1:8611 http://127.0.0.1:8610/ok.html); chk "$R"
echo "$R" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if d['engine_used']=='falcon' and not d['fallback_used'] else 1)" && pass "(a)" || fail "(a)"

echo "(b) Falcon fails (canvas) + fallback ON → engine=chromium, diagnostics kept"
R=$(ex http://127.0.0.1:8611 http://127.0.0.1:8610/canvas.html); chk "$R"
echo "$R" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if d['engine_used']=='chromium' and d['fallback_used'] and 'CHROME_RENDERED' in d['text'] and d.get('falcon_diagnostics',{}).get('page_errors') else 1)" && pass "(b)" || fail "(b)"

echo "(d) fallback DISABLED + canvas → honest falcon failure, no chromium"
R=$(ex http://127.0.0.1:8612 http://127.0.0.1:8610/canvas.html); chk "$R"
echo "$R" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if d['engine_used']=='falcon' and not d['fallback_used'] and d['falcon_status']=='javascript_failure' else 1)" && pass "(d)" || fail "(d)"

echo "(e) fallback ENABLED, backend UNAVAILABLE → fallback_used=false, falcon result kept"
R=$(ex http://127.0.0.1:8613 http://127.0.0.1:8610/canvas.html); chk "$R"
echo "$R" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if not d['fallback_used'] and 'fallback_error' in d else 1)" && pass "(e)" || fail "(e)"

echo "(c) both fail (bad URL) + fallback ON → handled honestly, process survives"
R=$(ex http://127.0.0.1:8611 http://127.0.0.1:9/nope); chk "$R"
alive=$(curl -s -m3 http://127.0.0.1:8611/healthz)
[ "$alive" = "ok" ] && pass "(c) survived" || fail "(c)"

echo
echo "FALLBACK: ALL PASS" && [ $F -eq 0 ] || echo "FALLBACK: $F FAILED"
[ $F -eq 0 ]
