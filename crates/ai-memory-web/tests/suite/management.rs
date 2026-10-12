//! Public management assets disclose no data and cannot be framed.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

#[tokio::test]
async fn management_assets_work_under_a_prefix_and_deny_framing() {
    let router = axum::Router::new().nest("/memory", ai_memory_web::management::router("/memory"));
    for uri in [
        "/memory/manage",
        "/memory/manage/",
        "/memory/manage/assets/app.js",
        "/memory/manage/assets/app.css",
        "/memory/manage/assets/logo.webp",
        "/memory/manage/assets/instrument-sans-latin-wdth-normal.woff2",
        "/memory/manage/assets/bricolage-grotesque-latin-standard-normal.woff2",
        "/memory/manage/assets/commit-mono-latin-400-normal.woff2",
        "/memory/manage/assets/font-licenses.txt",
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-frame-options"], "DENY");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let bytes = axum::body::to_bytes(response.into_body(), 400_000)
            .await
            .unwrap();
        if uri.ends_with("manage") {
            let html = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(html.contains("<base href=\"/memory/manage/\">"));
            assert!(html.contains("content=\"/memory\""));
            assert!(!html.contains("aim_"));
        }
    }
    // Only explicit vendored assets exist; no filesystem-backed wildcard.
    for uri in [
        "/memory/manage/assets/config.toml",
        "/memory/manage/assets/fonts.txt",
        "/memory/manage/assets/%2e%2e%2fconfig.toml",
        "/memory/manage/assets/../../db/memory.sqlite",
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
