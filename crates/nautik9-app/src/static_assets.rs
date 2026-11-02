//! Static files compiled into the binary, served without a token under `/static/`.
//!
//! Nothing here is private (page script, style, and the vendored KaTeX 0.16.11, MIT; see
//! `static/katex/LICENSE`). Lookup is by exact name in a fixed table, so a request path can
//! never reach the file system.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

type Asset = (&'static str, &'static str, &'static [u8]);

static ASSETS: &[Asset] = &[
    ("notes-plus.js", "text/javascript; charset=utf-8", include_bytes!("../static/notes-plus.js")),
    ("notes-plus.css", "text/css; charset=utf-8", include_bytes!("../static/notes-plus.css")),
    ("katex/katex.min.js", "text/javascript; charset=utf-8", include_bytes!("../static/katex/katex.min.js")),
    ("katex/katex.min.css", "text/css; charset=utf-8", include_bytes!("../static/katex/katex.min.css")),
    ("katex/LICENSE", "text/plain; charset=utf-8", include_bytes!("../static/katex/LICENSE")),
    ("katex/fonts/KaTeX_AMS-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_AMS-Regular.woff2")),
    ("katex/fonts/KaTeX_Caligraphic-Bold.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Caligraphic-Bold.woff2")),
    ("katex/fonts/KaTeX_Caligraphic-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Caligraphic-Regular.woff2")),
    ("katex/fonts/KaTeX_Fraktur-Bold.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Fraktur-Bold.woff2")),
    ("katex/fonts/KaTeX_Fraktur-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Fraktur-Regular.woff2")),
    ("katex/fonts/KaTeX_Main-Bold.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Main-Bold.woff2")),
    ("katex/fonts/KaTeX_Main-BoldItalic.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Main-BoldItalic.woff2")),
    ("katex/fonts/KaTeX_Main-Italic.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Main-Italic.woff2")),
    ("katex/fonts/KaTeX_Main-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Main-Regular.woff2")),
    ("katex/fonts/KaTeX_Math-BoldItalic.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Math-BoldItalic.woff2")),
    ("katex/fonts/KaTeX_Math-Italic.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Math-Italic.woff2")),
    ("katex/fonts/KaTeX_SansSerif-Bold.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_SansSerif-Bold.woff2")),
    ("katex/fonts/KaTeX_SansSerif-Italic.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_SansSerif-Italic.woff2")),
    ("katex/fonts/KaTeX_SansSerif-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_SansSerif-Regular.woff2")),
    ("katex/fonts/KaTeX_Script-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Script-Regular.woff2")),
    ("katex/fonts/KaTeX_Size1-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Size1-Regular.woff2")),
    ("katex/fonts/KaTeX_Size2-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Size2-Regular.woff2")),
    ("katex/fonts/KaTeX_Size3-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Size3-Regular.woff2")),
    ("katex/fonts/KaTeX_Size4-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Size4-Regular.woff2")),
    ("katex/fonts/KaTeX_Typewriter-Regular.woff2", "font/woff2", include_bytes!("../static/katex/fonts/KaTeX_Typewriter-Regular.woff2")),
];

pub fn lookup(name: &str) -> Option<(&'static str, &'static [u8])> {
    ASSETS.iter().find(|a| a.0 == name).map(|a| (a.1, a.2))
}

pub async fn serve(Path(name): Path<String>) -> Response {
    match lookup(&name) {
        Some((ct, body)) => (
            [
                (header::CONTENT_TYPE, ct),
                (header::CACHE_CONTROL, "public, max-age=3600"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn katex_is_vendored_with_its_licence_and_fonts() {
        assert!(lookup("katex/katex.min.js").unwrap().1.len() > 100_000);
        assert!(std::str::from_utf8(lookup("katex/LICENSE").unwrap().1).unwrap().contains("MIT License"));
        let css = std::str::from_utf8(lookup("katex/katex.min.css").unwrap().1).unwrap();
        // Every font the stylesheet names is in the table (works offline).
        for part in css.split("url(fonts/").skip(1) {
            let f = part.split(')').next().unwrap();
            assert!(lookup(&format!("katex/fonts/{f}")).is_some(), "missing font {f}");
        }
        assert!(!css.contains("http"), "stylesheet must not fetch anything remote");
    }

    #[test]
    fn only_listed_names_resolve() {
        assert!(lookup("../Cargo.toml").is_none());
        assert!(lookup("katex/../notes-plus.js").is_none());
        assert!(lookup("").is_none());
    }
}
