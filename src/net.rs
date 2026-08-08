//! Networking (F5): blocking reqwest client with a per-job cookie jar, block
//! filter, redirect handling, and a request log feeding responses/failed_requests.

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::redirect::Policy;
use std::collections::HashSet;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct ResponseRecord {
    pub url: String,
    pub status: u16,
    pub method: String,
}

pub struct FetchOutcome {
    pub ok: bool,
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub error: Option<String>,
    pub bytes: u64,
}

/// Build a cookie-storing client. Clone it to share the cookie jar across a
/// tour's pages (reqwest's blocking Client is an Arc internally).
pub fn build_client(ua: &str, insecure: bool, max_redirects: usize) -> Client {
    Client::builder()
        .cookie_store(true)
        .redirect(Policy::limited(max_redirects))
        .referer(false) // we set Referer explicitly (R30)
        .timeout(Duration::from_secs(15))
        .danger_accept_invalid_certs(insecure)
        .user_agent(ua)
        .build()
        .expect("client build")
}

pub struct NetClient {
    client: Client,
    base: Url,
    ua: String,
    blocked: HashSet<String>,
    max_bytes: u64,
    referrer: Option<String>,
    pub responses: Vec<ResponseRecord>,
    pub failed: Vec<String>,
}

impl NetClient {
    pub fn with_client(client: Client, base: &str, ua: &str, blocked: Vec<String>) -> Self {
        let base_url = Url::parse(base).unwrap_or_else(|_| Url::parse("http://localhost/").unwrap());
        NetClient {
            client,
            base: base_url,
            ua: ua.to_string(),
            blocked: blocked.into_iter().collect(),
            max_bytes: u64::MAX,
            referrer: None,
            responses: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Cap on a single response body (R21).
    pub fn set_max_bytes(&mut self, n: u64) {
        self.max_bytes = n;
    }

    /// The document URL used as `Referer` on subsequent requests (R30).
    pub fn set_referrer(&mut self, url: &str) {
        self.referrer = Some(url.to_string());
    }

    /// Re-point the base URL (e.g. after redirects) for relative resolution.
    pub fn set_base(&mut self, url: &str) {
        if let Ok(u) = Url::parse(url) {
            self.base = u;
        }
    }

    pub fn is_blocked_kind(&self, kind: &str) -> bool {
        self.blocked.contains(kind)
    }

    /// Perform a request, recording the outcome into responses/failed.
    pub fn fetch(
        &mut self,
        url: &str,
        method: &str,
        headers: &[(String, String)],
        body: Option<&str>,
    ) -> FetchOutcome {
        let abs = match self.base.join(url) {
            Ok(u) => u.to_string(),
            Err(e) => {
                let msg = format!("BADURL {url}: {e}");
                self.failed.push(msg.clone());
                return FetchOutcome {
                    ok: false,
                    status: 0,
                    final_url: url.to_string(),
                    headers: vec![],
                    body: String::new(),
                    error: Some(msg),
                    bytes: 0,
                };
            }
        };
        let mut hmap = HeaderMap::new();
        hmap.insert(USER_AGENT, HeaderValue::from_str(&self.ua).unwrap());
        // Referer (R30): send the document URL on same-scheme requests; omit on
        // downgrade to a less-secure scheme.
        if let Some(referrer) = &self.referrer {
            let downgrade = referrer.starts_with("https://") && abs.starts_with("http://");
            if !downgrade
                && let Ok(val) = HeaderValue::from_str(referrer) {
                    hmap.insert(reqwest::header::REFERER, val);
                }
        }
        for (k, v) in headers {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                hmap.insert(name, val);
            }
        }
        let m = reqwest::Method::from_bytes(method.to_uppercase().as_bytes())
            .unwrap_or(reqwest::Method::GET);
        let mut req = self.client.request(m.clone(), &abs).headers(hmap);
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let final_url = resp.url().to_string();
                let hdrs: Vec<(String, String)> = resp
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                // Cap the download at max_bytes (R21). Read one byte past the
                // limit to detect an over-limit body.
                use std::io::Read;
                let mut buf = Vec::new();
                let read_err = resp
                    .take(self.max_bytes.saturating_add(1))
                    .read_to_end(&mut buf)
                    .err();
                let too_big = buf.len() as u64 > self.max_bytes;
                if too_big {
                    buf.truncate(self.max_bytes as usize);
                }
                let bytes = buf.len() as u64;
                let text = String::from_utf8_lossy(&buf).into_owned();
                self.responses.push(ResponseRecord {
                    url: final_url.clone(),
                    status,
                    method: m.to_string(),
                });
                if status >= 400 {
                    self.failed.push(format!("{} {} {}", status, m, final_url));
                }
                if too_big {
                    self.failed
                        .push(format!("RESPONSE_TOO_LARGE {} {}", final_url, self.max_bytes));
                }
                if let Some(e) = read_err {
                    self.failed.push(format!("READERR {final_url}: {e}"));
                }
                FetchOutcome {
                    ok: status < 400 && !too_big,
                    status,
                    final_url,
                    headers: hdrs,
                    body: text,
                    error: if too_big {
                        Some("response too large".into())
                    } else {
                        None
                    },
                    bytes,
                }
            }
            Err(e) => {
                let msg = format!("NETERR {} {}: {}", m, abs, e);
                self.responses.push(ResponseRecord {
                    url: abs.clone(),
                    status: 0,
                    method: m.to_string(),
                });
                self.failed.push(msg.clone());
                FetchOutcome {
                    ok: false,
                    status: 0,
                    final_url: abs,
                    headers: vec![],
                    body: String::new(),
                    error: Some(msg),
                    bytes: 0,
                }
            }
        }
    }
}

