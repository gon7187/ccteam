//! Content sanitization for IM → tmux injection.
//!
//! Mirrors `references/oh-my-claudecode/src/notifications/reply-listener.ts`
//! `sanitizeReplyInput()` (see `docs/versions/v0-6-0/wave-2-decisions.md` §4 for
//! the OMC parity contract).
//!
//! Two-layer model:
//!
//! 1. **Content layer** ([`sanitize_reply_input`]) — strips control
//!    chars, bidi overrides, escapes shell metacharacters. Applied
//!    before any further processing.
//! 2. **Tmux layer** ([`sanitize_for_tmux`]) — additionally collapses
//!    newlines into spaces (`tmux send-keys -l` injects literally; an
//!    embedded `\n` would issue an Enter the user didn't intend).

/// Maximum length of a single IM-to-tmux turn after sanitization.
/// V0.6 picks 4096 to match Telegram's per-message ceiling; longer
/// turns are split or truncated by the caller.
pub const MAX_TURN_LEN: usize = 4096;

/// Strip dangerous characters, escape shell metas, normalize the
/// payload for downstream consumption. Direct port of the OMC TS
/// `sanitizeReplyInput()`.
///
/// Pipeline:
/// 1. Strip ASCII control chars (`\x00-\x08`, `\x0b`, `\x0c`,
///    `\x0e-\x1f`, `\x7f`). Keeps `\t`, `\n`, `\r` for now — those are
///    handled by [`sanitize_for_tmux`] when the destination is a tmux
///    pane.
/// 2. Strip Unicode bidi overrides (`U+202A`–`U+202E`,
///    `U+2066`–`U+2069`).
/// 3. Escape `\`, `` ` ``, `$(`, `${` so a payload like `"$(rm -rf /)"`
///    can't trigger command substitution if forwarded into a shell.
/// 4. `trim()`.
pub fn sanitize_reply_input(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let cp = ch as u32;
        // Control chars (keep \t \n \r — newline collapse is the tmux
        // layer's job).
        if matches!(cp, 0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f) {
            continue;
        }
        // Bidi overrides.
        if matches!(cp, 0x202a..=0x202e | 0x2066..=0x2069) {
            continue;
        }
        out.push(ch);
    }
    // Escape shell-substitution sequences. Order matters: `\` first so
    // we don't double-escape the backslashes we add later.
    out = out
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace("$(", "\\$(")
        .replace("${", "\\${");
    out.trim().to_string()
}

/// Stronger variant — runs [`sanitize_reply_input`] then collapses
/// newlines/CR/tab into single spaces. Use this immediately before
/// `tmux send-keys -l <payload>` to prevent literal Enters being
/// pasted into the agent's prompt mid-turn.
pub fn sanitize_for_tmux(text: &str) -> String {
    let mut out = sanitize_reply_input(text);
    out = out.replace(['\r', '\n', '\t'], " ");
    // Collapse runs of whitespace introduced by the line-flattening.
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out.trim().to_string()
}

/// Truncate to [`MAX_TURN_LEN`] (UTF-8 char boundary aware).
pub fn truncate_to_max(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }
    text.chars().take(max_len).collect()
}

/// Sanity-check that a captured tmux pane has non-whitespace content
/// before we inject a turn — empty/whitespace panes are the typical
/// signature of a dead session that we should not paste into. Mirrors
/// OMC `injectReply()` lines 368–374.
pub fn verify_pane_not_empty(pane_capture: &str) -> bool {
    !pane_capture.trim().is_empty()
}

// ---------------------------------------------------------------------
// Outbound message splitting (V0.8.4 P0 / B2).
//
// Some channels cap a single message's length (Telegram: 4096 *UTF-16
// code units*). The gateway is channel-neutral — it asks the channel
// for its ceiling via `Channel::max_message_len()` and, if the content
// overflows, calls [`split_for_channel`] to fan one logical reply into
// ordered sub-messages. The 4096 constant lives only in `telegram.rs`;
// this module knows nothing about any specific platform.
// ---------------------------------------------------------------------

/// UTF-16 code-unit length of `s`. Telegram's message ceiling is
/// expressed in UTF-16 units (a supplementary-plane scalar such as an
/// emoji is a surrogate pair = 2 units), so budgeting by `chars().count()`
/// would under-count and still trip a 400 from the Bot API.
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Units reserved when a code fence straddles a split boundary, to hold
/// the re-open (marker + info + newline) on the next part plus the close
/// on this one. The final size is checked again after balancing.
const FENCE_REOPEN_MARGIN: usize = 24;

/// Split `text` into ordered chunks, each within `max_units` UTF-16 code
/// units, for channels that cap message length. Guarantees:
///
/// - **Lossless for plain text**: concatenating the parts reproduces the
///   original verbatim (no characters dropped, no separators consumed)
///   whenever no code fence crosses a boundary.
/// - **Balanced fences**: a backtick or tilde fence block that crosses a split is
///   closed at the end of one part and re-opened (preserving the
///   language info string) at the start of the next, so every part is
///   valid Markdown on its own.
/// - **Sensible break points**: prefers a paragraph break (`\n\n`), then a
///   line break (`\n`), then whitespace, then a hard character boundary,
///   and never splits a multi-byte char.
///
/// `max_units == 0` is treated as `1` to guarantee forward progress.
pub fn split_for_channel(text: &str, max_units: usize) -> Vec<String> {
    let max_units = max_units.max(1);
    if utf16_len(text) <= max_units {
        return vec![text.to_string()];
    }
    if !text.lines().any(|line| fence_line(line).is_some()) {
        return raw_split(text, max_units);
    }

    let mut budget = max_units.saturating_sub(FENCE_REOPEN_MARGIN).max(1);
    for _ in 0..32 {
        let parts = balance_fences(raw_split(text, budget));
        let over = parts
            .iter()
            .map(|part| utf16_len(part))
            .max()
            .unwrap_or_default()
            .saturating_sub(max_units);
        if over == 0 {
            return parts;
        }
        let next = budget.saturating_sub(over.max(1));
        if next == budget {
            break;
        }
        budget = next;
    }

    // A pathological tiny ceiling can be smaller than the fence wrapper
    // itself. The raw split still guarantees progress and the requested hard
    // ceiling for every representable chunk.
    raw_split(text, max_units)
}

/// [`split_for_channel`], plus a `(i/n)` numbering suffix on every part
/// once a message actually needs more than one — the shared numbering
/// contract both the pre-split classic path and the Telegram Rich
/// Message fallback split follow (TG-GATE-V2).
///
/// The suffix is appended as `\n\n(i/n)` — a full blank line, then the
/// marker on its own line — never glued onto the part's last content
/// line. That matters because [`split_for_channel`] can hand back a part
/// whose last line is a closing ` ``` ` fence (balanced by
/// [`balance_fences`]); gluing `(i/n)` onto that line would corrupt the
/// fence delimiter itself (`` ```(1/3) `` is not a valid fence). A blank
/// line in between guarantees the marker always reads as its own
/// trailing block, fence or no fence (see
/// `telegram_html::is_fence_line`, which every part's last line is
/// checked against in tests).
///
/// The suffix's own width must be reserved from `limit` BEFORE splitting
/// — appending it after the fact could push a part back over the
/// ceiling. But the suffix's width depends on `digits(n)`, and `n` isn't
/// known until we've already split. Resolved by iterating: guess
/// `digits`, reserve for it, split, check whether the actual part count
/// still fits that many digits. Reserving more digits only shrinks the
/// budget, which can only grow (never shrink) the part count, so
/// `digits` is monotonically non-decreasing across iterations and the
/// loop always converges (typically 1 iteration; 2 across a `9 → 10`
/// part-count boundary).
pub fn split_for_channel_numbered(text: &str, limit: usize) -> Vec<String> {
    if utf16_len(text) <= limit {
        return vec![text.to_string()];
    }
    let mut digits = 1usize;
    let parts = loop {
        // Width of `\n\n(i/n)` when both `i` and `n` have `digits` digits
        // (the widest `i` can ever be, since `i <= n`): 2 newlines + 2
        // parens + 1 slash + 2×`digits`.
        let reserve = 2 * digits + 5;
        let budget = limit.saturating_sub(reserve).max(1);
        let candidate = split_for_channel(text, budget);
        let needed = candidate.len().to_string().len();
        if needed <= digits {
            break candidate;
        }
        digits = needed;
    };
    if parts.len() <= 1 {
        return parts;
    }
    let total = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(idx, part)| format!("{part}\n\n({}/{total})", idx + 1))
        .collect()
}

/// Split Rich Message Markdown into independently valid numbered parts.
/// Tables, quotes and details are atomic. If one of those exceeds the ceiling,
/// it is cut only at line boundaries: the documented Rich Message ceiling wins.
pub fn split_rich_markdown_numbered(markdown: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    if markdown.len() <= limit {
        return vec![markdown.to_string()];
    }
    let mut digits = 1usize;
    let parts = loop {
        let reserve = 2 * digits + 5;
        let mut budget = limit.saturating_sub(reserve).max(1);
        let candidate = loop {
            let candidate = split_rich_raw(markdown, budget);
            let total = candidate.len();
            let over = candidate
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    part.len()
                        .saturating_add(if part.ends_with('\n') { 1 } else { 2 })
                        .saturating_add((index + 1).to_string().len() + total.to_string().len() + 3)
                })
                .max()
                .unwrap_or_default()
                .saturating_sub(limit);
            if over == 0 {
                break candidate;
            }
            let next = budget.saturating_sub(over.max(1));
            if next == budget {
                break candidate;
            }
            budget = next;
        };
        if candidate.len().to_string().len() <= digits {
            break candidate;
        }
        digits = candidate.len().to_string().len();
    };
    if parts.len() == 1 {
        return parts;
    }
    let total = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            let separator = if part.ends_with('\n') { "\n" } else { "\n\n" };
            format!("{part}{separator}({}/{total})", i + 1)
        })
        .collect()
}

/// Split a rich message once and keep a separate plain fallback for each
/// resulting row. The plain chunks use the same outer numbering and never
/// copy the Markdown payload into `SendMessage::content`.
pub fn split_rich_markdown_with_plain(
    markdown: &str,
    plain: &str,
    limit: usize,
) -> Vec<(String, String)> {
    let rich_parts = split_rich_markdown_numbered(markdown, limit);
    if rich_parts.len() == 1 {
        return vec![(rich_parts[0].clone(), plain.to_string())];
    }
    let total = rich_parts.len();
    split_plain_evenly(plain, total)
        .into_iter()
        .enumerate()
        .zip(rich_parts)
        .map(|((index, mut fallback), rich)| {
            fallback.push_str(&format!("\n\n({}/{total})", index + 1));
            (rich, fallback)
        })
        .collect()
}

fn split_rich_raw(markdown: &str, budget: usize) -> Vec<String> {
    let lines: Vec<&str> = markdown.split_inclusive('\n').collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let bare = lines[i].trim_end_matches('\n');
        let mut end = i + 1;
        if let Some(opening) = fence_line(bare) {
            while end < lines.len() {
                let line = lines[end].trim_end_matches('\n');
                end += 1;
                if is_fence_close(line, &opening) {
                    break;
                }
            }
        } else if bare.trim_start().starts_with("<details") {
            while end < lines.len() && !lines[end - 1].contains("</details>") {
                end += 1;
            }
        } else if bare.trim_start().starts_with("<blockquote") {
            while end < lines.len() && !lines[end - 1].contains("</blockquote>") {
                end += 1;
            }
        } else if bare.trim_start().starts_with('>') {
            while end < lines.len() && lines[end].trim_start().starts_with('>') {
                end += 1;
            }
        } else if end < lines.len()
            && bare.contains('|')
            && is_table_separator(lines[end].trim_end_matches('\n'))
        {
            end += 1;
            while end < lines.len() && lines[end].contains('|') {
                end += 1;
            }
        }
        blocks.push(lines[i..end].concat());
        i = end;
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    for block in blocks {
        if block.len() > budget {
            if !current.is_empty() {
                parts.push(current);
                current = String::new();
            }
            if fence_line(block.lines().next().unwrap_or_default()).is_some() {
                parts.extend(split_rich_fence(&block, budget));
            } else {
                parts.extend(split_rich_lines(&block, budget));
            }
        } else if current.len() + block.len() > budget && !current.is_empty() {
            parts.push(current);
            current = block;
        } else {
            current.push_str(&block);
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn is_table_separator(line: &str) -> bool {
    let cells: Vec<_> = line.trim().trim_matches('|').split('|').collect();
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim();
            !cell.is_empty() && cell.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
        })
}

fn split_rich_lines(block: &str, budget: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for line in block.split_inclusive('\n') {
        if line.len() > budget {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            parts.extend(raw_split_bytes(line, budget));
        } else {
            if current.len() + line.len() > budget && !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn split_rich_fence(block: &str, budget: usize) -> Vec<String> {
    let mut lines = block.split_inclusive('\n');
    let opener = lines.next().unwrap_or_default();
    let Some(opening) = fence_line(opener.trim_end_matches('\n')) else {
        return split_rich_lines(block, budget);
    };
    let open_line = if opener.ends_with('\n') {
        opener.to_string()
    } else {
        format!("{opener}\n")
    };
    let close_line = fence_close_line(&opening);
    if open_line.len() + close_line.len() + 1 > budget {
        return raw_split_bytes(block, budget);
    }
    let mut parts = Vec::new();
    let mut current = open_line.clone();
    for line in lines {
        if is_fence_close(line.trim_end_matches('\n'), &opening) {
            continue;
        }
        let room = budget - open_line.len() - close_line.len() - 1;
        for chunk in raw_split_bytes(line, room.max(1)) {
            if current.len() + chunk.len() + close_line.len() + 1 > budget && current != open_line {
                finish_fence_part(&mut parts, &mut current, &close_line);
                current = open_line.clone();
            }
            current.push_str(&chunk);
        }
    }
    finish_fence_part(&mut parts, &mut current, &close_line);
    parts
}

fn finish_fence_part(parts: &mut Vec<String>, current: &mut String, close_line: &str) {
    if !current.ends_with('\n') {
        current.push('\n');
    }
    current.push_str(close_line);
    parts.push(std::mem::take(current));
}

/// Greedy length-budgeted split that preserves every byte (no fence
/// awareness). Each returned chunk is `<= budget` UTF-16 units except a
/// pathological lone char wider than `budget`, which is emitted whole to
/// guarantee progress.
fn raw_split(text: &str, budget: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if utf16_len(rest) <= budget {
            parts.push(rest.to_string());
            break;
        }
        let cut = pick_cut(rest, budget);
        parts.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    parts
}

/// Byte-budgeted counterpart used by Rich Message Markdown. Rich Telegram
/// parts are capped in UTF-8 bytes, while classic Telegram parts use UTF-16.
fn raw_split_bytes(text: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(1);
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.len() <= budget {
            parts.push(rest.to_string());
            break;
        }
        let cut = pick_byte_cut(rest, budget);
        parts.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    parts
}

fn pick_byte_cut(s: &str, budget: usize) -> usize {
    let min_fill = budget / 2;
    let mut hard_cut = 0;
    let mut first_char_end = 0;
    let mut last_line = 0;
    let mut last_ws = 0;
    for (index, ch) in s.char_indices() {
        let end = index + ch.len_utf8();
        if first_char_end == 0 {
            first_char_end = end;
        }
        if end > budget {
            break;
        }
        hard_cut = end;
        if end >= min_fill {
            if ch == '\n' {
                last_line = end;
            } else if ch.is_whitespace() {
                last_ws = end;
            }
        }
    }
    if last_line > 0 {
        last_line
    } else if last_ws > 0 {
        last_ws
    } else if hard_cut > 0 {
        hard_cut
    } else {
        first_char_end
    }
}

/// Byte index (always on a char boundary, always `> 0`) at which to cut
/// `s` so the left part fits `budget` UTF-16 units, preferring the
/// latest paragraph > line > whitespace boundary in the *second half* of
/// the budget window (so we neither make tiny parts nor overflow).
fn pick_cut(s: &str, budget: usize) -> usize {
    let min_fill = budget / 2;
    let mut units = 0usize;
    let mut hard_cut = 0usize;
    let mut first_char_end = 0usize;
    let mut last_para = 0usize;
    let mut last_line = 0usize;
    let mut last_ws = 0usize;
    for (i, ch) in s.char_indices() {
        let end = i + ch.len_utf8();
        if first_char_end == 0 {
            first_char_end = end;
        }
        let w = ch.len_utf16();
        if units + w > budget {
            break;
        }
        units += w;
        hard_cut = end;
        if units >= min_fill {
            if ch == '\n' {
                last_line = end;
                if s[..end].ends_with("\n\n") {
                    last_para = end;
                }
            } else if ch.is_whitespace() {
                last_ws = end;
            }
        }
    }
    let cut = if last_para > 0 {
        last_para
    } else if last_line > 0 {
        last_line
    } else if last_ws > 0 {
        last_ws
    } else {
        hard_cut
    };
    // `cut == 0` only when even the first char exceeds `budget`; take it
    // whole so we always advance.
    if cut == 0 {
        first_char_end
    } else {
        cut
    }
}

/// Walk `parts` left-to-right; whenever a part ends inside an open code
/// fence, append a matching close and re-open it (with its language info)
/// at the head of the next part.
fn balance_fences(parts: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(parts.len());
    let mut carry: Option<FenceState> = None;
    for part in parts {
        let mut s = String::new();
        if let Some(opening) = &carry {
            s.push_str(&fence_open_line(opening));
            s.push('\n');
        }
        s.push_str(&part);
        if let Some(opening) = fence_state(&s) {
            if !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&fence_close_line(&opening));
            carry = Some(opening);
        } else {
            carry = None;
        }
        out.push(s);
    }
    out
}

/// Whether `s` ends inside an open backtick or tilde fence, including a
/// fence nested in a blockquote. Inline backticks are ignored.
#[derive(Debug, Clone)]
struct FenceState {
    prefix: String,
    marker: crate::telegram_html::FenceMarker,
    info: String,
}

fn fence_line(line: &str) -> Option<FenceState> {
    let mut offset = 0;
    loop {
        while line[offset..].starts_with(' ') {
            offset += 1;
        }
        if line[offset..].starts_with('>') {
            offset += 1;
            if line[offset..].starts_with(' ') {
                offset += 1;
            }
            continue;
        }
        break;
    }
    let marker = crate::telegram_html::fence_marker(&line[offset..])?;
    let info = line[offset + marker.len..].trim().to_string();
    Some(FenceState {
        prefix: line[..offset].to_string(),
        marker,
        info,
    })
}

fn is_fence_close(line: &str, opening: &FenceState) -> bool {
    let Some(candidate) = fence_line(line) else {
        return false;
    };
    candidate.prefix == opening.prefix
        && crate::telegram_html::is_fence_closer(
            line[opening.prefix.len()..].trim_end_matches('\n'),
            opening.marker,
        )
}

fn fence_open_line(opening: &FenceState) -> String {
    format!(
        "{}{}{}",
        opening.prefix,
        (opening.marker.marker as char)
            .to_string()
            .repeat(opening.marker.len),
        if opening.info.is_empty() {
            String::new()
        } else {
            opening.info.clone()
        }
    )
}

fn fence_close_line(opening: &FenceState) -> String {
    format!(
        "{}{}",
        opening.prefix,
        (opening.marker.marker as char)
            .to_string()
            .repeat(opening.marker.len)
    )
}

fn fence_state(s: &str) -> Option<FenceState> {
    let mut open: Option<FenceState> = None;
    for line in s.lines() {
        if let Some(opening) = open.as_ref() {
            if is_fence_close(line, opening) {
                open = None;
            }
        } else if let Some(opening) = fence_line(line) {
            open = Some(opening);
        }
    }
    open
}

fn split_plain_evenly(text: &str, count: usize) -> Vec<String> {
    if count <= 1 {
        return vec![text.to_string()];
    }
    let mut result = Vec::with_capacity(count);
    let mut start = 0;
    for index in 0..count {
        if index + 1 == count {
            result.push(text[start..].to_string());
            break;
        }
        let remaining_parts = count - index;
        let remaining = text.len() - start;
        let target = start + remaining / remaining_parts;
        let mut cut = target;
        while cut > start && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut == start && target < text.len() {
            cut = target + 1;
            while cut < text.len() && !text.is_char_boundary(cut) {
                cut += 1;
            }
        }
        if cut == start && start < text.len() {
            cut = start + text[start..].chars().next().unwrap().len_utf8();
        }
        result.push(text[start..cut].to_string());
        start = cut;
    }
    while result.len() < count {
        result.push(String::new());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control_chars_but_keeps_newline_and_tab() {
        let raw = "hello\x00\x01world\x07\nnext\tcol\x7fbad";
        let cleaned = sanitize_reply_input(raw);
        assert!(cleaned.contains("helloworld"));
        assert!(cleaned.contains("\nnext\tcol"));
        assert!(!cleaned.contains('\x00'));
        assert!(!cleaned.contains('\x7f'));
    }

    #[test]
    fn strips_bidi_overrides() {
        // U+202E = right-to-left override (classic phishing payload).
        let raw = "safe \u{202e}!evilcode\u{2069}";
        let cleaned = sanitize_reply_input(raw);
        assert!(!cleaned.contains('\u{202e}'));
        assert!(!cleaned.contains('\u{2069}'));
        assert!(cleaned.contains("evilcode"));
    }

    #[test]
    fn escapes_command_substitution() {
        let raw = "innocent text $(rm -rf /) and ${HOME}/bad";
        let cleaned = sanitize_reply_input(raw);
        assert!(cleaned.contains("\\$("));
        assert!(cleaned.contains("\\${"));
        // The escape is in place — a literal `$(` (no leading
        // backslash) must not survive. Use a window-search that
        // requires the preceding byte to NOT be a backslash.
        let unescaped_dollar_paren = cleaned
            .as_bytes()
            .windows(2)
            .enumerate()
            .any(|(i, w)| w == b"$(" && (i == 0 || cleaned.as_bytes()[i - 1] != b'\\'));
        assert!(
            !unescaped_dollar_paren,
            "found unescaped `$(` in {cleaned:?}"
        );
    }

    #[test]
    fn escapes_backticks_and_backslashes() {
        let raw = "before `whoami` after \\ndone";
        let cleaned = sanitize_reply_input(raw);
        assert!(cleaned.contains("\\`whoami\\`"));
        // Original backslash got doubled.
        assert!(cleaned.contains("\\\\ndone"));
    }

    #[test]
    fn tmux_variant_collapses_newlines() {
        let raw = "line one\nline two\r\nline three";
        let cleaned = sanitize_for_tmux(raw);
        assert!(!cleaned.contains('\n'));
        assert!(!cleaned.contains('\r'));
        assert!(cleaned.contains("line one line two line three"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let raw = "αβγδε".repeat(2000);
        let out = truncate_to_max(&raw, 100);
        assert_eq!(out.chars().count(), 100);
        // Must still be valid UTF-8 — implicit because we used .chars().
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn verify_pane_not_empty_rejects_whitespace() {
        assert!(!verify_pane_not_empty(""));
        assert!(!verify_pane_not_empty("   \n\t  "));
        assert!(verify_pane_not_empty("$ ccteam start"));
    }

    // ----- split_for_channel (V0.8.4 P0) ----------------------------

    #[test]
    fn split_fits_returns_single_part() {
        let parts = split_for_channel("short message", 4096);
        assert_eq!(parts, vec!["short message".to_string()]);
    }

    #[test]
    fn split_plain_text_concat_equals_original() {
        let original = "lorem ipsum dolor sit amet ".repeat(400); // ~10_800 chars
        let parts = split_for_channel(&original, 1000);
        assert!(
            parts.len() >= 2,
            "long text must split (got {})",
            parts.len()
        );
        // Lossless: parts concatenate back to the verbatim original.
        assert_eq!(parts.concat(), original);
        // Every part is within budget (UTF-16 units).
        for p in &parts {
            assert!(
                p.chars().map(char::len_utf16).sum::<usize>() <= 1000,
                "part over budget: {:?}",
                p
            );
        }
    }

    #[test]
    fn split_budgets_by_utf16_not_char_count() {
        // 3000 emoji: char count = 3000 (< 4096) but UTF-16 units =
        // 6000 (> 4096). Must still split, or Telegram returns 400.
        let original = "😀".repeat(3000);
        assert!(original.chars().count() < 4096);
        assert!(original.chars().map(char::len_utf16).sum::<usize>() > 4096);
        let parts = split_for_channel(&original, 4096);
        assert!(
            parts.len() >= 2,
            "emoji-heavy text must split by UTF-16 budget"
        );
        for p in &parts {
            assert!(p.chars().map(char::len_utf16).sum::<usize>() <= 4096);
        }
        assert_eq!(parts.concat(), original);
    }

    #[test]
    fn split_prefers_paragraph_over_line_break() {
        // \n\n at unit 20 (past min_fill=15), a lone \n later at unit 25.
        let text = format!(
            "{}\n\n{}\n{}",
            "x".repeat(18),
            "y".repeat(4),
            "z".repeat(60)
        );
        let parts = split_for_channel(&text, 30);
        // First part should end at the paragraph boundary, not the later
        // single newline nor a hard char cut.
        assert!(
            parts[0].ends_with("\n\n"),
            "expected paragraph-boundary cut, got {:?}",
            parts[0]
        );
        assert_eq!(parts.concat(), text);
    }

    #[test]
    fn split_reopens_and_closes_code_fence() {
        // A rust fence whose body overflows the budget.
        let body = "let x = 1;\n".repeat(60);
        let text = format!("intro line\n```rust\n{body}```\ntrailer");
        let parts = split_for_channel(&text, 120);
        assert!(parts.len() >= 2);
        for p in &parts {
            // Balanced: an even number of fence markers per part.
            let fences = p
                .lines()
                .filter(|l| l.trim_start().starts_with("```"))
                .count();
            assert_eq!(fences % 2, 0, "unbalanced fence in part: {:?}", p);
        }
        // The language info survives on at least one re-open.
        assert!(parts.iter().any(|p| p.contains("```rust")));
    }

    #[test]
    fn split_reopens_nested_fence_in_quote() {
        let body = "> value\n".repeat(40);
        let text = format!("> ```rust\n{body}> ```\n");
        let parts = split_for_channel(&text, 120);
        assert!(parts.len() > 1);
        for part in &parts {
            let fences = part
                .lines()
                .filter(|line| crate::telegram_html::is_fence_line(line.trim_start_matches("> ")))
                .count();
            assert_eq!(fences % 2, 0, "unbalanced quoted fence: {part:?}");
            assert!(
                part.lines().any(|line| line == "> ```rust"),
                "reopened fence lost its quote or language: {part:?}"
            );
        }
    }

    #[test]
    fn rich_tilde_fence_reopens_with_same_marker_and_language() {
        let markdown = format!("~~~rust\n{}~~~\n", "value\n".repeat(80));
        let parts = split_rich_markdown_numbered(&markdown, 180);
        assert!(parts.len() > 1);
        let total = parts.len();
        for (index, part) in parts.iter().enumerate() {
            assert!(part.len() <= 180, "part over ceiling: {part:?}");
            assert!(
                part.contains("~~~rust"),
                "language was not reopened: {part:?}"
            );
            let fences = part
                .lines()
                .filter(|line| crate::telegram_html::is_fence_line(line))
                .count();
            assert_eq!(fences % 2, 0, "unbalanced tilde fence: {part:?}");
            assert!(part.ends_with(&format!("({}/{total})", index + 1)));
        }
    }

    #[test]
    fn rich_split_reopens_nested_fence_in_quote() {
        let markdown = format!("> ```rust\n{}> ```\n", "> value\n".repeat(80));
        let parts = split_rich_markdown_numbered(&markdown, 180);
        assert!(parts.len() > 1);
        for part in &parts {
            let fences = part
                .lines()
                .filter_map(|line| line.strip_prefix("> "))
                .filter(|line| crate::telegram_html::is_fence_line(line))
                .count();
            assert_eq!(fences % 2, 0, "unbalanced quoted rich fence: {part:?}");
            assert!(
                part.contains("> ```rust"),
                "language was not reopened: {part:?}"
            );
        }
    }

    #[test]
    fn split_makes_progress_on_tiny_budget() {
        // Pathological: each emoji is 2 UTF-16 units, budget 1.
        let parts = split_for_channel("😀😀😀", 1);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts.concat(), "😀😀😀");
    }

    #[test]
    fn split_emoji_single_line_uses_utf16_ceiling() {
        let text = "😀".repeat(2000); // exactly 4000 UTF-16 units
        let parts = split_for_channel(&text, 3900);
        assert!(parts.len() > 1);
        assert_eq!(parts.concat(), text);
        assert!(parts.iter().all(|part| utf16_len(part) <= 3900));
    }

    #[test]
    fn rich_split_always_makes_progress() {
        for text in ["", "x", "~~~\nbody\n~~~", &"x".repeat(400)] {
            let parts = split_rich_markdown_numbered(text, 16);
            assert!(!parts.is_empty());
            if parts.len() > 1 {
                assert!(parts.iter().all(|part| part.len() < text.len()));
            }
        }
    }

    #[test]
    fn rich_split_keeps_plain_fallback_separate_per_part() {
        let parts =
            split_rich_markdown_with_plain(&"**rich** ".repeat(80), &"plain ".repeat(80), 100);
        assert!(parts.len() > 1);
        for (rich, plain) in parts {
            assert_ne!(rich, plain);
            assert!(plain.ends_with(')'));
        }
    }

    // ----- split_for_channel_numbered (TG-GATE-V2 W10) --------------

    #[test]
    fn numbered_split_fits_returns_single_unsuffixed_part() {
        let parts = split_for_channel_numbered("short message", 4096);
        assert_eq!(parts, vec!["short message".to_string()]);
    }

    #[test]
    fn numbered_split_appends_suffix_on_its_own_line_after_a_blank_line() {
        let original = "lorem ipsum dolor sit amet ".repeat(400);
        let parts = split_for_channel_numbered(&original, 1000);
        assert!(parts.len() >= 2);
        let total = parts.len();
        for (i, part) in parts.iter().enumerate() {
            let expected_suffix = format!("({}/{total})", i + 1);
            assert!(
                part.ends_with(&expected_suffix),
                "part {i} missing suffix: {part:?}"
            );
            // Blank line then the marker on its own line — never glued
            // onto the previous content line.
            let mut lines: Vec<&str> = part.lines().collect();
            let suffix_line = lines.pop().unwrap();
            assert_eq!(suffix_line, expected_suffix);
            let blank_line = lines.pop().unwrap_or("nonempty");
            assert!(
                blank_line.is_empty(),
                "expected a blank separator line before the suffix, got {blank_line:?} in {part:?}"
            );
            // The suffix line is never itself mistaken for a fence line.
            assert!(!crate::telegram_html::is_fence_line(suffix_line));
            // Every part stays within the requested ceiling.
            assert!(utf16_len(part) <= 1000, "part over budget: {part:?}");
        }
    }

    #[test]
    fn numbered_split_suffix_never_glues_onto_a_closing_fence_line() {
        // A fence whose body overflows the budget, so a part's last
        // content line before the suffix is a closing ` ``` ` — the
        // exact shape the suffix must never merge onto.
        let body = "let x = 1;\n".repeat(60);
        let text = format!("intro line\n```rust\n{body}```\ntrailer");
        let parts = split_for_channel_numbered(&text, 120);
        assert!(parts.len() >= 2);
        for part in &parts {
            let lines: Vec<&str> = part.lines().collect();
            for line in &lines {
                // No line mixes fence markers with the numbering marker —
                // each line is either a fence delimiter or the suffix,
                // never both.
                if crate::telegram_html::is_fence_line(line) {
                    assert!(
                        !line.contains('('),
                        "suffix glued onto a fence line: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn numbered_split_reserves_extra_digit_width_across_the_9_to_10_boundary() {
        // Craft input that needs exactly 10 parts once the `(i/n)`
        // suffix width is reserved — the digit width of `n` grows from
        // 1 to 2 mid-reservation (the case the iterative reserve loop
        // exists for). Every part must still carry a correctly-widened
        // 2-digit suffix, e.g. `(1/10)` not `(1/9)`.
        let block = "x".repeat(28);
        let text = std::iter::repeat_n(block, 40)
            .collect::<Vec<_>>()
            .join("\n\n");
        let limit = 32;
        let parts = split_for_channel_numbered(&text, limit);
        let total = parts.len();
        assert!(total >= 10, "expected at least 10 parts, got {total}");
        for (i, part) in parts.iter().enumerate() {
            let expected_suffix = format!("({}/{total})", i + 1);
            assert!(
                part.ends_with(&expected_suffix),
                "part {i} missing correctly-widened suffix: {part:?}"
            );
            assert!(utf16_len(part) <= limit, "part over budget: {part:?}");
        }
    }

    #[test]
    fn rich_split_keeps_each_part_valid_and_numbered() {
        let markdown = format!(
            "# One\n\n{}\n\n```rust\n{}\n```\n\n- last",
            "first ".repeat(20),
            "code();\n".repeat(30)
        );
        let parts = split_rich_markdown_numbered(&markdown, 180);
        assert!(parts.len() > 1);
        let total = parts.len();
        for (i, part) in parts.iter().enumerate() {
            assert!(part.len() <= 180, "{part:?}");
            assert!(part.ends_with(&format!("({}/{total})", i + 1)));
            assert_eq!(
                part.lines()
                    .filter(|line| line.trim_start().starts_with("```"))
                    .count()
                    % 2,
                0,
                "{part:?}"
            );
        }
    }

    #[test]
    fn rich_split_keeps_tables_quotes_and_details_atomic() {
        let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let quote = "> one\n> two\n";
        let details = "<details>\n<summary>x</summary>\nbody\n</details>\n";
        let markdown = format!(
            "{}\n{}\n{}\n{}",
            "before\n".repeat(10),
            table,
            quote,
            details
        );
        let parts = split_rich_markdown_numbered(&markdown, 100);
        for block in [table, quote, details] {
            assert!(
                parts.iter().any(|part| part.contains(block)),
                "lost atomic block: {block:?}"
            );
        }
    }
}
