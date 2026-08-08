#!/usr/bin/env python3
"""Generate the deterministic Chromium-parity corpus (R41). Writes self-checking
fixtures to tests/corpus/ plus a manifest.json. Each fixture computes a result
into <div id=out>; the parity runner compares Falcon vs Chromium on the
normalized text/title/verdict. Structured so adding fixtures is trivial.
"""
import json
import os

ROOT = os.path.join(os.path.dirname(__file__), "..", "tests", "corpus")
os.makedirs(ROOT, exist_ok=True)

FIXTURES = {}          # name.html -> html string
SUPPORT = {}           # name.js/json -> content
MANIFEST = []          # {file, category, note}


def page(body, title="Fx"):
    return f"<!doctype html><html><head><title>{title}</title></head><body>{body}</body></html>"


def add(name, category, body, title="Fx", note=""):
    FIXTURES[name + ".html"] = page(body, title)
    MANIFEST.append({"file": name + ".html", "category": category, "note": note})


def out(js):
    return f'<div id=out>START</div><script>{js}</script>'


def out_async(js):
    # js must call done(value)
    return ('<div id=out>START</div><script>'
            'function done(v){document.getElementById("out").textContent=v;}'
            + js + '</script>')


# ---- HTML parsing edge cases ----
add("html_implied_tags", "html-parsing",
    "<table><tr><td>cell</td></tr></table>" + out("done(document.querySelectorAll('tbody').length>0?'IMPLIED_TBODY':'NO')".replace("done", "var o=document.getElementById('out');o.textContent=")))
add("html_unclosed", "html-parsing",
    "<p>one<p>two<p>three" + out("var o=document.getElementById('out');o.textContent='PS='+document.querySelectorAll('p').length;"))
add("html_entities", "html-parsing",
    "<div id=e>a&amp;b&lt;c&gt;d</div>" + out("var o=document.getElementById('out');o.textContent='ENT='+document.getElementById('e').textContent;"))
add("html_comment", "html-parsing",
    "<!-- c --><div id=x>ok</div>" + out("var o=document.getElementById('out');o.textContent='CMT='+document.getElementById('x').textContent;"))
add("html_attributes", "html-parsing",
    "<input id=i type=text value='v1' data-k='dk' disabled>" + out("var i=document.getElementById('i');var o=document.getElementById('out');o.textContent='ATTR='+i.value+i.dataset.k+i.hasAttribute('disabled');"))

# ---- DOM manipulation ----
add("dom_create_append", "dom",
    out("var d=document.createElement('b');d.textContent='X';document.body.appendChild(d);done('CREATE='+document.querySelectorAll('b').length)".replace("done", "document.getElementById('out').textContent=")))
add("dom_remove", "dom",
    "<div id=a><span id=s>x</span></div>" + out("document.getElementById('s').remove();document.getElementById('out').textContent='REMOVE='+document.querySelectorAll('span').length;"))
add("dom_replace", "dom",
    "<div id=a>old</div>" + out("var n=document.createElement('em');n.textContent='new';document.getElementById('a').replaceWith(n);document.getElementById('out').textContent='REPLACE='+document.querySelector('em').textContent;"))
add("dom_innerhtml", "dom",
    "<div id=h></div>" + out("document.getElementById('h').innerHTML='<i>a</i><i>b</i>';document.getElementById('out').textContent='IH='+document.querySelectorAll('#h i').length;"))
add("dom_insertadjacent", "dom",
    "<div id=h>mid</div>" + out("document.getElementById('h').insertAdjacentHTML('afterbegin','<u>pre</u>');document.getElementById('out').textContent='IA='+(document.querySelector('u')!==null);"))
add("dom_classlist", "dom",
    "<div id=c class='a b'></div>" + out("var c=document.getElementById('c');c.classList.add('z');c.classList.remove('a');document.getElementById('out').textContent='CL='+c.className;"))
add("dom_dataset", "dom",
    "<div id=d data-foo-bar='v'></div>" + out("document.getElementById('out').textContent='DS='+document.getElementById('d').dataset.fooBar;"))
add("dom_clone", "dom",
    "<ul id=l><li>1</li><li>2</li></ul>" + out("var c=document.getElementById('l').cloneNode(true);document.getElementById('out').textContent='CLONE='+c.children.length;"))
add("dom_traversal", "dom",
    "<div id=p><a>1</a><b>2</b><c>3</c></div>" + out("var a=document.querySelector('a');document.getElementById('out').textContent='TRAV='+a.nextElementSibling.tagName+a.parentElement.childElementCount;"))
add("dom_textcontent", "dom",
    "<div id=t>a<span>b</span>c</div>" + out("document.getElementById('out').textContent='TC='+document.getElementById('t').textContent;"))

# ---- Events ----
add("evt_click_bubble", "events",
    "<div id=p><button id=b>x</button></div>" + out("var n=0;document.getElementById('p').addEventListener('click',function(){n++;});document.getElementById('b').click();document.getElementById('out').textContent='BUBBLE='+n;"))
add("evt_custom_detail", "events",
    out("var v=0;document.addEventListener('x',function(e){v=e.detail.n;});document.dispatchEvent(new CustomEvent('x',{detail:{n:42}}));document.getElementById('out').textContent='DETAIL='+v;"))
add("evt_once", "events",
    out("var n=0;var el=document.createElement('a');el.addEventListener('click',function(){n++;},{once:true});el.click();el.click();document.getElementById('out').textContent='ONCE='+n;"))
add("evt_stopprop", "events",
    "<div id=p><span id=s>x</span></div>" + out("var n=0;document.getElementById('p').addEventListener('click',function(){n++;});document.getElementById('s').addEventListener('click',function(e){e.stopPropagation();});document.getElementById('s').dispatchEvent(new Event('click',{bubbles:true}));document.getElementById('out').textContent='STOP='+n;"))
add("evt_preventdefault", "events",
    out("var e=new Event('submit',{cancelable:true});e.preventDefault();document.getElementById('out').textContent='PD='+e.defaultPrevented;"))

# ---- Timers ----
add("timer_settimeout", "timers",
    out_async("setTimeout(function(){done('TIMEOUT_OK');},10);"))
add("timer_interval_clear", "timers",
    out_async("var n=0;var id=setInterval(function(){n++;if(n>=3){clearInterval(id);done('INTERVAL='+n);}},5);"))
add("timer_order", "timers",
    out_async("var s='';setTimeout(function(){s+='B';done('ORDER='+s);},20);setTimeout(function(){s+='A';},10);"))
add("timer_raf", "timers",
    out_async("requestAnimationFrame(function(){done('RAF_OK');});"))

# ---- Promises / microtasks ----
add("promise_then", "promises",
    out_async("Promise.resolve(7).then(function(v){done('PROMISE='+v);});"))
add("promise_chain", "promises",
    out_async("Promise.resolve(1).then(v=>v+1).then(v=>v*3).then(function(v){done('CHAIN='+v);});"))
add("promise_all", "promises",
    out_async("Promise.all([Promise.resolve('a'),Promise.resolve('b')]).then(function(a){done('ALL='+a.join(''));});"))
add("microtask_order", "promises",
    out_async("var s='';setTimeout(function(){s+='M';done('MO='+s);},5);queueMicrotask(function(){s+='u';});"))
add("promise_catch", "promises",
    out_async("Promise.reject(new Error('e')).catch(function(){done('CATCH_OK');});"))
add("async_await", "promises",
    out_async("(async function(){var v=await Promise.resolve('AW_OK');done(v);})();"))

# ---- fetch ----
add("fetch_json", "fetch",
    out_async("fetch('/data.json').then(r=>r.json()).then(function(j){done('FETCH_JSON='+j.k);});"))
add("fetch_text", "fetch",
    out_async("fetch('/hello.txt').then(r=>r.text()).then(function(t){done('FETCH_TEXT='+t.trim());});"))
add("fetch_status", "fetch",
    out_async("fetch('/data.json').then(function(r){done('FETCH_STATUS='+r.status+r.ok);});"))
add("fetch_404", "fetch",
    out_async("fetch('/nope-404').then(function(r){done('FETCH_404='+r.status);});"))

# ---- XHR ----
add("xhr_get", "xhr",
    out_async("var x=new XMLHttpRequest();x.open('GET','/data.json');x.onload=function(){done('XHR='+JSON.parse(x.responseText).k);};x.send();"))
add("xhr_status", "xhr",
    out_async("var x=new XMLHttpRequest();x.open('GET','/hello.txt');x.onload=function(){done('XHR_STATUS='+x.status);};x.send();"))

# ---- storage ----
add("storage_local", "storage",
    out("localStorage.setItem('k','v1');localStorage.setItem('n',5);document.getElementById('out').textContent='LS='+localStorage.getItem('k')+localStorage.getItem('n')+localStorage.length;"))
add("storage_session", "storage",
    out("sessionStorage.setItem('s','x');sessionStorage.removeItem('s');document.getElementById('out').textContent='SS='+sessionStorage.length;"))

# ---- history / location ----
add("history_pushstate", "history",
    out("history.pushState({a:1},'','/new/path?q=2');document.getElementById('out').textContent='HIST='+location.pathname+location.search;"))
add("location_parse", "history",
    out("var u=new URL('https://h.com/a/b?x=1#f');document.getElementById('out').textContent='URL='+u.hostname+u.pathname+u.hash;"))
add("urlsearchparams", "history",
    out("var p=new URLSearchParams('a=1&b=2&a=3');document.getElementById('out').textContent='USP='+p.get('a')+p.getAll('a').length+p.has('b');"))

# ---- forms ----
add("form_elements", "forms",
    "<form id=f><input name=a value=1><input name=b value=2><textarea name=c>3</textarea></form>" + out("document.getElementById('out').textContent='FORM='+document.getElementById('f').elements.length;"))
add("form_submit_event", "forms",
    "<form id=f><input name=a></form>" + out("var s=0;document.getElementById('f').addEventListener('submit',function(e){e.preventDefault();s++;});document.getElementById('f').requestSubmit();document.getElementById('out').textContent='SUBMIT='+s;"))

# ---- MutationObserver ----
add("mutation_childlist", "mutationobserver",
    "<div id=t></div>" + out_async("var mo=new MutationObserver(function(l){done('MUT='+l[0].type+l[0].addedNodes.length);});mo.observe(document.getElementById('t'),{childList:true});var c=document.createElement('span');document.getElementById('t').appendChild(c);"))
add("mutation_attributes", "mutationobserver",
    "<div id=t></div>" + out_async("var mo=new MutationObserver(function(l){done('MUTATTR='+l[0].attributeName);});mo.observe(document.getElementById('t'),{attributes:true});document.getElementById('t').setAttribute('data-x','1');"))

# ---- malformed JavaScript ----
add("malformed_syntax", "malformed-js",
    "<div id=out>NOSCRIPT_RAN</div><script>this is not valid javascript )(</script>")
add("malformed_then_valid", "malformed-js",
    "<div id=out>START</div><script>var x=;</script><script>document.getElementById('out').textContent='RECOVERED';</script>")

# ---- uncaught exceptions ----
add("exception_throw", "exceptions",
    "<div id=out>BEFORE</div><script>throw new Error('boom');</script>")
add("exception_in_timer", "exceptions",
    out_async("setTimeout(function(){done('AFTER_TIMER');},5);setTimeout(function(){throw new Error('late');},1);"))

# ---- failed network requests ----
add("failed_subresource", "failed-network",
    "<img src=/missing.png><div id=out>IMG_PAGE</div>")
add("failed_fetch_caught", "failed-network",
    out_async("fetch('http://127.0.0.1:9/x').then(()=>done('NO')).catch(function(){done('FETCH_ERR_CAUGHT');});"))

# ---- dynamic content ----
add("dynamic_render", "dynamic",
    "<div id=out>loading</div><script>setTimeout(function(){var d=document.createElement('p');d.textContent='DYNAMIC_CONTENT';document.body.appendChild(d);document.getElementById('out').textContent='DYNAMIC_CONTENT';},15);</script>")
add("dynamic_fetch_render", "dynamic",
    out_async("fetch('/data.json').then(r=>r.json()).then(function(j){var d=document.createElement('p');d.textContent=j.k;document.body.appendChild(d);done('DFR='+j.k);});"))

# ---- modules ----
SUPPORT["m_c.js"] = "export const c='C';"
SUPPORT["m_b.js"] = "import {c} from './m_c.js'; export const b='B'+c;"
SUPPORT["m_d1.js"] = "export const x=10;"
SUPPORT["m_d2.js"] = "export const y=20;"
SUPPORT["m_dyn.js"] = "export const msg='DYNMOD';"
add("module_single", "modules",
    '<div id=out>START</div><script type="module">document.getElementById("out").textContent="MODULE_OK";</script>')
add("module_nested", "modules",
    '<div id=out>START</div><script type="module">import {b} from "./m_b.js";document.getElementById("out").textContent="NEST="+b;</script>')
add("module_multi", "modules",
    '<div id=out>START</div><script type="module">import {x} from "./m_d1.js";import {y} from "./m_d2.js";document.getElementById("out").textContent="MM="+(x+y);</script>')
add("module_dynamic", "modules",
    '<div id=out>START</div><script type="module">import("./m_dyn.js").then(function(m){document.getElementById("out").textContent=m.msg;});</script>')
add("module_failimport", "modules",
    '<div id=out>NO_DEP</div><script type="module">import {z} from "./missing-dep.js";document.getElementById("out").textContent="SHOULD_NOT_RUN";</script>',
    note="dependent must not run; error reported")

# ---- async dependency chains ----
add("async_chain", "async-chains",
    out_async("fetch('/data.json').then(r=>r.json()).then(function(j){return fetch('/hello.txt');}).then(r=>r.text()).then(function(t){done('ASYNC_CHAIN='+t.trim());});"))
add("async_parallel", "async-chains",
    out_async("Promise.all([fetch('/data.json').then(r=>r.json()),fetch('/hello.txt').then(r=>r.text())]).then(function(a){done('PARALLEL='+a[0].k+a[1].trim());});"))

# ---- resource-loading failures ----
add("resource_fail_reported", "resource-fail",
    "<script src=/missing-script.js></script><div id=out>MAIN_OK</div>")

# ---- selectors (stronger CSS) ----
add("sel_attr_ops", "dom",
    "<a data-k='hello'>1</a><a data-k='world'>2</a>" + out("document.getElementById('out').textContent='SEL='+document.querySelectorAll('[data-k^=hel]').length+document.querySelectorAll('[data-k*=orl]').length;"))
add("sel_not", "dom",
    "<ul><li class=x>1</li><li>2</li><li class=x>3</li></ul>" + out("document.getElementById('out').textContent='NOT='+document.querySelectorAll('li:not(.x)').length;"))
add("sel_combinators", "dom",
    "<div class=a><b>1</b></div><b>2</b>" + out("document.getElementById('out').textContent='COMB='+document.querySelectorAll('.a > b').length;"))


def main():
    for name, content in {**FIXTURES, **SUPPORT}.items():
        with open(os.path.join(ROOT, name), "w") as f:
            f.write(content)
    with open(os.path.join(ROOT, "manifest.json"), "w") as f:
        json.dump(MANIFEST, f, indent=1)
    # supporting data files referenced by fetch fixtures
    with open(os.path.join(ROOT, "data.json"), "w") as f:
        f.write('{"k":"kv"}')
    with open(os.path.join(ROOT, "hello.txt"), "w") as f:
        f.write("hellotext")
    print(f"wrote {len(FIXTURES)} fixtures + {len(SUPPORT)} module files to {ROOT}")
    cats = {}
    for m in MANIFEST:
        cats[m["category"]] = cats.get(m["category"], 0) + 1
    print("categories:", json.dumps(cats))


if __name__ == "__main__":
    main()
