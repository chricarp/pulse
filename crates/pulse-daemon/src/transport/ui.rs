//! The web UI: `apps/web/dist`, embedded in the binary at build time. When it has not been built
//! (a plain `cargo build` without Node), a small page says how to build it.
//!
//! Single-page app: paths without a file extension fall back to `index.html`. Hashed assets
//! under `assets/` are cached for a year; everything else is revalidated (ETag).

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../apps/web/dist"]
#[allow_missing = true]
struct Assets;

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Pulse</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:40rem;margin:4rem auto;padding:0 1rem;color:#222;background:#fff}
@media (prefers-color-scheme:dark){body{color:#ddd;background:#111}}code{background:rgba(127,127,127,.15);padding:.1em .3em;border-radius:3px}</style></head>
<body><h1>Pulse is running</h1>
<p>The web UI was not built into this binary. Build it, then rebuild Pulse:</p>
<pre><code>pnpm --dir apps/web install
pnpm --dir apps/web build
cargo build --release</code></pre>
<p>Everything works from the CLI meanwhile: <code>pulse status</code>, <code>pulse tail</code>, <code>pulse rules list</code>.</p>
</body></html>
"#;

fn security_headers(headers: &mut HeaderMap) {
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // The UI has approve buttons; it must not be framed by another page.
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
}

pub(crate) async fn serve(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let looks_like_file = path.rsplit('/').next().is_some_and(|last| last.contains('.'));
    let (name, file) = match Assets::get(path) {
        Some(f) => (path, f),
        None if !looks_like_file => match Assets::get("index.html") {
            Some(f) => ("index.html", f),
            None => return placeholder(),
        },
        None if path == "index.html" => return placeholder(),
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let etag = format!("\"{}\"", crate::auth::hex(&file.metadata.sha256_hash()[..16]));
    let mut res = if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(&etag)
    {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut r = Response::new(Body::from(file.data.into_owned()));
        if let Ok(v) = HeaderValue::from_str(file.metadata.mimetype()) {
            r.headers_mut().insert(header::CONTENT_TYPE, v);
        }
        r
    };
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    security_headers(h);
    res
}

fn placeholder() -> Response {
    let mut res = Response::new(Body::from(PLACEHOLDER));
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    security_headers(h);
    res
}
