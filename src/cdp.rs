//! Chromium CDP backend for the optional fallback (R33/R37). Drives
//! chrome-headless-shell over the Chrome DevTools Protocol with a synchronous
//! websocket client. Isolated here so Falcon stays standalone when disabled.

use serde_json::{json, Value};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

type Sock = WebSocket<MaybeTlsStream<TcpStream>>;

pub struct ChromeResult {
    pub html: String,
    pub text: String,
    pub title: String,
    pub status: u16,
    pub final_url: String,
    pub console_errors: Vec<String>,
    pub page_errors: Vec<String>,
}

/// A launched chrome-headless-shell, killed on drop.
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Render a URL via the Chromium backend. `backend` is a `ws://` CDP URL, or a
/// chrome-headless-shell binary path to launch. Returns Err when the backend is
/// unavailable so the caller can fall back honestly (R38e).
pub fn render_via_chromium(
    backend: &str,
    url: &str,
    timeout_secs: u64,
) -> Result<ChromeResult, String> {
    if backend.is_empty() {
        return Err("no chromium backend configured".into());
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(2));
    let (ws_url, _child) = if backend.starts_with("ws://") || backend.starts_with("wss://") {
        (backend.to_string(), None)
    } else {
        let (u, child) = launch_chrome(backend, deadline)?;
        (u, Some(child))
    };
    let mut sock = connect_ws(&ws_url, deadline)?;
    let res = drive(&mut sock, url, deadline);
    let _ = sock.close(None);
    res
}

fn launch_chrome(bin: &str, deadline: Instant) -> Result<(String, ChildGuard), String> {
    // Pick a port derived from pid to avoid collisions across workers.
    let port = 9500 + (std::process::id() % 400) as u16;
    let child = std::process::Command::new(bin)
        .args([
            &format!("--remote-debugging-port={port}"),
            "--headless=new",
            "--remote-allow-origins=*",
            "--disable-gpu",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "about:blank",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to launch chromium: {e}"))?;
    let guard = ChildGuard(child);
    // Poll /json/version for the browser ws endpoint.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .map_err(|e| e.to_string())?;
    loop {
        if Instant::now() > deadline {
            return Err("chromium did not become ready".into());
        }
        if let Ok(resp) = client.get(format!("http://127.0.0.1:{port}/json/version")).send()
            && let Ok(v) = resp.json::<Value>()
                && let Some(ws) = v.get("webSocketDebuggerUrl").and_then(|x| x.as_str()) {
                    return Ok((ws.to_string(), guard));
                }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn connect_ws(ws_url: &str, deadline: Instant) -> Result<Sock, String> {
    loop {
        if Instant::now() > deadline {
            return Err("cdp connect timed out".into());
        }
        match connect(ws_url) {
            Ok((sock, _resp)) => {
                if let MaybeTlsStream::Plain(s) = sock.get_ref() {
                    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                }
                return Ok(sock);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

struct Cdp<'a> {
    sock: &'a mut Sock,
    id: i64,
    events: Vec<Value>,
}

impl Cdp<'_> {
    fn send(&mut self, method: &str, params: Value, session: Option<&str>, deadline: Instant) -> Result<Value, String> {
        self.id += 1;
        let mid = self.id;
        let mut msg = json!({ "id": mid, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.sock
            .send(Message::Text(msg.to_string()))
            .map_err(|e| format!("cdp send: {e}"))?;
        loop {
            if Instant::now() > deadline {
                return Err(format!("cdp timeout waiting for {method}"));
            }
            match self.sock.read() {
                Ok(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if v.get("id").and_then(|x| x.as_i64()) == Some(mid) {
                            return Ok(v.get("result").cloned().unwrap_or(json!({})));
                        } else if v.get("method").is_some() {
                            self.events.push(v);
                        }
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("cdp read: {e}")),
            }
        }
    }

    fn drain(&mut self, dur: Duration) {
        let until = Instant::now() + dur;
        while Instant::now() < until {
            match self.sock.read() {
                Ok(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t)
                        && v.get("method").is_some() {
                            self.events.push(v);
                        }
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

fn drive(sock: &mut Sock, url: &str, deadline: Instant) -> Result<ChromeResult, String> {
    let mut cdp = Cdp { sock, id: 0, events: Vec::new() };
    let target = cdp.send("Target.createTarget", json!({"url": "about:blank"}), None, deadline)?;
    let tid = target.get("targetId").and_then(|x| x.as_str()).ok_or("no targetId")?.to_string();
    let attach = cdp.send("Target.attachToTarget", json!({"targetId": tid, "flatten": true}), None, deadline)?;
    let sess = attach.get("sessionId").and_then(|x| x.as_str()).ok_or("no sessionId")?.to_string();
    for dom in ["Page", "Runtime", "Log", "Network"] {
        let _ = cdp.send(&format!("{dom}.enable"), json!({}), Some(&sess), deadline);
    }
    cdp.events.clear();
    cdp.send("Page.navigate", json!({"url": url}), Some(&sess), deadline)?;
    cdp.drain(Duration::from_millis(1500));

    // Collect console errors / exceptions / doc status from buffered events.
    let mut console_errors = Vec::new();
    let mut page_errors = Vec::new();
    let mut doc_status: u16 = 0;
    for ev in &cdp.events {
        let m = ev.get("method").and_then(|x| x.as_str()).unwrap_or("");
        let p = ev.get("params").cloned().unwrap_or(json!({}));
        match m {
            "Runtime.consoleAPICalled" if p.get("type").and_then(|x| x.as_str()) == Some("error") => {
                console_errors.push("console.error".to_string());
            }
            "Runtime.exceptionThrown" => page_errors.push("uncaught exception".to_string()),
            "Network.responseReceived"
                if p.get("type").and_then(|x| x.as_str()) == Some("Document") => {
                    doc_status = p.get("response").and_then(|r| r.get("status")).and_then(|s| s.as_u64()).unwrap_or(0) as u16;
                }
            _ => {}
        }
    }

    let text = eval_string(&mut cdp, &sess, "document.body?document.body.innerText:''", deadline);
    let title = eval_string(&mut cdp, &sess, "document.title", deadline);
    let html = eval_string(&mut cdp, &sess, "document.documentElement?document.documentElement.outerHTML:''", deadline);
    let final_url = eval_string(&mut cdp, &sess, "location.href", deadline);
    let _ = cdp.send("Target.closeTarget", json!({"targetId": tid}), None, deadline);

    Ok(ChromeResult {
        html,
        text: text.split_whitespace().collect::<Vec<_>>().join(" "),
        title,
        status: if doc_status == 0 { 200 } else { doc_status },
        final_url,
        console_errors,
        page_errors,
    })
}

fn eval_string(cdp: &mut Cdp, sess: &str, expr: &str, deadline: Instant) -> String {
    match cdp.send(
        "Runtime.evaluate",
        json!({"expression": expr, "returnByValue": true}),
        Some(sess),
        deadline,
    ) {
        Ok(v) => v.get("result").and_then(|r| r.get("value")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
        Err(_) => String::new(),
    }
}
