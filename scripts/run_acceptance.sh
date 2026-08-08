#!/usr/bin/env bash
# falcon acceptance + integration suite (A1–A7). Starts the bundled fixture
# server and falcon, then runs every acceptance test and prints a summary.
# Requires: cargo (release build), python3 venv at .venv with websocket-client,
# and chrome-headless-shell available (PATH or ms-playwright cache) for A4/A5.
set -u
cd "$(dirname "$0")/.."
ROOT="$PWD"
PY="$ROOT/.venv/bin/python"
FX_PORT=8300
F_PORT=8200
FALCON="http://127.0.0.1:$F_PORT"
FIX="http://127.0.0.1:$FX_PORT"
PASS=0; FAIL=0
ok(){ echo "  PASS: $1"; PASS=$((PASS+1)); }
bad(){ echo "  FAIL: $1"; FAIL=$((FAIL+1)); }

echo "== building (release) =="
cargo build --release --quiet || { echo "build failed"; exit 1; }

# ---- start servers ----
pkill -9 -f 'target/release/fixtures' 2>/dev/null
pkill -9 -f 'target/release/falcon' 2>/dev/null
sleep 1
./target/release/fixtures 127.0.0.1:$FX_PORT >/tmp/fx.log 2>&1 &
FXPID=$!
./target/release/falcon --bind 127.0.0.1:$F_PORT --wall-cap-secs 60 --max-jobs 4 >/tmp/falcon.log 2>&1 &
FPID=$!
cleanup(){ kill -9 $FXPID $FPID 2>/dev/null; }
trap cleanup EXIT
sleep 2
[ "$(curl -s -m3 $FALCON/healthz)" = "ok" ] || { echo "falcon did not start"; exit 1; }

extract(){ curl -s -m 30 -X POST $FALCON/v1/extract -H 'content-type: application/json' -d "$1"; }
jget(){ $PY -c "import sys,json;d=json.load(sys.stdin);print(json.dumps(d.get('$1')))"; }

echo "== A1: static extract =="
R=$(extract "{\"url\":\"$FIX/\"}")
[ "$(echo "$R" | jget status)" = "200" ] && echo "$R" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if 'static-content-marker' in d['text'] and d['title']=='Static Home' else 1)" \
  && ok "A1 static text+title+status" || bad "A1 static"
echo "$R" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if any('/hugealloc' not in x['url'] and x['status']==200 for x in d['responses']) else 1)" \
  && ok "A1 responses lists document" || bad "A1 responses"

echo "== A2: JS execution + error attribution =="
for pg in jsdom:js-built-content-marker fetchrender:fetched-content-marker xhr:fetched-content-marker deferred:deferred-content-marker; do
  path=${pg%%:*}; marker=${pg##*:}
  echo "$(extract "{\"url\":\"$FIX/$path\"}")" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if '$marker' in d['text'] and not d['page_errors'] else 1)" \
    && ok "A2 $path renders JS content" || bad "A2 $path"
done
extract "{\"url\":\"$FIX/consoleerror\"}" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if any('boom' in c for c in d['console_errors']) else 1)" \
  && ok "A2 console.error captured" || bad "A2 console.error"
extract "{\"url\":\"$FIX/exception\"}" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if any('thrown-page-marker' in c for c in d['page_errors']) else 1)" \
  && ok "A2 uncaught exception captured" || bad "A2 exception"
extract "{\"url\":\"$FIX/badsub\"}" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if any('missing-image' in c for c in d['failed_requests']) else 1)" \
  && ok "A2 404 subresource in failed_requests" || bad "A2 badsub"

echo "== A3: tour with login =="
TOUR=$(curl -s -m 30 -X POST $FALCON/v1/tour -H 'content-type: application/json' -d "{
  \"base\":\"$FIX\",\"pages\":[\"/\",\"/private\"],
  \"login\":{\"path\":\"/login\",\"user\":\"alice\",\"pass\":\"secret\",\"user_sel\":\"[name=username]\",\"pass_sel\":\"[name=password]\",\"submit_sel\":\"#submit\"}}")
echo "$TOUR" | $PY -c "import sys,json;d=json.load(sys.stdin);p={x['page']:x for x in d['pages']};exit(0 if p['/private']['ok'] and 'private' in str(p['/private']) or (p['/private']['ok'] and p['/private']['status']==200) else 1)" \
  && ok "A3 login reaches authenticated /private" || bad "A3 login"

echo "== A4: parity vs Chromium =="
if $PY scripts/parity.py --falcon $FALCON --fixtures $FIX 2>/tmp/parity.log | tee /tmp/parity.out | grep -q "PARITY PASS"; then
  ok "A4 parity: falcon agrees with Chromium on all pages"
else
  tail -3 /tmp/parity.log; cat /tmp/parity.out 2>/dev/null | tail -12; bad "A4 parity"
fi

echo "== A5: resources vs Chromium =="
if $PY scripts/resources.py --falcon $FALCON --fixtures $FIX 2>/tmp/res.log | tee /tmp/res.out | grep -q "RESOURCE PASS"; then
  grep -E "falcon RSS|chromium|smaller" /tmp/res.out
  ok "A5 RSS<150MB, no leak, >=3x smaller than Chromium"
else
  tail -5 /tmp/res.log; cat /tmp/res.out 2>/dev/null | tail -8; bad "A5 resources"
fi

echo "== A6: containment + concurrency =="
# Launch two runaway jobs in the background; they must terminate <65s. Use a
# curl timeout (70s) longer than falcon's 60s wall cap so we capture the
# structured error response rather than a client-side timeout.
runaway(){ curl -s -m 70 -X POST $FALCON/v1/extract -H 'content-type: application/json' -d "$1"; }
( t0=$(date +%s); runaway "{\"url\":\"$FIX/infinite\"}" > /tmp/inf.json; echo $(( $(date +%s)-t0 )) > /tmp/inf.t ) &
INF=$!
( t0=$(date +%s); runaway "{\"url\":\"$FIX/hugealloc\"}" > /tmp/huge.json; echo $(( $(date +%s)-t0 )) > /tmp/huge.t ) &
HUGE=$!
sleep 2
# While they run, a normal tour must still complete quickly (workers not exhausted).
t0=$(date +%s)
CT=$(curl -s -m 20 -X POST $FALCON/v1/tour -H 'content-type: application/json' -d "{\"base\":\"$FIX\",\"pages\":[\"/\",\"/jsdom\"]}")
dt=$(( $(date +%s)-t0 ))
echo "$CT" | $PY -c "import sys,json;d=json.load(sys.stdin);exit(0 if d['summary']['failed']==0 else 1)" && [ $dt -lt 15 ] \
  && ok "A6 concurrent tour completes ($dt s) while 2 jobs are stuck" || bad "A6 concurrency"
wait $INF $HUGE
[ "$(curl -s -m3 $FALCON/healthz)" = "ok" ] && ok "A6 process survived runaway jobs" || bad "A6 survival"
IT=$(cat /tmp/inf.t 2>/dev/null||echo 999); HT=$(cat /tmp/huge.t 2>/dev/null||echo 999)
$PY -c "import json;d=json.load(open('/tmp/inf.json'));exit(0 if d['page_errors'] else 1)" && [ "$IT" -lt 65 ] \
  && ok "A6 infinite-loop terminated cleanly in ${IT}s (<65)" || bad "A6 infinite ($IT s)"
$PY -c "import json;d=json.load(open('/tmp/huge.json'));exit(0 if d['page_errors'] else 1)" && [ "$HT" -lt 65 ] \
  && ok "A6 huge-alloc terminated cleanly in ${HT}s (<65)" || bad "A6 hugealloc ($HT s)"

echo "== A7: real-world extracts (informational, not a gate) =="
for u in https://example.com/ https://www.rust-lang.org/ https://news.ycombinator.com/; do
  R=$(extract "{\"url\":\"$u\",\"block\":[\"image\",\"font\",\"media\"]}" 2>/dev/null)
  st=$(echo "$R" | jget status 2>/dev/null); tl=$(echo "$R" | $PY -c "import sys,json;print(len(json.load(sys.stdin).get('text','')))" 2>/dev/null||echo 0)
  echo "  [info] $u -> status=$st text_len=$tl"
done

echo
echo "==================== SUMMARY ===================="
echo "PASS=$PASS  FAIL=$FAIL"
[ $FAIL -eq 0 ] && { echo "ACCEPTANCE: ALL PASS"; exit 0; } || { echo "ACCEPTANCE: $FAIL FAILED"; exit 1; }
