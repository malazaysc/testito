use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

/// Render a user-supplied string as HTML through pulldown-cmark.
///
/// Two layers of XSS defense:
///   1. raw HTML events (block + inline) are dropped, so `<script>` and
///      `<img onerror>` written directly in the source disappear.
///   2. links and images are rewritten when their URL uses a dangerous scheme
///      (`javascript:`, `data:`, `vbscript:`) — preserving the link text but
///      defusing the click. Browsers also strip control characters before
///      checking the scheme, so we treat e.g. `java\tscript:` as dangerous too.
pub fn to_html(src: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let parser = Parser::new_ext(src, opts).filter_map(|ev| match ev {
        Event::Html(_) | Event::InlineHtml(_) => None,
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: sanitize_url(dest_url),
            title,
            id,
        })),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Image {
            link_type,
            dest_url: sanitize_url(dest_url),
            title,
            id,
        })),
        other => Some(other),
    });

    let mut out = String::with_capacity(src.len() + src.len() / 4);
    html::push_html(&mut out, parser);
    out
}

fn sanitize_url(url: CowStr<'_>) -> CowStr<'_> {
    if is_safe_url(&url) {
        url
    } else {
        CowStr::Borrowed("#")
    }
}

/// True if a URL is safe to use as an `href` / `src`. We accept the common
/// safe schemes plus relative references; everything else (including
/// `javascript:`, `data:`, `vbscript:`, and unknown schemes) is rejected.
fn is_safe_url(url: &str) -> bool {
    let trimmed = url.trim_start();
    if trimmed.is_empty() {
        return true; // empty href is a no-op, harmless
    }
    // Relative reference (path / fragment / query) — no scheme to worry about.
    let first = trimmed.as_bytes()[0];
    if first == b'/' || first == b'#' || first == b'?' || first == b'.' {
        return true;
    }
    // Look up to the first ':' for the scheme. Browsers strip ASCII whitespace
    // and control chars from the scheme before resolving, so we do too — a
    // sneaky `java\tscript:` is treated as `javascript:`.
    let mut scheme = String::new();
    for &b in trimmed.as_bytes() {
        if b == b':' {
            break;
        }
        if (b as char).is_ascii_whitespace() || b.is_ascii_control() {
            continue;
        }
        scheme.push(b.to_ascii_lowercase() as char);
    }
    // No colon at all → relative reference.
    if !trimmed.contains(':') {
        return true;
    }
    matches!(scheme.as_str(), "http" | "https" | "mailto")
}

#[cfg(test)]
mod tests {
    use super::{is_safe_url, to_html};

    #[test]
    fn renders_inline_code() {
        let html = to_html("Hit `Ctrl+C` to stop.");
        assert!(html.contains("<code>Ctrl+C</code>"), "got: {html}");
    }

    #[test]
    fn renders_fenced_code_block() {
        let html = to_html("```\nERR: oops\n```\n");
        assert!(html.contains("<pre><code>"));
        assert!(html.contains("ERR: oops"));
    }

    #[test]
    fn renders_links() {
        let html = to_html("see [docs](https://example.com)");
        assert!(
            html.contains(r#"<a href="https://example.com">docs</a>"#),
            "got: {html}"
        );
    }

    #[test]
    fn renders_lists() {
        let html = to_html("- one\n- two\n");
        assert!(html.contains("<ul>"));
        assert!(html.contains("<li>one</li>"));
        assert!(html.contains("<li>two</li>"));
    }

    #[test]
    fn drops_inline_html_so_no_xss() {
        let html = to_html(r#"hi <script>alert("xss")</script> end"#);
        assert!(!html.contains("<script"), "raw <script> tag leaked: {html}");
        assert!(
            !html.contains("</script"),
            "raw </script> tag leaked: {html}"
        );
        assert!(html.contains("hi"));
        assert!(html.contains("end"));
    }

    #[test]
    fn drops_event_handler_attrs_via_html_event() {
        let html = to_html(r#"<img src=x onerror="alert(1)">"#);
        assert!(!html.contains("<img"), "raw <img> leaked: {html}");
        assert!(!html.contains("onerror"), "onerror leaked: {html}");
    }

    #[test]
    fn drops_block_level_html() {
        let html = to_html("<iframe src=\"http://evil\"></iframe>\n\nhello");
        assert!(!html.contains("<iframe"));
        assert!(html.contains("hello"));
    }

    #[test]
    fn special_chars_in_text_are_escaped() {
        let html = to_html("a & b < c");
        assert!(html.contains("&amp;"));
        assert!(html.contains("&lt;"));
    }

    #[test]
    fn empty_input_returns_empty_string() {
        assert_eq!(to_html(""), "");
    }

    // ---- URL scheme sanitization ----

    #[test]
    fn link_with_javascript_scheme_is_rewritten_to_hash() {
        let html = to_html(r#"[click](javascript:alert(1))"#);
        assert!(
            !html.contains("javascript:"),
            "javascript: scheme leaked: {html}"
        );
        assert!(html.contains(r##"<a href="#">click</a>"##), "got: {html}");
    }

    #[test]
    fn link_with_data_scheme_is_rewritten() {
        let html = to_html(r#"[x](data:text/html,<script>alert(1)</script>)"#);
        assert!(!html.contains("data:"), "data: scheme leaked: {html}");
    }

    #[test]
    fn link_with_vbscript_scheme_is_rewritten() {
        let html = to_html(r#"[x](vbscript:msgbox(1))"#);
        assert!(!html.contains("vbscript:"), "vbscript: leaked: {html}");
    }

    #[test]
    fn link_with_uppercase_javascript_scheme_is_rewritten() {
        let html = to_html(r#"[x](JavaScript:alert(1))"#);
        assert!(
            !html.to_ascii_lowercase().contains("javascript:"),
            "case-insensitive scheme leaked: {html}"
        );
    }

    #[test]
    fn link_with_leading_whitespace_javascript_is_rewritten() {
        let html = to_html("[x](   javascript:alert(1))");
        assert!(
            !html.to_ascii_lowercase().contains("javascript:"),
            "leading-whitespace scheme leaked: {html}"
        );
    }

    #[test]
    fn link_with_control_chars_in_scheme_does_not_become_a_link() {
        // Browsers strip ASCII tab/CR/LF from the scheme — `java\tscript:` would
        // resolve as `javascript:` in a real <a href>. Belt-and-braces here:
        // (a) pulldown-cmark itself rejects the embedded tab so this never even
        // becomes a link tag; (b) `is_safe_url` strips control chars before
        // checking the scheme so it would also catch a parsed link if one ever
        // formed. Either way: no `<a href="javascript:...">` is produced.
        let html = to_html("[x](java\tscript:alert(1))");
        assert!(
            !html.contains(r#"<a href="javascript:"#),
            "javascript link materialized: {html}"
        );
        // And the literal `javascript:` does not appear inside an href anywhere.
        assert!(!html.contains(r#"href="javascript:"#));
    }

    #[test]
    fn image_with_javascript_scheme_is_rewritten() {
        let html = to_html("![alt](javascript:alert(1))");
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn relative_links_are_kept() {
        let html = to_html("[a](/foo) [b](#bar) [c](?q=1) [d](./x)");
        assert!(html.contains(r#"href="/foo""#));
        assert!(html.contains(r##"href="#bar""##));
        assert!(html.contains(r#"href="?q=1""#));
        assert!(html.contains(r#"href="./x""#));
    }

    #[test]
    fn mailto_is_kept() {
        let html = to_html("[mail](mailto:a@b.com)");
        assert!(html.contains(r#"href="mailto:a@b.com""#));
    }

    // ---- is_safe_url unit ----

    #[test]
    fn is_safe_url_table() {
        let safe = [
            "https://example.com",
            "http://example.com",
            "HTTP://example.com",
            "mailto:a@b.com",
            "/relative",
            "#frag",
            "?q=1",
            "./local",
            "",
        ];
        let unsafe_urls = [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "  javascript:alert(1)",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "data:text/html,x",
            "vbscript:x",
            "ftp://something",
            "ssh://h",
        ];
        for u in safe {
            assert!(is_safe_url(u), "expected safe: {u:?}");
        }
        for u in unsafe_urls {
            assert!(!is_safe_url(u), "expected unsafe: {u:?}");
        }
    }
}
