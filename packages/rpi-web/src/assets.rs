//! Complete frontend embedded in both the extension and standalone program.
use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/frontend.rs"));

/// Served when the crate is compiled without a generated `assets/` bundle.
const FALLBACK_INDEX: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"UTF-8\"/><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"/><title>rpi — frontend not built</title></head><body style=\"font-family:system-ui,sans-serif;max-width:34rem;margin:12vh auto;padding:0 1.5rem;line-height:1.6\"><h1>Frontend not built</h1><p>This rpi-web build does not embed the browser workspace. Build the frontend and rebuild, or point <code>--assets</code> at an existing bundle:</p><pre style=\"background:#f5f5f7;padding:1rem;border-radius:.5rem;overflow:auto\">cd packages/rpi-web/frontend\nnpm ci\nnpm run build</pre><p>Documentation: packages/rpi-web/README.md</p></body></html>";

/// Embedded bundle manifest, or the notice served without one.
pub fn index_html() -> &'static str {
    FILES
        .iter()
        .find(|(path, _)| *path == "index.html")
        .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
        .unwrap_or(FALLBACK_INDEX)
}

fn response(name: &str) -> Response {
    let Some((_, bytes)) = FILES.iter().find(|(path, _)| *path == name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, mime)], *bytes).into_response()
}

pub async fn asset(uri: Uri) -> Response {
    response(&format!("assets/{}", uri.path().trim_start_matches('/')))
}

pub async fn favicon() -> Response {
    response("favicon.svg")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontend_is_complete_and_paths_cannot_escape_the_inventory() {
        assert!(index_html().starts_with("<!doctype html>"));
        if FILES.is_empty() {
            // Built without a frontend bundle: only the notice page is served.
            assert_eq!(response("index.html").status(), StatusCode::NOT_FOUND);
            assert_eq!(response("favicon.svg").status(), StatusCode::NOT_FOUND);
            return;
        }
        assert_eq!(response("index.html").status(), StatusCode::OK);
        assert_eq!(
            response("favicon.svg").headers()[header::CONTENT_TYPE],
            "image/svg+xml"
        );
        let html =
            std::str::from_utf8(FILES.iter().find(|(n, _)| *n == "index.html").unwrap().1).unwrap();
        for fragment in html.split('"').filter(|s| s.starts_with("/assets/")) {
            assert_eq!(
                response(fragment.trim_start_matches('/')).status(),
                StatusCode::OK
            );
        }
        for path in ["../Cargo.toml", "assets/../../.env", "assets/missing.js"] {
            assert_eq!(response(path).status(), StatusCode::NOT_FOUND);
        }
    }
}
