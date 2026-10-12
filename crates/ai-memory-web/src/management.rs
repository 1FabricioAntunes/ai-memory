//! Embedded React/Tailwind management console, compiled only on opt-in builds.

use axum::{
    Router,
    extract::Path,
    http::StatusCode,
    http::{HeaderValue, header},
    response::{Html, IntoResponse},
    routing::get,
};

async fn secure_response(mut response: axum::response::Response) -> axum::response::Response {
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Public static shell; data and mutations remain behind human-session authentication.
/// Vendored assets keep Cargo builds independent of Node and network access.
pub fn router(base_path: &str) -> Router {
    let base = crate::mount::normalize_prefix(base_path);
    let html = crate::mount::inject_base_path_meta(
        &crate::mount::inject_base_href(
            include_str!("../management-ui/dist/index.html"),
            &format!("{base}/manage/"),
        ),
        &base,
    );
    Router::new()
        .route(
            "/manage",
            get({
                let html = html.clone();
                move || async move { Html(html) }
            }),
        )
        .route("/manage/", get(move || async move { Html(html.clone()) }))
        .route("/manage/assets/{asset}", get(asset))
        .layer(axum::middleware::map_response(secure_response))
}

async fn asset(Path(name): Path<String>) -> axum::response::Response {
    let (mime, body): (&str, &[u8]) = match name.as_str() {
        "app.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../management-ui/dist/assets/app.js"),
        ),
        "app.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../management-ui/dist/assets/app.css"),
        ),
        "logo.webp" => (
            "image/webp",
            include_bytes!("../management-ui/dist/assets/logo.webp"),
        ),
        "instrument-sans-latin-wdth-normal.woff2" => (
            "font/woff2",
            include_bytes!("../management-ui/dist/assets/instrument-sans-latin-wdth-normal.woff2"),
        ),
        "bricolage-grotesque-latin-standard-normal.woff2" => (
            "font/woff2",
            include_bytes!(
                "../management-ui/dist/assets/bricolage-grotesque-latin-standard-normal.woff2"
            ),
        ),
        "commit-mono-latin-400-normal.woff2" => (
            "font/woff2",
            include_bytes!("../management-ui/dist/assets/commit-mono-latin-400-normal.woff2"),
        ),
        "font-licenses.txt" => (
            "text/plain; charset=utf-8",
            include_bytes!("../management-ui/licenses/fonts.txt"),
        ),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static(mime))],
        body,
    )
        .into_response()
}
