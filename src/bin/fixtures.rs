//! Deterministic fixture site for falcon's acceptance tests (A1–A6).
//! Serves static/JS/fetch/timer/error/subresource/login/infinite/huge pages.
//! Usage: fixtures [bind]  (default 127.0.0.1:8300)

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};

const PAD: &str = "This is padding body text to comfortably exceed the minimum text length threshold used by the tour endpoint so that healthy pages are marked ok true reliably.";

#[tokio::main]
async fn main() {
    let bind = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8300".into());
    let app = Router::new()
        .route("/", get(static_home))
        .route("/jsdom", get(jsdom))
        .route("/fetchrender", get(fetchrender))
        .route("/api/data", get(api_data))
        .route("/deferred", get(deferred))
        .route("/consoleerror", get(console_error))
        .route("/exception", get(exception))
        .route("/badsub", get(badsub))
        .route("/login", get(login_get).post(login_post))
        .route("/private", get(private))
        .route("/infinite", get(infinite))
        .route("/hugealloc", get(hugealloc))
        .route("/xhr", get(xhr_page))
        .route("/fetchloop", get(fetchloop))
        .route("/recursivetimer", get(recursivetimer))
        .route("/redirectloop", get(redirectloop))
        .route("/hugehtml", get(hugehtml))
        .route("/ping", get(|| async { "pong" }));

    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();
    eprintln!("fixtures listening on {bind}");
    axum::serve(listener, app).await.unwrap();
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html><head><title>{title}</title></head><body>{body}</body></html>"
    ))
}

async fn static_home() -> impl IntoResponse {
    page("Static Home", &format!("<h1>Static Home</h1><p id=marker>static-content-marker</p><p>{PAD}</p>"))
}

async fn jsdom() -> impl IntoResponse {
    page(
        "JS DOM",
        &format!(
            "<div id=root></div><p>{PAD}</p><script>\
             var d=document.createElement('div');d.id='built';\
             d.textContent='js-built-content-marker';\
             document.getElementById('root').appendChild(d);\
             document.title='JS DOM Ready';</script>"
        ),
    )
}

async fn fetchrender() -> impl IntoResponse {
    page(
        "Fetch Render",
        &format!(
            "<div id=out>loading</div><p>{PAD}</p><script>\
             fetch('/api/data').then(function(r){{return r.json();}}).then(function(j){{\
             document.getElementById('out').textContent=j.msg;}});</script>"
        ),
    )
}

async fn api_data() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"msg":"fetched-content-marker delivered via fetch json body with plenty of extra words here"}"#,
    )
}

async fn xhr_page() -> impl IntoResponse {
    page(
        "XHR",
        &format!(
            "<div id=out>loading</div><p>{PAD}</p><script>\
             var x=new XMLHttpRequest();x.open('GET','/api/data');\
             x.onload=function(){{document.getElementById('out').textContent=JSON.parse(x.responseText).msg;}};\
             x.send();</script>"
        ),
    )
}

async fn deferred() -> impl IntoResponse {
    page(
        "Deferred",
        &format!(
            "<div id=out>loading</div><p>{PAD}</p><script>\
             setTimeout(function(){{document.getElementById('out').textContent='deferred-content-marker';}},50);\
             </script>"
        ),
    )
}

async fn console_error() -> impl IntoResponse {
    page("Console Error", &format!("<p>{PAD}</p><script>console.error('boom-console-marker');</script>"))
}

async fn exception() -> impl IntoResponse {
    page("Exception", &format!("<p>{PAD}</p><script>throw new Error('thrown-page-marker');</script>"))
}

async fn badsub() -> impl IntoResponse {
    page("Bad Subresource", &format!("<img src=\"/missing-image.png\"><p>{PAD}</p>"))
}

async fn login_get() -> impl IntoResponse {
    page(
        "Login",
        "<p>login-page-marker please sign in to continue to your private area</p>\
         <form action=\"/login\" method=\"post\">\
         <input type=text name=username id=username>\
         <input type=password name=password id=password>\
         <button type=submit id=submit>Sign in</button></form>",
    )
}

async fn login_post(req: Request) -> Response {
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024).await.unwrap_or_default();
    let form = String::from_utf8_lossy(&body);
    let ok = form.contains("username=") && form.contains("password=");
    if ok {
        Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/private")
            .header(header::SET_COOKIE, "sid=authok; Path=/")
            .body(Body::empty())
            .unwrap()
    } else {
        (StatusCode::BAD_REQUEST, "missing credentials").into_response()
    }
}

async fn private(headers: HeaderMap) -> Response {
    let authed = headers
        .get(header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .map(|c| c.contains("sid=authok"))
        .unwrap_or(false);
    if authed {
        page(
            "Private",
            &format!("<h1>private-area-marker</h1><p>Welcome, you are authenticated. {PAD}</p>"),
        )
        .into_response()
    } else {
        Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/login")
            .body(Body::empty())
            .unwrap()
    }
}

async fn infinite() -> impl IntoResponse {
    page("Infinite", &format!("<p>{PAD}</p><script>while(true){{}}</script>"))
}

async fn fetchloop() -> impl IntoResponse {
    // Endless fetch chain: each response triggers another fetch (R25/E9).
    page(
        "Fetch Loop",
        &format!(
            "<p>{PAD}</p><script>function go(){{fetch('/ping').then(function(){{go();}});}}go();</script>"
        ),
    )
}

async fn recursivetimer() -> impl IntoResponse {
    // A timer that schedules another timer forever (R25/E8).
    page(
        "Recursive Timer",
        &format!(
            "<p>{PAD}</p><script>function t(){{setTimeout(t,0);}}t();</script>"
        ),
    )
}

async fn redirectloop() -> Response {
    // Server redirect loop (R25/E10).
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, "/redirectloop")
        .body(Body::empty())
        .unwrap()
}

async fn hugehtml() -> impl IntoResponse {
    // A very large HTML document (R25/E11) — ~200k elements.
    let mut body = String::with_capacity(4_000_000);
    for i in 0..200_000 {
        body.push_str(&format!("<div id=n{i}>x</div>"));
    }
    page("Huge HTML", &body)
}

async fn hugealloc() -> impl IntoResponse {
    // The sink is global and periodically observed so V8 cannot dead-code-
    // eliminate the allocations — this genuinely grows the heap.
    page(
        "Huge Alloc",
        &format!(
            "<p>{PAD}</p><script>globalThis.__leak=[];var n=0;\
             while(true){{globalThis.__leak.push(new Array(2000000).join('x'));\
             n++;if(n%10===0)document.title='leaked '+globalThis.__leak.length;}}</script>"
        ),
    )
}
