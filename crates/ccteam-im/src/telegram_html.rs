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
    /// Whether the rendered text contains at least one non-whitespace character.
    pub has_non_whitespace: bool,
}

#[derive(Default)]
struct Fragment {
    html: String,
    text_utf16_len: usize,
    has_non_whitespace: bool,
    contains_code: bool,
}

/// Render the supported Markdown subset as Telegram HTML.
pub fn render_markdown(input: &str) -> RenderedMarkdown {
    render_markdown_at_depth(input, 0)
}

const MAX_RENDER_DEPTH: usize = 16;

fn render_markdown_at_depth(input: &str, depth: usize) -> RenderedMarkdown {
    if depth > MAX_RENDER_DEPTH {
        let mut out = Fragment::default();
        append_escaped_text(&mut out, input);
        return RenderedMarkdown {
            html: out.html,
            text_utf16_len: out.text_utf16_len,
            has_non_whitespace: out.has_non_whitespace,
        };
    }

    let lines: Vec<&str> = input.split_inclusive('\n').collect();
    let mut out = Fragment::default();
    let mut index = 0;

    while index < lines.len() {
        let raw_line = lines[index];
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);

        if let Some((fence_char, fence_len)) = fence_marker(line) {
            let language = fence_language(line, fence_len);
            let mut body = String::new();
            let mut closing = None;
            let mut cursor = index + 1;
            while cursor < lines.len() {
                let candidate = lines[cursor];
                let candidate_line = candidate.strip_suffix('\n').unwrap_or(candidate);
                if is_fence_closer(candidate_line, fence_char, fence_len) {
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

        if let Some((table, next_index, has_newline)) = render_table(&lines, index) {
            append_fragment(&mut out, table);
            if has_newline {
                append_escaped_text(&mut out, "\n");
            }
            index = next_index;
            continue;
        }

        // Review round 2 — `im_views::render_status`/`render_sessions`
        // splice a literal `<blockquote expandable>…</blockquote>` (a
        // Telegram Rich Message extension our own `> ` syntax below has no
        // way to produce) straight into `.markdown`. Left alone, the
        // classic HTML fallback (this function) would treat the raw tag as
        // plain text and escape it into visible `&lt;blockquote…`. Render
        // it as the same tag instead — Telegram's classic `parse_mode=HTML`
        // accepts `<blockquote>`/`<blockquote expandable>` too — rather
        // than stripping it to bare text, so the collapsible-detail UX
        // survives the fallback path as well as the Rich Message one.
        if let Some((expandable, first_content)) = strip_html_blockquote_open(line) {
            let mut quote = Fragment::default();
            let mut cursor = index;
            let mut content = first_content;
            loop {
                let raw_line = lines[cursor];
                let has_newline = raw_line.ends_with('\n');
                if let Some(before_close) = content.strip_suffix("</blockquote>") {
                    append_fragment(&mut quote, render_inline(before_close, true, true));
                    cursor += 1;
                    break;
                }
                append_fragment(&mut quote, render_inline(content, true, true));
                if has_newline {
                    append_escaped_text(&mut quote, "\n");
                }
                cursor += 1;
                if cursor >= lines.len() {
                    break;
                }
                let next_raw = lines[cursor];
                content = next_raw.strip_suffix('\n').unwrap_or(next_raw);
            }
            let mut html = String::from(if expandable {
                "<blockquote expandable>"
            } else {
                "<blockquote>"
            });
            html.push_str(&quote.html);
            html.push_str("</blockquote>");
            append_fragment(
                &mut out,
                Fragment {
                    html,
                    text_utf16_len: quote.text_utf16_len,
                    has_non_whitespace: quote.has_non_whitespace,
                    contains_code: quote.contains_code,
                },
            );
            index = cursor;
            continue;
        }

        if is_blockquote_line(line) {
            let mut quote_source = String::new();
            let mut cursor = index;
            while cursor < lines.len() {
                let raw_quote = lines[cursor];
                let quote_line = raw_quote.strip_suffix('\n').unwrap_or(raw_quote);
                let Some(content) = strip_blockquote(quote_line) else {
                    break;
                };
                quote_source.push_str(content);
                if raw_quote.ends_with('\n') {
                    quote_source.push('\n');
                }
                cursor += 1;
            }
            for part in render_blockquote_parts(&quote_source) {
                append_fragment(&mut out, part);
            }
            index = cursor;
            continue;
        }

        if let Some((indent, content)) = unordered_item(line) {
            if fence_marker(content).is_some() {
                let (code, next_index, has_newline) =
                    render_list_fence(&lines, index, indent, content, depth)
                        .expect("fence marker checked above");
                let mut item = Fragment::default();
                append_escaped_text(&mut item, indent);
                append_escaped_text(&mut item, "• ");
                append_fragment(&mut item, code);
                if has_newline {
                    append_escaped_text(&mut item, "\n");
                }
                append_fragment(&mut out, item);
                index = next_index;
                continue;
            }
        }

        if let Some((indent, number, content)) = ordered_item(line) {
            if fence_marker(content).is_some() {
                let (code, next_index, has_newline) =
                    render_list_fence(&lines, index, indent, content, depth)
                        .expect("fence marker checked above");
                let mut item = Fragment::default();
                append_escaped_text(&mut item, indent);
                append_escaped_text(&mut item, number);
                append_escaped_text(&mut item, ". ");
                append_fragment(&mut item, code);
                if has_newline {
                    append_escaped_text(&mut item, "\n");
                }
                append_fragment(&mut out, item);
                index = next_index;
                continue;
            }
        }

        let line_fragment = if let Some(content) = heading_content(line) {
            let inner = render_inline(content, true, true);
            if inner.contains_code {
                inner
            } else {
                wrap("b", inner)
            }
        } else if let Some((indent, content)) = unordered_item(line) {
            let mut item = Fragment::default();
            append_escaped_text(&mut item, indent);
            append_escaped_text(&mut item, "• ");
            append_fragment(&mut item, render_inline(content, true, true));
            item
        } else if let Some((indent, number, content)) = ordered_item(line) {
            let mut item = Fragment::default();
            append_escaped_text(&mut item, indent);
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
        has_non_whitespace: out.has_non_whitespace,
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
        has_non_whitespace: body.chars().any(|ch| !ch.is_whitespace()),
        contains_code: true,
    }
}

fn render_table(lines: &[&str], start: usize) -> Option<(Fragment, usize, bool)> {
    let header = table_row(lines.get(start)?.strip_suffix('\n').unwrap_or(lines[start]))?;
    let separator = table_row(
        lines
            .get(start + 1)?
            .strip_suffix('\n')
            .unwrap_or(lines[start + 1]),
    )?;
    if header.len() != separator.len()
        || header.is_empty()
        || !separator.iter().all(|cell| is_table_separator_cell(cell))
    {
        return None;
    }

    let mut rows = vec![header];
    let mut index = start + 2;
    while let Some(raw_line) = lines.get(index) {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let Some(row) = table_row(line) else {
            break;
        };
        if row.len() != rows[0].len() {
            break;
        }
        rows.push(row);
        index += 1;
    }

    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| table_cell_text(cell)).collect())
        .collect();
    let widths: Vec<usize> = (0..cells[0].len())
        .map(|column| {
            cells
                .iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();

    let mut body = String::new();
    append_table_row(&mut body, &cells[0], &widths);
    trim_table_row_end(&mut body);
    body.push('\n');
    for (column, width) in widths.iter().enumerate() {
        if column > 0 {
            body.push_str("-+-");
        }
        body.extend(std::iter::repeat_n('-', *width));
    }
    for row in &cells[1..] {
        body.push('\n');
        append_table_row(&mut body, row, &widths);
        trim_table_row_end(&mut body);
    }

    let fragment = Fragment {
        html: format!("<pre>{}</pre>", escape_html(&body)),
        text_utf16_len: utf16_len(&body),
        has_non_whitespace: body.chars().any(|ch| !ch.is_whitespace()),
        contains_code: true,
    };
    let has_newline = lines[index - 1].ends_with('\n');
    Some((fragment, index, has_newline))
}

fn append_table_row(body: &mut String, row: &[String], widths: &[usize]) {
    for (column, cell) in row.iter().enumerate() {
        if column > 0 {
            body.push_str(" | ");
        }
        body.push_str(cell);
        body.extend(std::iter::repeat_n(
            ' ',
            widths[column] - cell.chars().count(),
        ));
    }
}

fn trim_table_row_end(body: &mut String) {
    let trimmed_len = body.trim_end_matches(' ').len();
    body.truncate(trimmed_len);
}

fn table_row(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let trimmed = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap_or(trimmed));
    Some(trimmed.split('|').map(str::trim).collect())
}

fn is_table_separator_cell(cell: &str) -> bool {
    let cell = cell.trim();
    let without_left = cell.strip_prefix(':').unwrap_or(cell);
    let without_edges = without_left.strip_suffix(':').unwrap_or(without_left);
    without_edges.len() >= 3 && without_edges.chars().all(|ch| ch == '-')
}

fn table_cell_text(cell: &str) -> String {
    let rendered = render_inline(cell, true, true);
    let mut plain = strip_rendered_tags(&rendered.html);
    if plain.chars().count() > 24 {
        plain = plain.chars().take(23).collect();
        plain.push('…');
    }
    plain
}

fn strip_rendered_tags(input: &str) -> String {
    let mut out = String::new();
    let mut index = 0;
    while index < input.len() {
        let tail = &input[index..];
        if tail.starts_with('<') {
            if let Some(end) = tail.find('>') {
                index += end + 1;
                continue;
            }
        }
        if let Some((entity, character)) = [
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&amp;", '&'),
            ("&quot;", '"'),
        ]
        .into_iter()
        .find(|(entity, _)| tail.starts_with(entity))
        {
            out.push(character);
            index += entity.len();
            continue;
        }
        let ch = tail.chars().next().expect("valid char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn render_inline(input: &str, allow_code: bool, allow_links: bool) -> Fragment {
    let mut out = Fragment::default();
    let mut index = 0;
    while index < input.len() {
        if input.as_bytes()[index] == b'\\' {
            let next = input[index + 1..].chars().next();
            if next.is_some_and(|ch| ch.is_ascii_punctuation()) {
                let escaped = next.expect("checked above");
                append_escaped_text(&mut out, &escaped.to_string());
                index += 1 + escaped.len_utf8();
                continue;
            }
            append_escaped_text(&mut out, "\\");
            index += 1;
            continue;
        }

        if allow_code && input.as_bytes()[index] == b'`' {
            if let Some(end) = find_unescaped(input, b'`', index + 1) {
                if end > index + 1 {
                    let code = &input[index + 1..end];
                    let mut fragment = Fragment {
                        html: String::from("<code>"),
                        text_utf16_len: utf16_len(code),
                        has_non_whitespace: code.chars().any(|ch| !ch.is_whitespace()),
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
                    has_non_whitespace: label.has_non_whitespace,
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

        if !can_open_delimiter(input, index, delimiter) {
            append_escaped_text(&mut out, delimiter);
            index += delimiter.len();
            continue;
        }
        let content_start = index + delimiter.len();
        let Some(end) = find_delimiter(input, delimiter, content_start) else {
            append_escaped_text(&mut out, delimiter);
            index += delimiter.len();
            continue;
        };
        if end == content_start || is_identifier_like_underscore_pair(input, index, end, delimiter)
        {
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
        has_non_whitespace: inner.has_non_whitespace,
        contains_code: inner.contains_code,
    }
}

fn append_fragment(out: &mut Fragment, fragment: Fragment) {
    out.html.push_str(&fragment.html);
    out.text_utf16_len += fragment.text_utf16_len;
    out.has_non_whitespace |= fragment.has_non_whitespace;
    out.contains_code |= fragment.contains_code;
}

fn append_escaped_text(out: &mut Fragment, text: &str) {
    out.html.push_str(&escape_html(text));
    out.text_utf16_len += utf16_len(text);
    out.has_non_whitespace |= text.chars().any(|ch| !ch.is_whitespace());
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
    let text = unescape_html_attr(text);
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

fn unescape_html_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let tail = &text[index..];
        let entity = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
        ]
        .into_iter()
        .find(|(entity, _)| tail.starts_with(entity));
        if let Some((entity, ch)) = entity {
            out.push(ch);
            index += entity.len();
        } else {
            let ch = tail.chars().next().expect("valid char boundary");
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// TG-GATE-V2 W7a — `pub(crate)` so the rich-fallback split (telegram.rs)
/// can be tested against the exact fence-line predicate it must never
/// corrupt by appending a `(i/n)` suffix onto the same line.
pub(crate) fn is_fence_line(line: &str) -> bool {
    fence_marker(line).is_some()
}

fn fence_language(line: &str, fence_len: usize) -> Option<&str> {
    let info = line.trim_start().get(fence_len..)?.trim();
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

/// Recognize `im_views`' literal `<blockquote expandable>`/`<blockquote>`
/// opening tag at the start of a line, returning whether it was the
/// expandable variant plus whatever content follows the tag on that same
/// line (the tag and the first content line are glued together with no
/// separating newline in `.markdown`, e.g. `<blockquote expandable>Роль: …`).
fn strip_html_blockquote_open(line: &str) -> Option<(bool, &str)> {
    if let Some(rest) = line.strip_prefix("<blockquote expandable>") {
        Some((true, rest))
    } else {
        line.strip_prefix("<blockquote>").map(|rest| (false, rest))
    }
}

fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start();
    let marker = *trimmed.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = trimmed.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some((marker, length))
}

fn is_fence_closer(line: &str, opening_char: u8, opening_len: usize) -> bool {
    let trimmed = line.trim_start();
    let Some((marker, length)) = fence_marker(line) else {
        return false;
    };
    marker == opening_char && length >= opening_len && trimmed[length..].trim().is_empty()
}

fn render_list_fence(
    lines: &[&str],
    start: usize,
    indent: &str,
    content: &str,
    depth: usize,
) -> Option<(Fragment, usize, bool)> {
    let (opening_char, opening_len) = fence_marker(content)?;
    let required_indent = indent.len() + 2;
    let mut source = content.to_owned();
    if lines[start].ends_with('\n') {
        source.push('\n');
    }

    let mut cursor = start + 1;
    let mut closed = false;
    while let Some(raw_line) = lines.get(cursor) {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let is_indented = line.len() >= required_indent
            && line.as_bytes()[..required_indent]
                .iter()
                .all(|byte| *byte == b' ');
        let candidate = if is_indented {
            &line[required_indent..]
        } else if line.trim().is_empty() {
            ""
        } else if fence_marker(line).is_some() {
            line.trim_start()
        } else {
            break;
        };
        if is_fence_closer(candidate, opening_char, opening_len) {
            cursor += 1;
            closed = true;
            break;
        }
        source.push_str(candidate);
        if raw_line.ends_with('\n') {
            source.push('\n');
        }
        cursor += 1;
    }

    let rendered = render_markdown_at_depth(&source, depth + 1);
    let contains_code = rendered.html.contains("<pre>") || rendered.html.contains("<code>");
    Some((
        Fragment {
            html: rendered.html,
            text_utf16_len: rendered.text_utf16_len,
            has_non_whitespace: rendered.has_non_whitespace,
            contains_code,
        },
        cursor,
        closed && lines[cursor - 1].ends_with('\n'),
    ))
}

fn render_blockquote_parts(source: &str) -> Vec<Fragment> {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut parts = Vec::new();
    let mut quote = Fragment::default();
    let mut index = 0;

    while index < lines.len() {
        let raw_line = lines[index];
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        if let Some((fence_char, fence_len)) = fence_marker(line) {
            if !quote.html.is_empty() {
                parts.push(wrap("blockquote", std::mem::take(&mut quote)));
            }
            let language = fence_language(line, fence_len);
            let mut body = String::new();
            let mut closing = None;
            let mut cursor = index + 1;
            while cursor < lines.len() {
                let candidate = lines[cursor];
                let candidate_line = candidate.strip_suffix('\n').unwrap_or(candidate);
                if is_fence_closer(candidate_line, fence_char, fence_len) {
                    closing = Some(candidate.ends_with('\n'));
                    break;
                }
                body.push_str(candidate);
                cursor += 1;
            }
            let mut code = render_code_block(&body, language);
            if closing == Some(true) {
                append_escaped_text(&mut code, "\n");
            }
            parts.push(code);
            index = closing.map_or(lines.len(), |_| cursor + 1);
            continue;
        }

        if let Some((table, next_index, has_newline)) = render_table(&lines, index) {
            if !quote.html.is_empty() {
                parts.push(wrap("blockquote", std::mem::take(&mut quote)));
            }
            parts.push(table);
            if has_newline {
                let mut newline = Fragment::default();
                append_escaped_text(&mut newline, "\n");
                parts.push(newline);
            }
            index = next_index;
            continue;
        }

        append_fragment(&mut quote, render_inline(line, true, true));
        if raw_line.ends_with('\n') {
            append_escaped_text(&mut quote, "\n");
        }
        index += 1;
    }

    if !quote.html.is_empty() {
        parts.push(wrap("blockquote", quote));
    }
    parts
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

fn unordered_item(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .map(|content| (indent, content))
}

fn ordered_item(line: &str) -> Option<(&str, &str, &str)> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let dot = trimmed.find(". ")?;
    if dot > 0 && trimmed[..dot].bytes().all(|b| b.is_ascii_digit()) {
        Some((indent, &trimmed[..dot], &trimmed[dot + 2..]))
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
            && can_close_delimiter(input, index, delimiter)
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

fn can_open_delimiter(input: &str, index: usize, delimiter: &str) -> bool {
    let next = input[index + delimiter.len()..].chars().next();
    match delimiter.as_bytes()[0] {
        b'*' => {
            next.is_some_and(|ch| !ch.is_whitespace()) && !is_path_wildcard(input, index, delimiter)
        }
        b'~' => next.is_some_and(|ch| !ch.is_whitespace()),
        b'_' => {
            !input[..index]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
                && next.is_some_and(|ch| !ch.is_whitespace())
        }
        _ => false,
    }
}

fn can_close_delimiter(input: &str, index: usize, delimiter: &str) -> bool {
    let previous = input[..index].chars().next_back();
    let next = input[index + delimiter.len()..].chars().next();
    match delimiter.as_bytes()[0] {
        b'*' => {
            previous.is_some_and(|ch| !ch.is_whitespace())
                && !is_path_wildcard(input, index, delimiter)
        }
        b'~' => previous.is_some_and(|ch| !ch.is_whitespace()),
        b'_' => {
            previous.is_some_and(|ch| !ch.is_whitespace())
                && !next.is_some_and(char::is_alphanumeric)
        }
        _ => false,
    }
}

fn is_path_wildcard(input: &str, index: usize, delimiter: &str) -> bool {
    delimiter.starts_with('*')
        && (input[..index].ends_with('/')
            || input[index + delimiter.len()..].starts_with('/')
            || input[index + delimiter.len()..].starts_with('.'))
}

fn is_identifier_like_underscore_pair(
    input: &str,
    start: usize,
    end: usize,
    delimiter: &str,
) -> bool {
    if delimiter != "_" && delimiter != "__" {
        return false;
    }
    let content = &input[start + delimiter.len()..end];
    let identifier = content.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
    identifier
        && ((delimiter == "_" && content.contains('_'))
            || (delimiter == "__" && is_known_dunder_name(content)))
}

// Keep common source identifiers such as `__init__` literal, while retaining
// the documented `__bold__` strong-emphasis form for ordinary prose.
fn is_known_dunder_name(content: &str) -> bool {
    matches!(
        content,
        "init"
            | "new"
            | "del"
            | "repr"
            | "str"
            | "bytes"
            | "format"
            | "lt"
            | "le"
            | "eq"
            | "ne"
            | "gt"
            | "ge"
            | "hash"
            | "bool"
            | "call"
            | "len"
            | "getitem"
            | "setitem"
            | "delitem"
            | "iter"
            | "next"
            | "contains"
            | "enter"
            | "exit"
            | "aenter"
            | "aexit"
            | "name"
            | "module"
            | "qualname"
            | "doc"
            | "all"
            | "version"
            | "slots"
            | "dict"
            | "weakref"
    )
}

fn is_escaped(input: &str, index: usize) -> bool {
    let slash_count = input[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count();
    slash_count % 2 == 1
}

fn parse_link(input: &str, start: usize) -> Option<(usize, usize, usize)> {
    let label_end = find_unescaped(input, b']', start + 1)?;
    if input.as_bytes().get(label_end + 1) != Some(&b'(') {
        return None;
    }
    let url_start = label_end + 2;
    let mut depth = 0;
    let mut index = url_start;
    while index < input.len() {
        let ch = input[index..].chars().next().expect("valid char boundary");
        if ch == '\\' {
            let next = input[index + ch.len_utf8()..].chars().next();
            if let Some(next) = next.filter(|ch| ch.is_ascii_punctuation()) {
                index += ch.len_utf8() + next.len_utf8();
            } else {
                index += ch.len_utf8();
            }
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => {
                return (index > url_start).then_some((label_end, url_start, index));
            }
            ')' => depth -= 1,
            _ => {}
        }
        index += ch.len_utf8();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{render_markdown, render_markdown_at_depth, MAX_RENDER_DEPTH};

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
        let rendered = render_markdown("**bold *italic* and** [link](https://e.test?a=1&amp;b=2)");
        assert_eq!(
            rendered.html,
            "<b>bold <i>italic</i> and</b> <a href=\"https://e.test?a=1&amp;b=2\">link</a>"
        );
    }

    #[test]
    fn parses_balanced_link_parentheses_and_keeps_unclosed_links_literal() {
        assert_eq!(
            render_markdown("[x](https://e.test/a_(b)?a=1&b=2)").html,
            "<a href=\"https://e.test/a_(b)?a=1&amp;b=2\">x</a>"
        );
        assert_eq!(
            render_markdown("[x](https://e.test/a_(b)").html,
            "[x](https://e.test/a_(b)"
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

    /// Review round 2 — `im_views::render_status`'s literal
    /// `<blockquote expandable>…</blockquote>` (glued to its neighboring
    /// content lines with no separating newline) must render as a real
    /// `<blockquote expandable>` tag on the classic HTML fallback path,
    /// not escape into visible `&lt;blockquote…` text.
    #[test]
    fn renders_the_expandable_html_blockquote_im_views_splices_in() {
        let rendered = render_markdown(
            "header line\n<blockquote expandable>Роль: reviewer\n**bold** & <tag></blockquote>",
        );
        assert_eq!(
            rendered.html,
            "header line\n<blockquote expandable>Роль: reviewer\n<b>bold</b> &amp; &lt;tag&gt;</blockquote>"
        );
    }

    #[test]
    fn renders_the_plain_html_blockquote_without_the_expandable_attribute() {
        let rendered = render_markdown("<blockquote>one line only</blockquote>");
        assert_eq!(rendered.html, "<blockquote>one line only</blockquote>");
    }

    #[test]
    fn preserves_nested_list_indentation() {
        assert_eq!(
            render_markdown("- top\n  - nested\n    1. deeper").html,
            "• top\n  • nested\n    1. deeper"
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
    fn preserves_intraword_emphasis_candidates() {
        for input in [
            "run src/**/*.rs and tests/**/*.rs",
            "session s12 max_live=4 send_http_ms=12",
            "fn foo_bar_baz()",
            "5 * 3 = 15 and 2 * 4 = 8",
            "see https://x.test/a_b_c/d for docs",
            "__init__ and _private_var_",
            "日本語_変数_名",
        ] {
            assert_eq!(render_markdown(input).html, input);
        }
        assert_eq!(render_markdown("x__y__z").html, "x__y__z");
    }

    #[test]
    fn still_formats_flanked_emphasis() {
        for (input, expected) in [
            ("**bold**", "<b>bold</b>"),
            ("*it*", "<i>it</i>"),
            ("_it_", "<i>it</i>"),
            ("a **b** c", "a <b>b</b> c"),
            ("__bold__", "<b>bold</b>"),
        ] {
            assert_eq!(render_markdown(input).html, expected);
        }
    }

    #[test]
    fn code_is_not_nested_inside_inline_formatting() {
        let rendered = render_markdown("**before `code` after**");
        assert_eq!(rendered.html, "before <code>code</code> after");
    }

    #[test]
    fn backslash_follows_commonmark_escape_rule() {
        assert_eq!(render_markdown(r#"C:\Users\ops"#).html, r#"C:\Users\ops"#);
        assert_eq!(render_markdown(r#"a\\b"#).html, r#"a\b"#);
        assert_eq!(render_markdown(r#"\*not italic\*"#).html, "*not italic*");
    }

    #[test]
    fn blockquotes_allow_inline_code_and_links() {
        assert_eq!(
            render_markdown("> `code` [link](https://e.test?a=1&amp;b=2)").html,
            "<blockquote><code>code</code> <a href=\"https://e.test?a=1&amp;b=2\">link</a></blockquote>"
        );
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
    fn fence_closer_needs_matching_length_and_no_trailing_text() {
        let rendered = render_markdown("```\n```oops\nx\n```\n");
        assert!(rendered.html.contains("```oops\nx\n"));

        let rendered = render_markdown("````\n```\nx\n```\n````");
        assert_eq!(rendered.html, "<pre><code>```\nx\n```\n</code></pre>");
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

    #[test]
    fn renders_gfm_tables_as_padded_plain_text_pre() {
        let rendered = render_markdown(
            "| Name | Status |\n| :--- | ---: |\n| **Alice** | [ok](https://e.test?a=1&b=2) |\n| Bob | `pending` |\n",
        );
        assert_eq!(
            rendered.html,
            "<pre>Name  | Status\n------+--------\nAlice | ok\nBob   | pending</pre>\n"
        );
        assert!(!rendered.html.contains("<a "));
        assert!(!rendered.html.contains("<b>"));
    }

    #[test]
    fn caps_gfm_table_cells_without_emitting_markup() {
        let rendered = render_markdown(
            "| Long | Value |\n| --- | --- |\n| abcdefghijklmnopqrstuvwxyz | <tag> |\n",
        );
        assert_eq!(
            rendered.html,
            "<pre>Long                     | Value\n-------------------------+------\nabcdefghijklmnopqrstuvw… | &lt;tag&gt;</pre>\n"
        );
    }

    #[test]
    fn renders_tilde_fences_and_closes_unterminated_fences_at_eof() {
        assert_eq!(
            render_markdown("~~~~rust\n```\nvalue < 1\n~~~~").html,
            "<pre><code class=\"language-rust\">```\nvalue &lt; 1\n</code></pre>"
        );
        assert_eq!(
            render_markdown("~~~\nvalue < 1").html,
            "<pre><code>value &lt; 1</code></pre>"
        );
    }

    #[test]
    fn renders_fences_inside_blockquotes_and_list_items() {
        assert_eq!(
            render_markdown("> ````\n> ```\n> quoted < 1\n> ````").html,
            "<pre><code>```\nquoted &lt; 1\n</code></pre>"
        );
        assert_eq!(
            render_markdown("- ~~~rust\n  list < 1\n  ~~~").html,
            "• <pre><code class=\"language-rust\">list &lt; 1\n</code></pre>"
        );
    }

    #[test]
    fn does_not_nest_blockquotes_for_literal_nested_markers() {
        assert_eq!(
            render_markdown("> > nested\n").html,
            "<blockquote>&gt; nested\n</blockquote>"
        );
    }

    #[test]
    fn bounds_render_recursion_and_handles_deep_quote_input() {
        let input = format!("{}x", "> ".repeat(20_000));
        let rendered = render_markdown(&input);
        assert_eq!(rendered.html.matches("<blockquote>").count(), 1);
        assert_eq!(rendered.html.matches("</blockquote>").count(), 1);
        assert_eq!(
            render_markdown_at_depth("**literal**", MAX_RENDER_DEPTH + 1).html,
            "**literal**"
        );
    }

    #[test]
    fn keeps_fenced_code_outside_blockquote_tags() {
        assert_eq!(
            render_markdown("> before\n> ```\n> code\n> ```\n> after\n").html,
            "<blockquote>before\n</blockquote><pre><code>code\n</code></pre>\n<blockquote>after\n</blockquote>"
        );
    }
}
