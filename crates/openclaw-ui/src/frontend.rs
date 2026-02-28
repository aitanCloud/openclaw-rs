//! Serves the embedded frontend assets (HTML, JS, CSS).
//!
//! All assets are embedded in the binary at compile time via `include_str!()`.
//! This keeps the deployment as a single binary with no external static files.

use axum::response::{Html, IntoResponse, Response};
use axum::http::{header, StatusCode};

/// The HTML shell page.
const INDEX_HTML: &str = include_str!("static/index.html");

/// The bundled JavaScript application.
const APP_JS: &str = include_str!("static/app.js");

/// The CSS stylesheet.
const STYLES_CSS: &str = include_str!("static/styles.css");

/// GET / — serve the HTML shell.
pub async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// GET /app.js — serve the bundled JavaScript.
pub async fn app_js() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        APP_JS,
    )
        .into_response()
}

/// GET /styles.css — serve the CSS stylesheet.
pub async fn styles_css() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_is_not_empty() {
        assert!(!INDEX_HTML.is_empty());
        assert!(INDEX_HTML.contains("<!DOCTYPE html>"));
        assert!(INDEX_HTML.contains("app.js"));
        assert!(INDEX_HTML.contains("styles.css"));
    }

    #[test]
    fn app_js_is_not_empty() {
        assert!(!APP_JS.is_empty());
        assert!(APP_JS.contains("OpenClaw"));
    }

    #[test]
    fn styles_css_is_not_empty() {
        assert!(!STYLES_CSS.is_empty());
        assert!(STYLES_CSS.contains("--bg-primary"));
    }

    #[tokio::test]
    async fn index_returns_html() {
        let resp = index().await;
        let body = resp.0;
        assert!(body.contains("<!DOCTYPE html>"));
    }

    #[tokio::test]
    async fn app_js_returns_js_content_type() {
        let resp = app_js().await;
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(ct, "application/javascript; charset=utf-8");
    }

    #[tokio::test]
    async fn styles_css_returns_css_content_type() {
        let resp = styles_css().await;
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(ct, "text/css; charset=utf-8");
    }
}
