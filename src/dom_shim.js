// falcon in-V8 DOM shim. Runs inside the isolate. Host functions provided by
// Rust: __parse_html_fragment(html)->json, __enqueue_request(...), __set_timer,
// __clear_timer, __console(level,msg), __page_error(msg). Rust calls back:
// __resolve_request(...), __fire_timer(id), __abort_request(id). Read-back:
// __collect(), __node_count(). Modules are compiled/evaluated by Rust against
// this same context, so they see window/document.
(function () {
  "use strict";
  const g = globalThis;

  function reportPageError(e) {
    let msg;
    try { msg = (e && e.stack) ? String(e.stack) : String(e); } catch (_) { msg = "unknown error"; }
    __page_error(msg);
    try { window.dispatchEvent(new FalconEvent("error", { message: msg })); } catch (_) {}
  }

  g.console = {
    log: function () { __console("log", fmt(arguments)); },
    info: function () { __console("info", fmt(arguments)); },
    warn: function () { __console("warn", fmt(arguments)); },
    error: function () { __console("error", fmt(arguments)); },
    debug: function () { __console("debug", fmt(arguments)); },
    trace: function () {}, group: function () {}, groupEnd: function () {}, table: function () {},
    assert: function (c) { if (!c) __console("error", "assertion failed"); },
  };
  function fmt(args) {
    const out = [];
    for (let i = 0; i < args.length; i++) {
      const a = args[i];
      try { out.push(typeof a === "string" ? a : JSON.stringify(a)); } catch (_) { out.push(String(a)); }
    }
    return out.join(" ");
  }

  // ---- Events (R16) ----
  function FalconEvent(type, init) {
    init = init || {};
    this.type = type;
    this.bubbles = !!init.bubbles;
    this.cancelable = !!init.cancelable;
    this.composed = !!init.composed;
    this.defaultPrevented = false;
    this.detail = init.detail !== undefined ? init.detail : null;
    this.target = null;
    this.currentTarget = null;
    this.eventPhase = 0;
    this.timeStamp = 0;
    this._stop = false;
    this._stopImmediate = false;
    for (const k in init) if (!(k in this)) this[k] = init[k];
  }
  FalconEvent.prototype.preventDefault = function () { if (this.cancelable) this.defaultPrevented = true; };
  FalconEvent.prototype.stopPropagation = function () { this._stop = true; };
  FalconEvent.prototype.stopImmediatePropagation = function () { this._stop = true; this._stopImmediate = true; };
  g.Event = FalconEvent;
  g.CustomEvent = FalconEvent;

  const ELEMENT_NODE = 1, TEXT_NODE = 3, COMMENT_NODE = 8, DOCUMENT_FRAGMENT_NODE = 11, DOCUMENT_NODE = 9;

  // ---- MutationObserver plumbing (R9) ----
  const _observers = [];
  let _mutationScheduled = false;
  function scheduleMutations() {
    if (_mutationScheduled) return;
    _mutationScheduled = true;
    Promise.resolve().then(function () {
      _mutationScheduled = false;
      for (const ob of _observers.slice()) {
        if (ob._queue.length) {
          const records = ob._queue; ob._queue = [];
          try { ob._cb(records, ob); } catch (e) { reportPageError(e); }
        }
      }
    });
  }
  function recordMutation(type, target, extra) {
    if (!_observers.length) return;
    for (const ob of _observers) {
      for (const cfg of ob._targets) {
        const match = cfg.node === target || (cfg.subtree && cfg.node.contains && cfg.node.contains(target));
        if (!match) continue;
        if (type === "childList" && !cfg.childList) continue;
        if (type === "attributes" && !cfg.attributes) continue;
        if (type === "characterData" && !cfg.characterData) continue;
        const rec = {
          type: type, target: target,
          addedNodes: extra.addedNodes || [], removedNodes: extra.removedNodes || [],
          attributeName: extra.attributeName || null, oldValue: extra.oldValue !== undefined ? extra.oldValue : null,
          previousSibling: null, nextSibling: null,
        };
        ob._queue.push(rec);
        scheduleMutations();
        break;
      }
    }
  }
  function MutationObserver(cb) { this._cb = cb; this._targets = []; this._queue = []; }
  MutationObserver.prototype.observe = function (node, opts) {
    opts = opts || {};
    this._targets.push({
      node: node, subtree: !!opts.subtree,
      childList: !!opts.childList, attributes: !!opts.attributes, characterData: !!opts.characterData,
    });
    if (_observers.indexOf(this) < 0) _observers.push(this);
  };
  MutationObserver.prototype.disconnect = function () {
    this._targets = []; this._queue = [];
    const i = _observers.indexOf(this); if (i >= 0) _observers.splice(i, 1);
  };
  MutationObserver.prototype.takeRecords = function () { const q = this._queue; this._queue = []; return q; };
  g.MutationObserver = MutationObserver;

  // ---- Node / Element ----
  let _nodeCount = 0;
  function Node(nodeType) {
    this.nodeType = nodeType;
    this.childNodes = [];
    this.parentNode = null;
    this._listeners = Object.create(null);
    _nodeCount++;
  }
  Object.defineProperty(Node.prototype, "children", {
    get() { return this.childNodes.filter((n) => n.nodeType === ELEMENT_NODE); },
  });
  Object.defineProperty(Node.prototype, "childElementCount", { get() { return this.children.length; } });
  Object.defineProperty(Node.prototype, "firstChild", { get() { return this.childNodes[0] || null; } });
  Object.defineProperty(Node.prototype, "lastChild", { get() { return this.childNodes[this.childNodes.length - 1] || null; } });
  Object.defineProperty(Node.prototype, "firstElementChild", { get() { return this.children[0] || null; } });
  Object.defineProperty(Node.prototype, "lastElementChild", { get() { const c = this.children; return c[c.length - 1] || null; } });
  Object.defineProperty(Node.prototype, "parentElement", {
    get() { return this.parentNode && this.parentNode.nodeType === ELEMENT_NODE ? this.parentNode : null; },
  });
  Object.defineProperty(Node.prototype, "nextSibling", {
    get() { if (!this.parentNode) return null; const s = this.parentNode.childNodes; const i = s.indexOf(this); return i >= 0 ? (s[i + 1] || null) : null; },
  });
  Object.defineProperty(Node.prototype, "previousSibling", {
    get() { if (!this.parentNode) return null; const s = this.parentNode.childNodes; const i = s.indexOf(this); return i > 0 ? s[i - 1] : null; },
  });
  Object.defineProperty(Node.prototype, "nextElementSibling", {
    get() { let n = this.nextSibling; while (n && n.nodeType !== ELEMENT_NODE) n = n.nextSibling; return n; },
  });
  Object.defineProperty(Node.prototype, "previousElementSibling", {
    get() { let n = this.previousSibling; while (n && n.nodeType !== ELEMENT_NODE) n = n.previousSibling; return n; },
  });
  Node.prototype.hasChildNodes = function () { return this.childNodes.length > 0; };
  Node.prototype.appendChild = function (child) {
    const added = child.nodeType === DOCUMENT_FRAGMENT_NODE ? child.childNodes.slice() : [child];
    if (child.nodeType === DOCUMENT_FRAGMENT_NODE) {
      for (const c of added) { if (c.parentNode) c.parentNode.removeChild(c); c.parentNode = this; this.childNodes.push(c); }
      child.childNodes = [];
    } else {
      if (child.parentNode) child.parentNode.removeChild(child);
      child.parentNode = this; this.childNodes.push(child);
    }
    recordMutation("childList", this, { addedNodes: added });
    return child;
  };
  Node.prototype.removeChild = function (child) {
    const i = this.childNodes.indexOf(child);
    if (i >= 0) { this.childNodes.splice(i, 1); child.parentNode = null; recordMutation("childList", this, { removedNodes: [child] }); }
    return child;
  };
  Node.prototype.insertBefore = function (child, ref) {
    if (!ref) return this.appendChild(child);
    if (child.parentNode) child.parentNode.removeChild(child);
    const i = this.childNodes.indexOf(ref);
    child.parentNode = this;
    if (i < 0) this.childNodes.push(child); else this.childNodes.splice(i, 0, child);
    recordMutation("childList", this, { addedNodes: [child] });
    return child;
  };
  Node.prototype.replaceChild = function (nw, old) {
    const i = this.childNodes.indexOf(old);
    if (i < 0) return old;
    if (nw.parentNode) nw.parentNode.removeChild(nw);
    nw.parentNode = this; this.childNodes[i] = nw; old.parentNode = null;
    recordMutation("childList", this, { addedNodes: [nw], removedNodes: [old] });
    return old;
  };
  Node.prototype.remove = function () { if (this.parentNode) this.parentNode.removeChild(this); };
  Node.prototype.contains = function (n) { while (n) { if (n === this) return true; n = n.parentNode; } return false; };
  Node.prototype.cloneNode = function (deep) {
    let copy;
    if (this.nodeType === TEXT_NODE) copy = new TextNode(this._text);
    else if (this.nodeType === COMMENT_NODE) copy = new CommentNode(this._text);
    else if (this.nodeType === ELEMENT_NODE) {
      copy = new Element(this.localName);
      for (const k in this._attrs) copy.setAttribute(k, this._attrs[k]);
    } else return new Node(this.nodeType);
    if (deep && this.childNodes) for (const c of this.childNodes) copy.appendChild(c.cloneNode(true));
    return copy;
  };
  Node.prototype.addEventListener = function (type, fn, opts) {
    if (!fn) return;
    const o = typeof opts === "object" && opts ? opts : { capture: !!opts };
    (this._listeners[type] || (this._listeners[type] = [])).push({ fn: fn, once: !!o.once, capture: !!o.capture });
  };
  Node.prototype.removeEventListener = function (type, fn) {
    const a = this._listeners[type]; if (!a) return;
    for (let i = a.length - 1; i >= 0; i--) if (a[i].fn === fn) a.splice(i, 1);
  };
  Node.prototype.dispatchEvent = function (ev) {
    ev.target = this;
    const chain = []; let node = this;
    while (node) { chain.push(node); node = node.parentNode; }
    // capture phase (root -> target)
    for (let i = chain.length - 1; i >= 0 && !ev._stop; i--) runListeners(chain[i], ev, true);
    // target + bubble
    for (let i = 0; i < chain.length && !ev._stop; i++) {
      runListeners(chain[i], ev, false);
      if (!ev.bubbles) break;
    }
    return !ev.defaultPrevented;
  };
  function runListeners(cur, ev, capture) {
    ev.currentTarget = cur;
    const ls = cur._listeners[ev.type];
    if (ls) {
      for (const l of ls.slice()) {
        if (!!l.capture !== capture) continue;
        try { l.fn.call(cur, ev); } catch (e) { reportPageError(e); }
        if (l.once) cur.removeEventListener(ev.type, l.fn);
        if (ev._stopImmediate) return;
      }
    }
    if (!capture) {
      const on = cur["on" + ev.type];
      if (typeof on === "function") { try { on.call(cur, ev); } catch (e) { reportPageError(e); } }
    }
  }

  function TextNode(text) { Node.call(this, TEXT_NODE); this._text = text; }
  TextNode.prototype = Object.create(Node.prototype);
  ["textContent", "nodeValue", "data"].forEach(function (p) {
    Object.defineProperty(TextNode.prototype, p, {
      get() { return this._text; },
      set(v) { const old = this._text; this._text = String(v); recordMutation("characterData", this, { oldValue: old }); },
    });
  });
  Object.defineProperty(TextNode.prototype, "length", { get() { return this._text.length; } });

  function CommentNode(text) { Node.call(this, COMMENT_NODE); this._text = text; }
  CommentNode.prototype = Object.create(Node.prototype);
  Object.defineProperty(CommentNode.prototype, "textContent", { get() { return this._text; }, set(v) { this._text = String(v); } });

  function DocumentFragment() { Node.call(this, DOCUMENT_FRAGMENT_NODE); }
  DocumentFragment.prototype = Object.create(Node.prototype);
  DocumentFragment.prototype.querySelector = function (s) { return querySel(this, s, false)[0] || null; };
  DocumentFragment.prototype.querySelectorAll = function (s) { return querySel(this, s, true); };
  g.DocumentFragment = DocumentFragment;

  function classListFor(el) {
    return {
      add() { for (const c of arguments) if (!el._classes.includes(c)) el._classes.push(c); syncClass(el); },
      remove() { for (const c of arguments) { const i = el._classes.indexOf(c); if (i >= 0) el._classes.splice(i, 1); } syncClass(el); },
      toggle(c, force) {
        const has = el._classes.includes(c);
        if (force === true || (force === undefined && !has)) { if (!has) el._classes.push(c); }
        else { const i = el._classes.indexOf(c); if (i >= 0) el._classes.splice(i, 1); }
        syncClass(el); return el._classes.includes(c);
      },
      contains(c) { return el._classes.includes(c); },
      replace(a, b) { const i = el._classes.indexOf(a); if (i >= 0) { el._classes[i] = b; syncClass(el); return true; } return false; },
      get length() { return el._classes.length; },
      item(i) { return el._classes[i] || null; },
      toString() { return el._classes.join(" "); },
    };
  }
  function syncClass(el) { el._attrs["class"] = el._classes.join(" "); }

  function Element(tag) {
    Node.call(this, ELEMENT_NODE);
    this.tagName = tag.toUpperCase();
    this.localName = tag.toLowerCase();
    this._attrs = Object.create(null);
    this._classes = [];
    this.style = makeStyle();
    this._value = undefined;
  }
  function makeStyle() {
    const s = {}; s.setProperty = function (k, v) { s[k] = v; }; s.getPropertyValue = function (k) { return s[k] || ""; };
    s.removeProperty = function (k) { delete s[k]; }; return s;
  }
  Element.prototype = Object.create(Node.prototype);
  Object.defineProperty(Element.prototype, "nodeName", { get() { return this.tagName; } });
  Object.defineProperty(Element.prototype, "id", { get() { return this._attrs["id"] || ""; }, set(v) { this.setAttribute("id", v); } });
  Object.defineProperty(Element.prototype, "className", {
    get() { return this._classes.join(" "); },
    set(v) { this._classes = String(v).split(/\s+/).filter(Boolean); syncClass(this); },
  });
  Object.defineProperty(Element.prototype, "classList", { get() { return classListFor(this); } });
  Object.defineProperty(Element.prototype, "dataset", {
    get() {
      const el = this; const d = {};
      for (const k in el._attrs) if (k.indexOf("data-") === 0) {
        const camel = k.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase()); d[camel] = el._attrs[k];
      }
      return d;
    },
  });
  Element.prototype.getAttribute = function (n) { n = n.toLowerCase(); return n in this._attrs ? this._attrs[n] : null; };
  Element.prototype.getAttributeNames = function () { return Object.keys(this._attrs); };
  Element.prototype.setAttribute = function (n, v) {
    n = n.toLowerCase(); v = String(v);
    const old = this._attrs[n];
    this._attrs[n] = v;
    if (n === "class") this._classes = v.split(/\s+/).filter(Boolean);
    if (n === "value") this._value = v;
    recordMutation("attributes", this, { attributeName: n, oldValue: old !== undefined ? old : null });
  };
  Element.prototype.removeAttribute = function (n) {
    n = n.toLowerCase(); const old = this._attrs[n]; delete this._attrs[n];
    if (n === "class") this._classes = [];
    recordMutation("attributes", this, { attributeName: n, oldValue: old !== undefined ? old : null });
  };
  Element.prototype.hasAttribute = function (n) { return n.toLowerCase() in this._attrs; };
  Element.prototype.toggleAttribute = function (n, force) {
    if (this.hasAttribute(n) && force !== true) { this.removeAttribute(n); return false; }
    if (!this.hasAttribute(n) && force !== false) { this.setAttribute(n, ""); return true; }
    return this.hasAttribute(n);
  };
  Object.defineProperty(Element.prototype, "attributes", {
    get() { const out = []; for (const k in this._attrs) out.push({ name: k, value: this._attrs[k] }); return out; },
  });
  Object.defineProperty(Element.prototype, "value", {
    get() { if (this._value !== undefined) return this._value; if ("value" in this._attrs) return this._attrs["value"]; return ""; },
    set(v) { this._value = String(v); },
  });
  Object.defineProperty(Element.prototype, "checked", { get() { return !!this._checked; }, set(v) { this._checked = !!v; } });
  Object.defineProperty(Element.prototype, "href", { get() { return this._attrs["href"] || ""; }, set(v) { this.setAttribute("href", v); } });
  Object.defineProperty(Element.prototype, "name", { get() { return this._attrs["name"] || ""; }, set(v) { this.setAttribute("name", v); } });
  Object.defineProperty(Element.prototype, "type", { get() { return this._attrs["type"] || ""; }, set(v) { this.setAttribute("type", v); } });
  Object.defineProperty(Element.prototype, "textContent", {
    get() { let s = ""; const walk = (n) => { for (const c of n.childNodes) { if (c.nodeType === TEXT_NODE) s += c._text; else if (c.nodeType === ELEMENT_NODE) walk(c); } }; walk(this); return s; },
    set(v) { this.childNodes = []; this.appendChild(new TextNode(String(v))); },
  });
  Object.defineProperty(Element.prototype, "innerText", { get() { return this.textContent; }, set(v) { this.textContent = v; } });
  Object.defineProperty(Element.prototype, "innerHTML", {
    get() { return serializeChildren(this); },
    set(v) { this.childNodes = []; const nodes = buildNodes(JSON.parse(__parse_html_fragment(String(v)))); for (const n of nodes) this.appendChild(n); },
  });
  Object.defineProperty(Element.prototype, "outerHTML", { get() { return serializeNode(this); } });
  Element.prototype.insertAdjacentHTML = function (pos, html) {
    const nodes = buildNodes(JSON.parse(__parse_html_fragment(String(html))));
    pos = String(pos).toLowerCase();
    if (pos === "beforeend") { for (const n of nodes) this.appendChild(n); }
    else if (pos === "afterbegin") { for (let i = nodes.length - 1; i >= 0; i--) this.insertBefore(nodes[i], this.firstChild); }
    else if (pos === "beforebegin" && this.parentNode) { for (const n of nodes) this.parentNode.insertBefore(n, this); }
    else if (pos === "afterend" && this.parentNode) { const ref = this.nextSibling; for (const n of nodes) this.parentNode.insertBefore(n, ref); }
  };
  function toNodes(args) { const out = []; for (const a of args) out.push(typeof a === "string" ? new TextNode(a) : a); return out; }
  Element.prototype.append = function () { for (const n of toNodes(arguments)) this.appendChild(n); };
  Element.prototype.prepend = function () { const ns = toNodes(arguments); for (let i = ns.length - 1; i >= 0; i--) this.insertBefore(ns[i], this.firstChild); };
  Element.prototype.before = function () { if (!this.parentNode) return; for (const n of toNodes(arguments)) this.parentNode.insertBefore(n, this); };
  Element.prototype.after = function () { if (!this.parentNode) return; const ref = this.nextSibling; for (const n of toNodes(arguments)) this.parentNode.insertBefore(n, ref); };
  Element.prototype.replaceWith = function () { if (!this.parentNode) return; const p = this.parentNode; const ref = this.nextSibling; p.removeChild(this); for (const n of toNodes(arguments)) p.insertBefore(n, ref); };
  Element.prototype.replaceChildren = function () { this.childNodes = []; for (const n of toNodes(arguments)) this.appendChild(n); };
  Element.prototype.querySelector = function (sel) { return querySel(this, sel, false)[0] || null; };
  Element.prototype.querySelectorAll = function (sel) { return querySel(this, sel, true); };
  Element.prototype.getElementsByTagName = function (tag) {
    tag = tag.toLowerCase(); const out = [];
    const walk = (n) => { for (const c of n.children) { if (tag === "*" || c.localName === tag) out.push(c); walk(c); } };
    walk(this); return out;
  };
  Element.prototype.getElementsByClassName = function (cls) {
    const out = []; const walk = (n) => { for (const c of n.children) { if (c._classes.includes(cls)) out.push(c); walk(c); } }; walk(this); return out;
  };
  Element.prototype.closest = function (sel) { let n = this; while (n && n.nodeType === ELEMENT_NODE) { if (n.matches(sel)) return n; n = n.parentNode; } return null; };
  Element.prototype.matches = function (sel) {
    for (const grp of String(sel).split(",")) { if (matchesCompound(this, parseCompound(grp.trim()))) return true; } return false;
  };
  Element.prototype.click = function () { this.dispatchEvent(new FalconEvent("click", { bubbles: true, cancelable: true })); };
  Element.prototype.focus = function () {}; Element.prototype.blur = function () {};
  Element.prototype.getBoundingClientRect = function () { return { top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0, x: 0, y: 0 }; };
  Object.defineProperty(Element.prototype, "offsetWidth", { get() { return 0; } });
  Object.defineProperty(Element.prototype, "offsetHeight", { get() { return 0; } });
  Object.defineProperty(Element.prototype, "clientWidth", { get() { return 0; } });
  Object.defineProperty(Element.prototype, "clientHeight", { get() { return 0; } });
  Element.prototype.setAttributeNS = function (_ns, n, v) { this.setAttribute(n, v); };
  Element.prototype.getAttributeNS = function (_ns, n) { return this.getAttribute(n); };
  Element.prototype.hasAttributeNS = function (_ns, n) { return this.hasAttribute(n); };
  Element.prototype.scrollIntoView = function () {};
  // form controls
  Object.defineProperty(Element.prototype, "elements", {
    get() { if (this.localName !== "form") return []; return this.querySelectorAll("input,textarea,select,button"); },
  });
  Element.prototype.submit = function () { this.dispatchEvent(new FalconEvent("submit", { bubbles: true, cancelable: false })); };
  Element.prototype.requestSubmit = function () { this.dispatchEvent(new FalconEvent("submit", { bubbles: true, cancelable: true })); };
  Element.prototype.reset = function () {};

  // ---- serialization ----
  const VOID = new Set(["area","base","br","col","embed","hr","img","input","link","meta","param","source","track","wbr"]);
  function esc(s) { return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"); }
  function escAttr(s) { return String(s).replace(/&/g, "&amp;").replace(/"/g, "&quot;"); }
  function serializeNode(n) {
    if (n.nodeType === TEXT_NODE) return esc(n._text);
    if (n.nodeType === COMMENT_NODE) return "<!--" + n._text + "-->";
    if (n.nodeType !== ELEMENT_NODE) return "";
    let s = "<" + n.localName;
    for (const k in n._attrs) s += " " + k + '="' + escAttr(n._attrs[k]) + '"';
    s += ">";
    if (VOID.has(n.localName)) return s;
    s += serializeChildren(n); s += "</" + n.localName + ">"; return s;
  }
  function serializeChildren(n) { let s = ""; for (const c of n.childNodes) s += serializeNode(c); return s; }

  // ---- selector engine (R20) ----
  function parseCompound(str) {
    const c = { tag: null, id: null, classes: [], attrs: [], not: [] };
    // extract :not(...) first
    str = str.replace(/:not\(([^)]*)\)/g, function (_, inner) { c.not.push(parseCompound(inner.trim())); return ""; });
    const re = /([#.]?[\w-]+)|(\[[^\]]+\])|(\*)/g;
    let m;
    while ((m = re.exec(str))) {
      const tok = m[0];
      if (tok === "*") continue;
      if (tok[0] === "#") c.id = tok.slice(1);
      else if (tok[0] === ".") c.classes.push(tok.slice(1));
      else if (tok[0] === "[") {
        const inner = tok.slice(1, -1);
        const mm = inner.match(/^\s*([\w-]+)\s*([~^$*|]?=)?\s*(.*?)\s*$/);
        if (mm) {
          const name = mm[1]; const op = mm[2] || null;
          let val = mm[3] || ""; val = val.replace(/^["']|["']$/g, "");
          c.attrs.push({ name: name, op: op, val: val });
        }
      } else c.tag = tok.toLowerCase();
    }
    return c;
  }
  function attrMatch(el, a) {
    const has = a.name in el._attrs; if (!has) return false;
    if (!a.op) return true;
    const v = el._attrs[a.name];
    switch (a.op) {
      case "=": return v === a.val;
      case "^=": return v.indexOf(a.val) === 0;
      case "$=": return a.val.length <= v.length && v.slice(v.length - a.val.length) === a.val;
      case "*=": return v.indexOf(a.val) >= 0;
      case "~=": return v.split(/\s+/).includes(a.val);
      case "|=": return v === a.val || v.indexOf(a.val + "-") === 0;
      default: return false;
    }
  }
  function matchesCompound(el, c) {
    if (!el || el.nodeType !== ELEMENT_NODE) return false;
    if (c.tag && el.localName !== c.tag) return false;
    if (c.id && el._attrs["id"] !== c.id) return false;
    for (const cl of c.classes) if (!el._classes.includes(cl)) return false;
    for (const a of c.attrs) if (!attrMatch(el, a)) return false;
    for (const n of c.not) if (matchesCompound(el, n)) return false;
    return true;
  }
  function tokenizeSelector(group) {
    // returns [{combinator, compound}]; combinator is ' ', '>', '+', '~'
    const seq = []; let combinator = " ";
    const parts = group.trim().split(/\s*([>+~])\s*|\s+/).filter((p) => p !== undefined && p !== "");
    for (const p of parts) {
      if (p === ">" || p === "+" || p === "~") { combinator = p; continue; }
      seq.push({ combinator: combinator, compound: parseCompound(p) }); combinator = " ";
    }
    return seq;
  }
  function querySel(root, selector, all) {
    const results = [];
    for (const group of String(selector).split(",")) {
      const seq = tokenizeSelector(group);
      if (!seq.length) continue;
      const walk = (n) => {
        for (const c of n.children) {
          if (matchSeq(c, seq)) { if (!results.includes(c)) results.push(c); if (!all) return true; }
          if (walk(c) && !all) return true;
        }
        return false;
      };
      walk(root);
      if (!all && results.length) break;
    }
    return results;
  }
  function matchSeq(el, seq) {
    let i = seq.length - 1;
    if (!matchesCompound(el, seq[i].compound)) return false;
    let node = el; i--;
    while (i >= 0) {
      const comb = seq[i + 1].combinator;
      const want = seq[i].compound;
      if (comb === ">") { node = node.parentNode; if (!matchesCompound(node, want)) return false; }
      else if (comb === "+") { node = node.previousElementSibling; if (!node || !matchesCompound(node, want)) return false; }
      else if (comb === "~") {
        let s = node.previousElementSibling; let found = false;
        while (s) { if (matchesCompound(s, want)) { found = true; node = s; break; } s = s.previousElementSibling; }
        if (!found) return false;
      } else { // descendant
        node = node.parentNode; let found = false;
        while (node && node.nodeType === ELEMENT_NODE) { if (matchesCompound(node, want)) { found = true; break; } node = node.parentNode; }
        if (!found) return false;
      }
      i--;
    }
    return true;
  }

  // ---- document build ----
  function buildNodes(arr) { const out = []; for (const j of arr) out.push(buildNode(j)); return out.filter(Boolean); }
  function buildNode(j) {
    if (j.t === "text") return new TextNode(j.text);
    if (j.t === "comment") return new CommentNode(j.text);
    if (j.t === "element") {
      const el = new Element(j.tag);
      for (const k in j.attrs) el.setAttribute(k, j.attrs[k]);
      if (j.children) for (const c of buildNodes(j.children)) el.appendChild(c);
      return el;
    }
    if (j.t === "root") { const el = new Element("html"); if (j.children) for (const c of buildNodes(j.children)) el.appendChild(c); return el; }
    return null;
  }

  // ---- Document ----
  function Document() { Node.call(this, DOCUMENT_NODE); this._title = ""; this.readyState = "loading"; }
  Document.prototype = Object.create(Node.prototype);
  Document.prototype.createElement = function (tag) { return new Element(tag); };
  Document.prototype.createElementNS = function (_ns, tag) { return new Element(tag); };
  Document.prototype.createTextNode = function (t) { return new TextNode(String(t)); };
  Document.prototype.createComment = function (t) { return new CommentNode(String(t)); };
  Document.prototype.createDocumentFragment = function () { return new DocumentFragment(); };
  Document.prototype.getElementById = function (id) {
    let found = null;
    const walk = (n) => { for (const c of n.children) { if (c._attrs["id"] === id) { found = c; return true; } if (walk(c)) return true; } return false; };
    if (this.documentElement) walk(this.documentElement);
    return found;
  };
  Document.prototype.querySelector = function (s) { return this.documentElement ? querySel(this.documentElement, s, false)[0] || null : null; };
  Document.prototype.querySelectorAll = function (s) { return this.documentElement ? querySel(this.documentElement, s, true) : []; };
  Document.prototype.getElementsByTagName = function (t) { return this.documentElement ? this.documentElement.getElementsByTagName(t) : []; };
  Document.prototype.getElementsByClassName = function (c) { return this.documentElement ? this.documentElement.getElementsByClassName(c) : []; };
  Document.prototype.getElementsByName = function (nm) { return this.documentElement ? this.documentElement.querySelectorAll("[name=" + nm + "]") : []; };
  Document.prototype.addEventListener = Node.prototype.addEventListener;
  Document.prototype.removeEventListener = Node.prototype.removeEventListener;
  Document.prototype.dispatchEvent = Node.prototype.dispatchEvent;
  Object.defineProperty(Document.prototype, "title", {
    get() { const t = this.documentElement && this.documentElement.getElementsByTagName("title")[0]; return t ? t.textContent : this._title; },
    set(v) { this._title = String(v); const t = this.documentElement && this.documentElement.getElementsByTagName("title")[0]; if (t) t.textContent = String(v); },
  });
  Object.defineProperty(Document.prototype, "cookie", { get() { return ""; }, set() {} });
  Object.defineProperty(Document.prototype, "forms", { get() { return this.documentElement ? this.documentElement.getElementsByTagName("form") : []; } });
  Object.defineProperty(Document.prototype, "links", { get() { return this.documentElement ? this.documentElement.querySelectorAll("a[href]") : []; } });

  const document = new Document();
  g.document = document;
  g.Node = Node; g.Element = Element; g.HTMLElement = Element; g.Text = TextNode; g.Comment = CommentNode;
  g.window = g; g.self = g; g.top = g; g.parent = g; g.frames = g;
  g.navigator = { userAgent: __UA__, platform: "Linux x86_64", language: "en-US", languages: ["en-US", "en"], onLine: true, cookieEnabled: false };
  g.screen = { width: 1280, height: 800, availWidth: 1280, availHeight: 800, colorDepth: 24 };
  g.innerWidth = 1280; g.innerHeight = 800; g.devicePixelRatio = 1;
  g.location = __LOCATION__;

  // ---- history (R12) ----
  const _historyState = { _stack: [null], _i: 0, state: null, length: 1,
    pushState: function (st, _t, url) { this._stack.push(st); this._i++; this.state = st !== undefined ? st : null; this.length = this._stack.length; if (url) applyUrl(url); },
    replaceState: function (st, _t, url) { this._stack[this._i] = st; this.state = st !== undefined ? st : null; if (url) applyUrl(url); },
    back: function () {}, forward: function () {}, go: function () {}, scrollRestoration: "auto" };
  function applyUrl(url) {
    try { const u = new g.URL(url, g.location.href); g.location.pathname = u.pathname; g.location.search = u.search; g.location.hash = u.hash; g.location.href = u.href; }
    catch (_) {}
  }
  g.history = _historyState;

  g.getComputedStyle = function () { return { getPropertyValue() { return ""; } }; };
  g.matchMedia = function () { return { matches: false, media: "", addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {} }; };
  g.requestAnimationFrame = function (fn) { return setTimeout(function () { fn(Date.now ? 0 : 0); }, 16); };
  g.cancelAnimationFrame = function (id) { clearTimeout(id); };
  g.alert = function () {}; g.confirm = function () { return true; }; g.prompt = function () { return null; };
  g.scrollTo = function () {}; g.scroll = function () {}; g.scrollBy = function () {}; g.focus = function () {}; g.blur = function () {};
  window.addEventListener = Node.prototype.addEventListener.bind(window);
  window.removeEventListener = Node.prototype.removeEventListener.bind(window);
  window.dispatchEvent = Node.prototype.dispatchEvent.bind(window);
  window._listeners = Object.create(null);

  // ---- Storage (R13) ----
  function Storage() { this._d = Object.create(null); }
  Storage.prototype.getItem = function (k) { k = String(k); return k in this._d ? this._d[k] : null; };
  Storage.prototype.setItem = function (k, v) { this._d[String(k)] = String(v); };
  Storage.prototype.removeItem = function (k) { delete this._d[String(k)]; };
  Storage.prototype.clear = function () { this._d = Object.create(null); };
  Storage.prototype.key = function (i) { const ks = Object.keys(this._d); return i < ks.length ? ks[i] : null; };
  Object.defineProperty(Storage.prototype, "length", { get() { return Object.keys(this._d).length; } });
  g.localStorage = new Storage();
  g.sessionStorage = new Storage();
  g.Storage = Storage;

  // ---- URL / URLSearchParams (R10) ----
  function URLSearchParams(init) {
    this._p = [];
    if (typeof init === "string") {
      init = init.replace(/^\?/, "");
      if (init) for (const pair of init.split("&")) { const idx = pair.indexOf("="); const k = idx < 0 ? pair : pair.slice(0, idx); const v = idx < 0 ? "" : pair.slice(idx + 1); this._p.push([decodeURIComponent(k), decodeURIComponent(v)]); }
    } else if (init && typeof init === "object") {
      if (typeof init.forEach === "function" && !Array.isArray(init)) init.forEach((v, k) => this._p.push([k, String(v)]));
      else for (const k in init) this._p.push([k, String(init[k])]);
    }
  }
  URLSearchParams.prototype.get = function (k) { for (const p of this._p) if (p[0] === k) return p[1]; return null; };
  URLSearchParams.prototype.getAll = function (k) { return this._p.filter((p) => p[0] === k).map((p) => p[1]); };
  URLSearchParams.prototype.has = function (k) { return this._p.some((p) => p[0] === k); };
  URLSearchParams.prototype.set = function (k, v) { let done = false; this._p = this._p.filter((p) => { if (p[0] === k) { if (!done) { p[1] = String(v); done = true; return true; } return false; } return true; }); if (!done) this._p.push([k, String(v)]); };
  URLSearchParams.prototype.append = function (k, v) { this._p.push([String(k), String(v)]); };
  URLSearchParams.prototype.delete = function (k) { this._p = this._p.filter((p) => p[0] !== k); };
  URLSearchParams.prototype.forEach = function (fn) { for (const p of this._p) fn(p[1], p[0], this); };
  URLSearchParams.prototype.keys = function () { return this._p.map((p) => p[0])[Symbol.iterator](); };
  URLSearchParams.prototype.values = function () { return this._p.map((p) => p[1])[Symbol.iterator](); };
  URLSearchParams.prototype.entries = function () { return this._p.map((p) => [p[0], p[1]])[Symbol.iterator](); };
  URLSearchParams.prototype[Symbol.iterator] = function () { return this.entries(); };
  URLSearchParams.prototype.toString = function () { return this._p.map((p) => encodeURIComponent(p[0]) + "=" + encodeURIComponent(p[1])).join("&"); };
  g.URLSearchParams = URLSearchParams;

  function URLClass(url, base) {
    const parsed = __parse_url(String(url), base ? String(base) : (g.location ? g.location.href : ""));
    const o = JSON.parse(parsed);
    if (o.error) throw new TypeError("Invalid URL: " + url);
    this.href = o.href; this.protocol = o.protocol; this.host = o.host; this.hostname = o.hostname;
    this.port = o.port; this.pathname = o.pathname; this.search = o.search; this.hash = o.hash;
    this.origin = o.origin; this.username = o.username || ""; this.password = o.password || "";
    this.searchParams = new URLSearchParams(o.search);
  }
  URLClass.prototype.toString = function () { return this.href; };
  URLClass.prototype.toJSON = function () { return this.href; };
  g.URL = URLClass;

  // atob/btoa
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  g.btoa = function (s) {
    let out = ""; s = String(s);
    for (let i = 0; i < s.length; i += 3) {
      const c1 = s.charCodeAt(i), c2 = s.charCodeAt(i + 1), c3 = s.charCodeAt(i + 2);
      out += B64[c1 >> 2] + B64[((c1 & 3) << 4) | (c2 >> 4)];
      out += isNaN(c2) ? "=" : B64[((c2 & 15) << 2) | (c3 >> 6)];
      out += isNaN(c2) || isNaN(c3) ? "=" : B64[c3 & 63];
    }
    return out;
  };
  g.atob = function (s) {
    s = String(s).replace(/=+$/, ""); let out = "";
    for (let i = 0; i < s.length; i += 4) {
      const c0 = B64.indexOf(s[i]);
      const c1 = B64.indexOf(s[i + 1]);
      const c2 = s[i + 2] !== undefined ? B64.indexOf(s[i + 2]) : 0;
      const c3 = s[i + 3] !== undefined ? B64.indexOf(s[i + 3]) : 0;
      const n = (c0 << 18) | (c1 << 12) | (c2 << 6) | c3;
      out += String.fromCharCode((n >> 16) & 255);
      if (s[i + 2]) out += String.fromCharCode((n >> 8) & 255);
      if (s[i + 3]) out += String.fromCharCode(n & 255);
    }
    return out;
  };

  // ---- timers (driven by Rust) ----
  let timerSeq = 1;
  const timerCbs = Object.create(null);
  g.setTimeout = function (fn, delay) { const id = timerSeq++; timerCbs[id] = { fn, args: Array.prototype.slice.call(arguments, 2) }; __set_timer(id, delay | 0); return id; };
  g.setInterval = function (fn, delay) { const id = timerSeq++; timerCbs[id] = { fn, args: Array.prototype.slice.call(arguments, 2), interval: delay | 0 }; __set_timer(id, delay | 0); return id; };
  g.clearTimeout = function (id) { delete timerCbs[id]; __clear_timer(id); };
  g.clearInterval = g.clearTimeout;
  g.queueMicrotask = function (fn) { Promise.resolve().then(fn); };
  g.setImmediate = function (fn) { return setTimeout(fn, 0); };
  g.__fire_timer = function (id) {
    const t = timerCbs[id]; if (!t) return;
    if (t.interval === undefined) delete timerCbs[id];
    try { t.fn.apply(null, t.args); } catch (e) { reportPageError(e); }
    if (t.interval !== undefined && timerCbs[id]) __set_timer(id, t.interval);
  };

  // ---- AbortController / AbortSignal (R15) ----
  function AbortSignal() { this.aborted = false; this.reason = undefined; this._listeners = Object.create(null); }
  AbortSignal.prototype.addEventListener = Node.prototype.addEventListener;
  AbortSignal.prototype.removeEventListener = Node.prototype.removeEventListener;
  AbortSignal.prototype.dispatchEvent = Node.prototype.dispatchEvent;
  Object.defineProperty(AbortSignal.prototype, "parentNode", { get() { return null; } });
  g.AbortSignal = AbortSignal;
  function AbortController() { this.signal = new AbortSignal(); }
  AbortController.prototype.abort = function (reason) {
    if (this.signal.aborted) return;
    this.signal.aborted = true; this.signal.reason = reason !== undefined ? reason : new Error("AbortError");
    try { this.signal.dispatchEvent(new FalconEvent("abort", {})); } catch (e) { reportPageError(e); }
    if (typeof this.signal.onabort === "function") { try { this.signal.onabort(); } catch (e) { reportPageError(e); } }
  };
  g.AbortController = AbortController;

  // ---- fetch / XHR (driven by Rust) ----
  let reqSeq = 1;
  const pendingReq = Object.create(null);
  function DOMException(msg, name) { this.message = msg; this.name = name || "Error"; }
  DOMException.prototype.toString = function () { return this.name + ": " + this.message; };
  g.DOMException = DOMException;
  g.fetch = function (url, opts) {
    opts = opts || {};
    const id = reqSeq++;
    const headers = opts.headers ? JSON.stringify(headersToObj(opts.headers)) : "{}";
    const signal = opts.signal;
    return new Promise(function (resolve, reject) {
      if (signal && signal.aborted) { reject(new DOMException("The operation was aborted.", "AbortError")); return; }
      pendingReq[id] = { resolve, reject, kind: "fetch", signal };
      if (signal) signal.addEventListener("abort", function () { if (pendingReq[id]) { delete pendingReq[id]; try { __abort_request(id); } catch (_) {} reject(new DOMException("The operation was aborted.", "AbortError")); } });
      __enqueue_request(id, String(url), (opts.method || "GET"), headers, opts.body ? String(opts.body) : "", false);
    });
  };
  function headersToObj(h) {
    if (!h) return {};
    if (h instanceof Headers) return h._o;
    if (typeof h.forEach === "function" && !Array.isArray(h)) { const o = {}; try { h.forEach((v, k) => (o[k] = v)); return o; } catch (_) {} }
    if (Array.isArray(h)) { const o = {}; for (const pair of h) o[pair[0]] = pair[1]; return o; }
    return h;
  }
  function Headers(init) { this._o = {}; if (init) { const o = headersToObj(init); for (const k in o) this._o[String(k).toLowerCase()] = o[k]; } }
  Headers.prototype.get = function (k) { const v = this._o[String(k).toLowerCase()]; return v === undefined ? null : v; };
  Headers.prototype.set = function (k, v) { this._o[String(k).toLowerCase()] = String(v); };
  Headers.prototype.append = function (k, v) { const kk = String(k).toLowerCase(); this._o[kk] = this._o[kk] ? this._o[kk] + ", " + v : String(v); };
  Headers.prototype.has = function (k) { return String(k).toLowerCase() in this._o; };
  Headers.prototype.delete = function (k) { delete this._o[String(k).toLowerCase()]; };
  Headers.prototype.forEach = function (fn) { for (const k in this._o) fn(this._o[k], k, this); };
  g.Headers = Headers;
  function FalconResponse(id, ok, status, headersJson, body, url) {
    this.ok = ok; this.status = status; this.statusText = ok ? "OK" : "Error";
    this._body = body; this.headers = new Headers(JSON.parse(headersJson || "{}"));
    this.url = url || ""; this.redirected = false; this.type = "basic"; this.bodyUsed = false;
  }
  FalconResponse.prototype.text = function () { return Promise.resolve(this._body); };
  FalconResponse.prototype.json = function () { try { return Promise.resolve(JSON.parse(this._body)); } catch (e) { return Promise.reject(e); } };
  FalconResponse.prototype.clone = function () { const r = new FalconResponse(0, this.ok, this.status, "{}", this._body, this.url); r.headers = this.headers; return r; };
  g.Response = FalconResponse;

  g.XMLHttpRequest = function () {
    this.readyState = 0; this.status = 0; this.statusText = ""; this.responseText = ""; this.response = "";
    this._headers = {}; this._respHeaders = ""; this.onreadystatechange = null; this.onload = null; this.onerror = null; this.onabort = null;
    this._listeners = Object.create(null);
  };
  g.XMLHttpRequest.prototype.addEventListener = Node.prototype.addEventListener;
  g.XMLHttpRequest.prototype.open = function (method, url) { this._method = method; this._url = url; this.readyState = 1; this._fire("readystatechange"); };
  g.XMLHttpRequest.prototype.setRequestHeader = function (k, v) { this._headers[k] = v; };
  g.XMLHttpRequest.prototype._fire = function (t) { if (typeof this["on" + t] === "function") { try { this["on" + t](new FalconEvent(t, {})); } catch (e) { reportPageError(e); } } };
  g.XMLHttpRequest.prototype.send = function (body) {
    const id = reqSeq++; const self = this;
    pendingReq[id] = { kind: "xhr", xhr: self };
    __enqueue_request(id, String(this._url), this._method || "GET", JSON.stringify(this._headers), body ? String(body) : "", true);
  };
  g.XMLHttpRequest.prototype.getAllResponseHeaders = function () { return this._respHeaders; };
  g.XMLHttpRequest.prototype.getResponseHeader = function (k) {
    const re = new RegExp("^" + k + ":\\s*(.*)$", "im"); const m = this._respHeaders.match(re); return m ? m[1].trim() : null;
  };
  g.XMLHttpRequest.prototype.abort = function () {};

  g.__resolve_request = function (id, ok, status, headersJson, body, error, finalUrl) {
    const p = pendingReq[id]; if (!p) return; delete pendingReq[id];
    if (p.kind === "fetch") {
      if (error && status === 0) p.reject(new TypeError(error || "network error"));
      else { const r = new FalconResponse(id, ok, status, headersJson, body, finalUrl); if (finalUrl && finalUrl !== undefined) r.redirected = false; p.resolve(r); }
    } else if (p.kind === "xhr") {
      const x = p.xhr;
      x.status = status; x.statusText = ok ? "OK" : "Error"; x.responseText = body; x.response = body; x.readyState = 4;
      try { x._respHeaders = xhrHeaders(headersJson); } catch (_) {}
      x._fire("readystatechange");
      if (status === 0 && error) { x._fire("error"); } else { x._fire("load"); }
    }
  };
  function xhrHeaders(j) { const o = JSON.parse(j || "{}"); let s = ""; for (const k in o) s += k + ": " + o[k] + "\r\n"; return s; }

  // ---- document construction from Rust ----
  g.__build_document = function (json) {
    const root = buildNode(JSON.parse(json));
    document.appendChild(root); document.documentElement = root;
    const els = root.getElementsByTagName("*");
    for (const e of els) { if (e.localName === "head") document.head = e; if (e.localName === "body") document.body = e; }
    if (!document.head) { document.head = document.createElement("head"); root.insertBefore(document.head, root.firstChild); }
    if (!document.body) { document.body = document.createElement("body"); root.appendChild(document.body); }
    document.readyState = "interactive";
  };
  g.__node_count = function () { return _nodeCount; };

  g.__fire_lifecycle = function () {
    try { document.dispatchEvent(new FalconEvent("DOMContentLoaded", { bubbles: true })); } catch (e) { reportPageError(e); }
    document.readyState = "complete";
    try { window.dispatchEvent(new FalconEvent("load", { bubbles: false })); } catch (e) { reportPageError(e); }
    if (typeof window.onload === "function") { try { window.onload(new FalconEvent("load", {})); } catch (e) { reportPageError(e); } }
    if (typeof document.onreadystatechange === "function") { try { document.onreadystatechange(); } catch (e) { reportPageError(e); } }
  };

  g.__report_error = function (msg) { __page_error(String(msg)); };

  g.__extract_form = function (selector) {
    let el = document.querySelector(selector);
    if (!el) return JSON.stringify({ error: "submit selector not found" });
    let form = el; while (form && form.localName !== "form") form = form.parentNode;
    if (!form) form = document.querySelector("form");
    if (!form) return JSON.stringify({ error: "no form" });
    const fields = {};
    for (const inp of form.getElementsByTagName("input")) { const name = inp.getAttribute("name"); if (name) fields[name] = inp.value || inp.getAttribute("value") || ""; }
    for (const ta of form.getElementsByTagName("textarea")) { const name = ta.getAttribute("name"); if (name) fields[name] = ta.value || ""; }
    for (const sel of form.getElementsByTagName("select")) { const name = sel.getAttribute("name"); if (name) fields[name] = sel.value || ""; }
    return JSON.stringify({ action: form.getAttribute("action") || "", method: (form.getAttribute("method") || "GET").toUpperCase(), fields });
  };
  g.__set_input_value = function (selector, value) {
    const el = document.querySelector(selector);
    if (el) { el.value = value; el.setAttribute("value", value); el.dispatchEvent(new FalconEvent("input", { bubbles: true })); return true; }
    return false;
  };

  g.__collect = function () {
    let text = "";
    if (document.body) {
      const walk = (n) => { for (const c of n.childNodes) { if (c.nodeType === TEXT_NODE) text += c._text + " "; else if (c.nodeType === ELEMENT_NODE && c.localName !== "script" && c.localName !== "style") walk(c); } };
      walk(document.body);
    }
    return JSON.stringify({
      html: document.documentElement ? document.documentElement.outerHTML : "",
      text: text.replace(/\s+/g, " ").trim(),
      title: document.title,
      node_count: _nodeCount,
    });
  };
})();
