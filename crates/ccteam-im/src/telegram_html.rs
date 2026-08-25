//! Small, fail-safe Markdown subset renderer for Telegram's HTML mode.
//!
//! Telegram only accepts a small allowlist of tags in `parse_mode=HTML`, so
//! this deliberately renders the useful Markdown shapes and escapes every
//! other character as text. It is not a general Markdown parser.

/// Rendered Telegram HTML plus the visible UTF-16 length after entity parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMarkdown {
    /// HTML accepted by Telegram's `parse_mode=HTML`.
    pub html: String,
    /// Length of the rendered text, excluding HTML markup.
    pub text_utf16_len: usize,
}

#[derive(Default)]
struct Fragment {
    html: String,
    text_utf16_len: usize,
    contains_code: bool,
}

/// Render the supported Markdown subset as Telegram HTML.
pub fn render_markdown(input: &str) -> RenderedMarkdown {
    let lines: Vec<&str> = input.split_inclusive('\n').collect();
    let mut out = Fragment::default();
    let mut index = 0;

    while index < lines.len() {
        let raw_line = lines[index];
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);

        if is_fence_line(line) {
            let language = fence_language(line);
            let mut body = String::new();
            let mut closing = None;
            let mut cursor = index + 1;
            while cursor < lines.len() {
                let candidate = lines[cursor];
                let candidate_line = candidate.strip_suffix('\n').unwrap_or(candidate);
                if is_fence_line(candidate_line) {
                    closing = Some(candidate.ends_with('\n'));
                    break;
                }
                body.push_str(candidate);
                cursor += 1;
            }
            append_fragment(&mut out, render_code_block(&body, language));
            if let Some(has_newline) = closing {
                if has_newline {
                    append_escaped_text(&mut out, "\n");
                }
                index = cursor + 1;
            } else {
                index = lines.len();
            }
            continue;
        }

        if is_blockquote_line(line) {
            let mut quote = Fragment::default();
            let mut cursor = index;
            while cursor < lines.len() {
                let raw_quote = lines[cursor];
                let quote_line = raw_quote.strip_suffix('\n').unwrap_or(raw_quote);
                let Some(content) = strip_blockquote(quote_line) else {
                    break;
                };
                // Telegram does not allow code/pre nested in blockquotes.
                append_fragment(&mut quote, render_inline(content, false, false));
                if raw_quote.ends_with('\n') {
                    append_escaped_text(&mut quote, "\n");
                }
                cursor += 1;
            }
            append_fragment(&mut out, wrap("blockquote", quote));
            index = cursor;
            continue;
        }

        let line_fragment = if let Some(content) = heading_content(line) {
            let inner = render_inline(content, true, true);
            if inner.contains_code {
                inner
            } else {
                wrap("b", inner)
            }
        } else if let Some(content) = unordered_item(line) {
            let mut item = Fragment::default();
            append_escaped_text(&mut item, "• ");
            append_fragment(&mut item, render_inline(content, true, true));
            item
        } else if let Some((number, content)) = ordered_item(line) {
            let mut item = Fragment::default();
            append_escaped_text(&mut item, number);
            append_escaped_text(&mut item, ". ");
            append_fragment(&mut item, render_inline(content, true, true));
            item
        } else {
            render_inline(line, true, true)
        };
        append_fragment(&mut out, line_fragment);
        if raw_line.ends_with('\n') {
            append_escaped_text(&mut out, "\n");
        }
        index += 1;
    }

    RenderedMarkdown {
        html: out.html,
        text_utf16_len: out.text_utf16_len,
    }
}

fn render_code_block(body: &str, language: Option<&str>) -> Fragment {
    let escaped = escape_html(body);
    let mut html = String::from("<pre><code");
    if let Some(language) = language {
        html.push_str(" class=\"language-");
        html.push_str(&escape_html_attr(language));
        html.push('"');
    }
    html.push('>');
    html.push_str(&escaped);
    html.push_str("</code></pre>");
    Fragment {
        html,
        text_utf16_len: utf16_len(body),
        contains_code: true,
    }
}

fn render_inline(input: &str, allow_code: bool, allow_links: bool) -> Fragment {
    let mut out = Fragment::default();
    let mut index = 0;
    while index < input.len() {
        if input.as_bytes()[index] == b'\\' {
            let ch = input[index..].chars().next().expect("valid char boundary");
            append_escaped_text(&mut out, &ch.to_string());
            index += ch.len_utf8();
            if index < input.len() {
                let escaped = input[index..].chars().next().expect("valid char boundary");
                append_escaped_text(&mut out, &escaped.to_string());
                index += escaped.len_utf8();
            }
            continue;
        }

        if allow_code && input.as_bytes()[index] == b'`' {
            if let Some(end) = find_unescaped(input, b'`', index + 1) {
                if end > index + 1 {
                    let code = &input[index + 1..end];
                    let mut fragment = Fragment {
                        html: String::from("<code>"),
                        text_utf16_len: utf16_len(code),
                        contains_code: true,
                    };
                    fragment.html.push_str(&escape_html(code));
                    fragment.html.push_str("</code>");
                    append_fragment(&mut out, fragment);
                    index = end + 1;
                    continue;
                }
            }
        }

        if allow_links && input.as_bytes()[index] == b'[' {
            if let Some((end, url_start, url_end)) = parse_link(input, index) {
                let label = render_inline(&input[index + 1..end], false, false);
                let href = escape_html_attr(&input[url_start..url_end]);
                let mut fragment = Fragment {
                    html: format!("<a href=\"{href}\">"),
                    text_utf16_len: label.text_utf16_len,
                    contains_code: false,
                };
                fragment.html.push_str(&label.html);
                fragment.html.push_str("</a>");
                append_fragment(&mut out, fragment);
                index = url_end + 1;
                continue;
            }
        }

        let (delimiter, tag) = if starts_with(input, index, "**") {
            ("**", "b")
        } else if starts_with(input, index, "__") {
            ("__", "b")
        } else if starts_with(input, index, "~~") {
            ("~~", "s")
        } else if input.as_bytes()[index] == b'*' {
            ("*", "i")
        } else if input.as_bytes()[index] == b'_' {
            ("_", "i")
        } else {
            let ch = input[index..].chars().next().expect("valid char boundary");
            append_escaped_text(&mut out, &ch.to_string());
            index += ch.len_utf8();
            continue;
        };

        let content_start = index + delimiter.len();
        let Some(end) = find_delimiter(input, delimiter, content_start) else {
            append_escaped_text(&mut out, delimiter);
            index += delimiter.len();
            continue;
        };
        if end == content_start {
            append_escaped_text(&mut out, delimiter);
            index += delimiter.len();
            continue;
        }
        let inner = render_inline(&input[content_start..end], allow_code, allow_links);
        if inner.contains_code {
            // Telegram rejects formatting around code/pre. Keep the code and
            // visible text, dropping only the unsupported outer decoration.
            append_fragment(&mut out, inner);
        } else {
            append_fragment(&mut out, wrap(tag, inner));
        }
        index = end + delimiter.len();
    }
    out
}

fn wrap(tag: &str, inner: Fragment) -> Fragment {
    let mut html = String::with_capacity(tag.len() * 2 + inner.html.len() + 5);
    html.push('<');
    html.push_str(tag);
    html.push('>');
    html.push_str(&inner.html);
    html.push_str("</");
    html.push_str(tag);
    html.push('>');
    Fragment {
        html,
        text_utf16_len: inner.text_utf16_len,
        contains_code: inner.contains_code,
    }
}

fn append_fragment(out: &mut Fragment, fragment: Fragment) {
    out.html.push_str(&fragment.html);
    out.text_utf16_len += fragment.text_utf16_len;
    out.contains_code |= fragment.contains_code;
}

fn append_escaped_text(out: &mut Fragment, text: &str) {
    out.html.push_str(&escape_html(text));
    out.text_utf16_len += utf16_len(text);
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_html_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

fn is_fence_line(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn fence_language(line: &str) -> Option<&str> {
    let info = line.trim_start().strip_prefix("```")?.trim();
    let language = info.split_whitespace().next()?;
    if language
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-'))
    {
        Some(language)
    } else {
        None
    }
}

fn is_blockquote_line(line: &str) -> bool {
    strip_blockquote(line).is_some()
}

fn strip_blockquote(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("> ")
        .or_else(|| trimmed.strip_prefix('>'))
}

fn heading_content(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let hash_count = trimmed.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hash_count) && trimmed.as_bytes().get(hash_count) == Some(&b' ') {
        Some(trimmed[hash_count + 1..].trim())
    } else {
        None
    }
}

fn unordered_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
}

fn ordered_item(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let dot = trimmed.find(". ")?;
    if dot > 0 && trimmed[..dot].bytes().all(|b| b.is_ascii_digit()) {
        Some((&trimmed[..dot], &trimmed[dot + 2..]))
    } else {
        None
    }
}

fn starts_with(input: &str, index: usize, needle: &str) -> bool {
    input
        .get(index..)
        .is_some_and(|tail| tail.starts_with(needle))
}

fn find_unescaped(input: &str, needle: u8, start: usize) -> Option<usize> {
    input.as_bytes()[start..]
        .iter()
        .enumerate()
        .find_map(|(offset, byte)| {
            let index = start + offset;
            (*byte == needle && !is_escaped(input, index)).then_some(index)
        })
}

fn find_delimiter(input: &str, delimiter: &str, start: usize) -> Option<usize> {
    let mut cursor = start;
    while cursor < input.len() {
        let relative = input[cursor..].find(delimiter)?;
        let index = cursor + relative;
        if !is_escaped(input, index)
            && (delimiter.len() > 1
                || (input.as_bytes().get(index.wrapping_sub(1)) != Some(&delimiter.as_bytes()[0])
                    && input.as_bytes().get(index + 1) != Some(&delimiter.as_bytes()[0])))
        {
            return Some(index);
        }
        cursor = index + 1;
    }
    None
}

fn is_escaped(input: &str, index: usize) -> bool {
    index > 0 && input.as_bytes()[index - 1] == b'\\'
}

fn parse_link(input: &str, start: usize) -> Option<(usize, usize, usize)> {
    let label_end = find_unescaped(input, b']', start + 1)?;
    if input.as_bytes().get(label_end + 1) != Some(&b'(') {
        return None;
    }
    let url_start = label_end + 2;
    let url_end = find_unescaped(input, b')', url_start)?;
    (url_end > url_start).then_some((label_end, url_start, url_end))
}

#[cfg(test)]
mod tests {
    use super::render_markdown;
    #[test]
    fn escapes_html_text() {
        assert_eq!(render_markdown("<&>").html, "&lt;&amp;&gt;");
    }

    #[test]
    fn renders_fenced_code_with_language_and_escaped_body() {
        let rendered = render_markdown("```rust\nif a < b && c > d\n```");
        assert_eq!(
            rendered.html,
            "<pre><code class=\"language-rust\">if a &lt; b &amp;&amp; c &gt; d\n</code></pre>"
        );
    }

    #[test]
    fn renders_nested_inline_markup_as_well_formed_html() {
        let rendered = render_markdown("**bold *italic* ** and [link](https://e.test?a=1&b=2)");
        assert_eq!(
            rendered.html,
            "<b>bold <i>italic</i> </b> and <a href=\"https://e.test?a=1&amp;b=2\">link</a>"
        );
    }

    #[test]
    fn renders_headings_lists_and_blockquotes() {
        let rendered = render_markdown("# Heading\n- bullet\n* another\n1. numbered\n> quoted");
        assert_eq!(
            rendered.html,
            "<b>Heading</b>\n• bullet\n• another\n1. numbered\n<blockquote>quoted</blockquote>"
        );
    }

    #[test]
    fn renders_all_inline_styles() {
        let rendered = render_markdown("**bold** __bold__ *italic* _italic_ ~~strike~~ `code`");
        assert_eq!(
            rendered.html,
            "<b>bold</b> <b>bold</b> <i>italic</i> <i>italic</i> <s>strike</s> <code>code</code>"
        );
    }

    #[test]
    fn code_is_not_nested_inside_inline_formatting() {
        let rendered = render_markdown("**before `code` after**");
        assert_eq!(rendered.html, "before <code>code</code> after");
    }

    #[test]
    fn unbalanced_and_nested_fences_are_safe() {
        let rendered = render_markdown("```rust\n**not bold\n");
        assert!(rendered
            .html
            .contains("<pre><code class=\"language-rust\">"));
        assert!(rendered.html.contains("**not bold\n</code></pre>"));

        let nested = render_markdown("```\nouter\n```\n```rust\ninner\n```");
        assert_eq!(nested.html.matches("<pre>").count(), 2);
        assert_eq!(nested.html.matches("</pre>").count(), 2);
    }

    #[test]
    fn each_split_chunk_renders_as_independent_html() {
        let source = format!("before\n```rust\n{}\n```\nafter", "x\n".repeat(5000));
        let chunks = crate::sanitize::split_for_channel(&source, 3900);
        assert!(chunks.len() > 1);
        for chunk in chunks {
            let rendered = render_markdown(&chunk);
            assert_eq!(
                rendered.html.matches("<pre>").count(),
                rendered.html.matches("</pre>").count()
            );
            assert_eq!(
                rendered.html.matches("<code").count(),
                rendered.html.matches("</code>").count()
            );
        }
    }
}
