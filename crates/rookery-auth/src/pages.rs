//! The few plain pages of signing in, outside any shell. Light or dark as the browser is.

use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A page with a title and a body that is already escaped.
pub fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let title = escape(title);
    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="referrer" content="same-origin"><title>{title}</title>
<style>:root{{color-scheme:light dark;--bg:#f3f3f5;--card:#fff;--line:#dcdce1;--text:#17171a;--dim:#5b5b66;--btn:#17171a;--btn-text:#fff}}@media (prefers-color-scheme:dark){{:root{{--bg:#0a0a0b;--card:#111113;--line:#2a2a30;--text:#f1f1f3;--dim:#9a9aa5;--btn:#f1f1f3;--btn-text:#17171a}}}}*{{box-sizing:border-box}}body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;padding:16px;background:var(--bg);color:var(--text);font:14px/1.5 system-ui,sans-serif}}main{{width:100%;max-width:26rem;padding:24px;border:1px solid var(--line);border-radius:12px;background:var(--card)}}h1{{margin:0 0 8px;font-size:18px;font-weight:600}}p{{margin:.25rem 0;color:var(--dim)}}label{{display:block;margin:1rem 0 .25rem}}input,button{{width:100%;padding:.5rem .75rem;border-radius:8px;font:inherit}}input{{border:1px solid var(--line);background:transparent;color:inherit}}button{{margin-top:1rem;border:0;background:var(--btn);color:var(--btn-text);font-weight:600;cursor:pointer}}</style>
</head><body><main><h1>{title}</h1>{body}</main></body></html>"#
    );
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            // Same origin, not none: a form posted with no referrer carries `Origin: null`,
            // which the same-origin check of the sign-in link refuses.
            (header::REFERRER_POLICY, "same-origin"),
        ],
        Html(html),
    )
        .into_response()
}

/// A page that says something went wrong.
pub fn problem(status: StatusCode, title: &str, message: &str) -> Response {
    page(status, title, &format!("<p>{}</p>", escape(message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_keeps_its_referrer_on_this_server() {
        let response = page(StatusCode::OK, "Sign in", "");
        assert_eq!(response.headers()[header::REFERRER_POLICY], "same-origin");
    }
}
