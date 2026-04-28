use pulldown_cmark::{html, Event, Options, Parser};

/// Render a user-supplied string as HTML through pulldown-cmark, dropping any
/// raw HTML in the input so agents can't inject markup.
pub fn to_html(src: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let parser = Parser::new_ext(src, opts).filter_map(|ev| match ev {
        Event::Html(_) | Event::InlineHtml(_) => None,
        other => Some(other),
    });

    let mut out = String::with_capacity(src.len() + src.len() / 4);
    html::push_html(&mut out, parser);
    out
}

#[cfg(test)]
mod tests {
    use super::to_html;

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
        // pulldown-cmark splits the input into text + html events. We strip the
        // html events, so the script *tags* never appear in output. The inner text
        // (e.g. `alert("xss")`) survives as literal prose, which is safe — without
        // <script> tags the browser treats it as text, not JS.
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
}
