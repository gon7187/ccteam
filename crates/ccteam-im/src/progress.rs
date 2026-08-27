//! V0.8.4 P1 (B1) — live progress folding for IM status messages.
//!
//! A turn's tool / reasoning activity is folded into **one** editable
//! "status" message (mirrors the official telegram plugin UX) rather than
//! one ping per step. Borrows claude-code's `GroupedToolUseMessage` /
//! `CollapsedReadSearchGroup` (group + count by category) and
//! `truncateForPreview` (phone-sized arg previews).
//!
//! Granularity = **per completed step**. The transcript only lands a row
//! when a step finishes; there is no sub-second token stream to follow,
//! so the fold reacts to whole [`ThreadEvent`]s, never tokens.
//!
//! The shape Claude emits (verified against `transcript_tail`): a tool is
//! `ItemStarted{ToolCall{name,args=input}}` then
//! `ItemCompleted{ToolCall{name,args=result}}` (same `item.id`);
//! reasoning is `ItemUpdated{Reasoning}`; the answer is
//! `ItemCompleted{AgentMessage}`. Codex additionally emits
//! `CommandExecution` / `FileChange` / `WebSearch` as `ItemCompleted`.
//! [`ProgressFold`] handles all of them and de-dups a tool's
//! start/complete pair by `item.id` so it counts once.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use ccteam_harness::{ThreadEvent, ThreadItemDetails};
use serde_json::Value;

use crate::gateway::{ActivityKind, ActivityStatus, SessionActivity};

/// Phone-sized cap (chars) for a tool's argument preview.
pub const PREVIEW_MAX: usize = 200;
/// Recent tool/command lines shown in the terminal block.
const MAX_DETAIL_LINES: usize = 6;
/// Per-line terminal cap. Six Unicode scalar lines stay comfortably below
/// Telegram's 3000 UTF-16-unit cap.
const MAX_OUTPUT_LINE_CHARS: usize = 120;

/// Stable positive Bot API draft id derived from a session id.
pub fn draft_id(sid: &str) -> i64 {
    let hash = sid.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        hash.wrapping_mul(0x100000001b3_u64) ^ u64::from(byte)
    });
    let id = hash & i64::MAX as u64;
    if id == 0 {
        1
    } else {
        id as i64
    }
}

/// A folded category: a stable emoji + short label that several raw tool
/// names collapse into (e.g. `Read`/`Grep`/`Glob` → `read`).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Category {
    emoji: &'static str,
    label: &'static str,
}

const CAT_READ: Category = Category {
    emoji: "📖",
    label: "чтение",
};
const CAT_BASH: Category = Category {
    emoji: "🔧",
    label: "команда",
};
const CAT_EDIT: Category = Category {
    emoji: "✏️",
    label: "правка",
};
const CAT_WEB: Category = Category {
    emoji: "🔎",
    label: "веб",
};
const CAT_TASK: Category = Category {
    emoji: "🤖",
    label: "задача",
};
const CAT_TODO: Category = Category {
    emoji: "📝",
    label: "план",
};

/// Map a raw tool name (Claude `ToolCall.name`, or a Codex tool) to a
/// folded [`Category`]. Unknown names fold under the wrench with the raw
/// (lowercased) name so nothing is silently dropped.
fn tool_category(name: &str) -> Category {
    match name {
        "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" => CAT_READ,
        "Bash" | "BashOutput" | "KillBash" | "KillShell" => CAT_BASH,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => CAT_EDIT,
        "WebSearch" | "WebFetch" => CAT_WEB,
        "Task" => CAT_TASK,
        "TodoWrite" => CAT_TODO,
        _ => Category {
            emoji: "🔧",
            // Leak the raw name (it is `&str` with a non-static lifetime
            // in general, but tool names from the adapter live long
            // enough; we copy into the count label as an owned String, so
            // the &'static here is only the fallback emoji bucket).
            label: "инструмент",
        },
    }
}

/// Collapse whitespace + truncate to [`PREVIEW_MAX`] chars with an
/// ellipsis. Mirrors claude-code `truncateForPreview` — keeps a
/// `Write(5KB)` / multi-line `Bash(...)` from flooding the status line.
pub fn truncate_for_preview(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PREVIEW_MAX {
        return flat;
    }
    let mut out: String = flat.chars().take(PREVIEW_MAX - 1).collect();
    out.push('…');
    out
}

/// Pull a human-meaningful field out of a tool's JSON args for the
/// preview line (command / path / pattern / query), falling back to a
/// compact JSON rendering.
pub(crate) fn preview_args(args: &Value) -> String {
    let picked = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "prompt",
    ]
    .iter()
    .find_map(|k| args.get(*k).and_then(Value::as_str));
    let raw = match picked {
        Some(s) => s.to_string(),
        None => match args {
            Value::Null => String::new(),
            other => other.to_string(),
        },
    };
    truncate_for_preview(&raw)
}

fn literal_preview(value: &str) -> String {
    if value.contains('`') {
        value.to_string()
    } else {
        format!("`{value}`")
    }
}

/// Map a lifecycle [`ThreadEvent`] to its [`ActivityStatus`]. Item events
/// only — non-item events never reach this (the caller short-circuits).
fn activity_status(evt: &ThreadEvent) -> Option<ActivityStatus> {
    match evt {
        ThreadEvent::ItemStarted { .. } => Some(ActivityStatus::Started),
        ThreadEvent::ItemCompleted { .. } => Some(ActivityStatus::Completed),
        ThreadEvent::ItemUpdated { .. } => Some(ActivityStatus::Update),
        _ => None,
    }
}

/// The SHARED per-step summarizer (v0.8.19). Inspects the SAME
/// [`ThreadItemDetails`] cases [`ProgressFold::apply`] does and reuses the
/// same preview helpers ([`tool_category`] / [`preview_args`] /
/// [`truncate_for_preview`]), so the IM status string and the web activity
/// summary are computed from one place and can never drift.
///
/// Returns the structured [`SessionActivity`] for a renderable item event,
/// or `None` for the answer (`AgentMessage`), errors, and any non-item
/// lifecycle event (turn started/completed/failed, thread started) — those
/// the pump routes separately and must not show up as activity.
pub(crate) fn activity_for(evt: &ThreadEvent) -> Option<SessionActivity> {
    let (item, status) = match evt {
        ThreadEvent::ItemStarted { item }
        | ThreadEvent::ItemCompleted { item }
        | ThreadEvent::ItemUpdated { item } => (item, activity_status(evt)?),
        _ => return None,
    };
    match &item.details {
        ThreadItemDetails::ToolCall { name, args } => Some(SessionActivity {
            kind: ActivityKind::ToolCall,
            name: name.clone(),
            summary: format!("{name}({})", preview_args(args)),
            status,
            item_id: item.id.clone(),
        }),
        ThreadItemDetails::CommandExecution { cmd, .. } => Some(SessionActivity {
            kind: ActivityKind::CommandExec,
            name: "bash".to_string(),
            summary: format!("$ {}", literal_preview(&truncate_for_preview(cmd))),
            status,
            item_id: item.id.clone(),
        }),
        ThreadItemDetails::FileChange { path, kind } => Some(SessionActivity {
            kind: ActivityKind::FileChange,
            name: kind.clone(),
            summary: format!("{kind} {}", literal_preview(&path.display().to_string())),
            status,
            item_id: item.id.clone(),
        }),
        ThreadItemDetails::WebSearch { query } => Some(SessionActivity {
            kind: ActivityKind::WebSearch,
            name: "web".to_string(),
            summary: truncate_for_preview(query),
            status,
            item_id: item.id.clone(),
        }),
        ThreadItemDetails::Reasoning(text) => Some(SessionActivity {
            kind: ActivityKind::Thinking,
            name: String::new(),
            summary: truncate_for_preview(text),
            status: ActivityStatus::Update,
            item_id: item.id.clone(),
        }),
        // The answer + errors are not activity (the pump routes them).
        ThreadItemDetails::AgentMessage(_) | ThreadItemDetails::Error(_) => None,
    }
}

/// One folded count bucket, kept in first-seen order for stable render.
struct Bucket {
    emoji: String,
    label: String,
    count: usize,
}

/// Rolling fold of a single status epoch (one turn's progress). Feed it
/// [`ThreadEvent`]s with [`apply`](Self::apply); render the current
/// status text with [`render`](Self::render).
pub struct ProgressFold {
    buckets: Vec<Bucket>,
    seen_ids: HashSet<String>,
    /// Most recent step previews (newest last), capped at
    /// [`MAX_DETAIL_LINES`].
    recent: Vec<String>,
    thinking: bool,
    /// Codex streamed an `ItemUpdated{AgentMessage}` delta (drafting the
    /// reply), which keeps this fold eligible for a progress update.
    drafting: bool,
    done: bool,
    tool_total: usize,
    file_total: usize,
    started_at: Instant,
}

impl Default for ProgressFold {
    fn default() -> Self {
        Self {
            buckets: Vec::new(),
            seen_ids: HashSet::new(),
            recent: Vec::new(),
            thinking: false,
            drafting: false,
            done: false,
            tool_total: 0,
            file_total: 0,
            started_at: Instant::now(),
        }
    }
}

impl ProgressFold {
    /// Fresh, empty fold for a new status epoch (one per turn).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether anything worth showing has accumulated (so the pump knows
    /// whether to bother sending / finalizing a status message).
    pub fn has_activity(&self) -> bool {
        !self.buckets.is_empty() || self.thinking || self.drafting
    }

    /// Whether the epoch has been finalized (renders the `✅ done` line).
    pub fn done(&self) -> bool {
        self.done
    }

    /// Mark the epoch finished (renders the `✅ done · …` summary).
    pub fn mark_done(&mut self) {
        self.done = true;
    }

    /// `unknown_label` is the already-lowercased raw tool name
    /// ([`count_tool`] computes it once and shares it with the detail
    /// line, so the footer bucket and the detail line can never disagree
    /// about an unknown tool's display name).
    fn bump(&mut self, cat: Category, unknown_label: &str) {
        // Unknown tools fold under the wrench but keep their own label.
        let label: String = if cat.label == "инструмент" {
            unknown_label.to_string()
        } else {
            cat.label.to_string()
        };
        if let Some(b) = self.buckets.iter_mut().find(|b| b.label == label) {
            b.count += 1;
        } else {
            self.buckets.push(Bucket {
                emoji: cat.emoji.to_string(),
                label,
                count: 1,
            });
        }
    }

    fn push_recent(&mut self, line: String) {
        self.recent.push(line);
        if self.recent.len() > MAX_DETAIL_LINES {
            self.recent.remove(0);
        }
    }

    /// Count a tool once (de-duped by `item.id`), bump its category, and
    /// record a preview line. `content` is the tool-specific detail (args
    /// preview / command / path); the category label is prefixed here so
    /// every detail line reads `<emoji> <label>: <content>` instead of the
    /// raw (often English) tool name. Unknown tools keep their raw name
    /// after the fallback label so nothing is silently dropped. Returns
    /// whether state changed.
    fn count_tool(&mut self, id: &str, cat: Category, raw_name: &str, content: String) -> bool {
        if !self.seen_ids.insert(id.to_string()) {
            return false; // start/complete pair — already counted
        }
        // Computed once: the footer bucket ([`bump`]) and this detail line
        // must show the SAME name for an unknown tool, not two independently
        // formatted strings that can drift apart in case or content.
        let unknown_label = raw_name.to_lowercase();
        self.bump(cat, &unknown_label);
        self.tool_total += 1;
        if cat == CAT_EDIT {
            self.file_total += 1;
        }
        let label = if cat.label == "инструмент" {
            format!("{} {unknown_label}", cat.label)
        } else {
            cat.label.to_string()
        };
        self.push_recent(format!("{} {label}: {content}", cat.emoji));
        true
    }

    /// Fold one event into the status. Returns `true` if the rendered
    /// status would change (the caller marks the status dirty). Answer
    /// (`ItemCompleted{AgentMessage}`) and lifecycle events are *not*
    /// progress and return `false` — the pump routes those separately.
    pub fn apply(&mut self, evt: &ThreadEvent) -> bool {
        match evt {
            ThreadEvent::ItemStarted { item } | ThreadEvent::ItemCompleted { item } => {
                match &item.details {
                    ThreadItemDetails::ToolCall { name, args } => {
                        self.count_tool(&item.id, tool_category(name), name, preview_args(args))
                    }
                    ThreadItemDetails::CommandExecution { cmd, .. } => {
                        let preview = format!("$ {}", literal_preview(&truncate_for_preview(cmd)));
                        self.count_tool(&item.id, CAT_BASH, "bash", preview)
                    }
                    ThreadItemDetails::FileChange { path, kind } => {
                        let preview =
                            format!("{kind} {}", literal_preview(&path.display().to_string()));
                        self.count_tool(&item.id, CAT_EDIT, "edit", preview)
                    }
                    ThreadItemDetails::WebSearch { query } => {
                        let preview = truncate_for_preview(query);
                        self.count_tool(&item.id, CAT_WEB, "web", preview)
                    }
                    ThreadItemDetails::Reasoning(_) => self.set_thinking(),
                    ThreadItemDetails::AgentMessage(_) | ThreadItemDetails::Error(_) => false,
                }
            }
            ThreadEvent::ItemUpdated { item } => match &item.details {
                ThreadItemDetails::Reasoning(_) => self.set_thinking(),
                // Codex streaming delta — drafting, not an answer.
                ThreadItemDetails::AgentMessage(_) => {
                    if self.drafting {
                        false
                    } else {
                        self.drafting = true;
                        true
                    }
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn set_thinking(&mut self) -> bool {
        if self.thinking {
            false
        } else {
            self.thinking = true;
            true
        }
    }

    fn counts_summary(&self) -> String {
        self.buckets
            .iter()
            .map(|b| format!("{} {} ×{}", b.emoji, b.label, b.count))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Compact per-tool counts for the IM `/status` fleet line, e.g.
    /// `read×16·bash×8` — labels + counts only (no emoji, no spaces), joined
    /// by `·`. `None` when nothing tool-like has happened yet (the working
    /// line then shows only state + model + ctx). Shares the SAME per-category
    /// buckets the live IM status message folds, so `/status` can never drift
    /// from the progress text.
    pub fn compact_counts(&self) -> Option<String> {
        if self.buckets.is_empty() {
            return None;
        }
        Some(
            self.buckets
                .iter()
                .map(|b| format!("{}×{}", b.label, b.count))
                .collect::<Vec<_>>()
                .join("·"),
        )
    }

    fn done_summary(&self) -> String {
        format!(
            "{} {} · {} {}",
            self.tool_total,
            russian_count_word(self.tool_total, "инструмент", "инструмента", "инструментов"),
            self.file_total,
            russian_count_word(self.file_total, "файл", "файла", "файлов")
        )
    }

    /// Render the current terminal-style status text for `sid`.
    pub fn render(&self, sid: &str) -> String {
        let elapsed = render_elapsed(self.started_at.elapsed());
        if self.done {
            return format!("✅ {sid} готово · {elapsed} · {}", self.done_summary());
        }
        let mut lines = vec![format!("▶️ {sid} работает · {elapsed}")];
        let summary = self.counts_summary();
        if !self.recent.is_empty() {
            lines.push("```Terminal".to_string());
            lines.extend(self.recent.iter().map(|line| terminal_line(line)));
            lines.push("```".to_string());
        }
        if !summary.is_empty() {
            lines.push(summary);
        }
        lines.join("\n")
    }

    /// Render the draft-only thinking header plus the normal live progress.
    pub fn render_draft(&self, sid: &str) -> String {
        draft_markdown(&self.render(sid))
    }
}

/// Add Telegram's draft-only thinking block to an already-rendered progress
/// card. The gateway owns the fold, while the daemon owns channel delivery.
pub fn draft_markdown(progress: &str) -> String {
    format!("<tg-thinking>Thinking...</tg-thinking>\n\n{progress}")
}

fn render_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds >= 3600 {
        format!(
            "{}ч{}м{}с",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    } else if seconds >= 60 {
        format!("{}м{}с", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}с")
    }
}

fn terminal_line(line: &str) -> String {
    let stripped = strip_ansi(line);
    let flat = stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("```", "ˋˋˋ");
    if flat.chars().count() <= MAX_OUTPUT_LINE_CHARS {
        return flat;
    }
    let mut out: String = flat.chars().take(MAX_OUTPUT_LINE_CHARS - 1).collect();
    out.push('…');
    out
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(code) = chars.next() {
                    if code == '\u{7}' {
                        break;
                    }
                    if code == '\u{1b}' && chars.next() == Some('\\') {
                        break;
                    }
                }
            }
            Some(_) | None => {}
        }
    }
    out
}

pub(crate) fn russian_count_word<'a>(
    count: usize,
    one: &'a str,
    few: &'a str,
    many: &'a str,
) -> &'a str {
    let remainder = count % 100;
    if (11..=14).contains(&remainder) {
        return many;
    }
    match count % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_harness::{ThreadItem, ThreadItemDetails};
    use serde_json::json;

    fn started_tool(id: &str, name: &str, args: Value) -> ThreadEvent {
        ThreadEvent::ItemStarted {
            item: ThreadItem {
                id: id.to_string(),
                details: ThreadItemDetails::ToolCall {
                    name: name.to_string(),
                    args,
                },
            },
        }
    }

    fn completed_tool(id: &str, name: &str) -> ThreadEvent {
        ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: id.to_string(),
                details: ThreadItemDetails::ToolCall {
                    name: name.to_string(),
                    args: json!("result"),
                },
            },
        }
    }

    #[test]
    fn folds_and_counts_by_category() {
        let mut f = ProgressFold::new();
        assert!(f.apply(&started_tool("t1", "Read", json!({"file_path": "/a"}))));
        assert!(f.apply(&started_tool("t2", "Read", json!({"file_path": "/b"}))));
        assert!(f.apply(&started_tool("t3", "Bash", json!({"command": "ls"}))));
        let r = f.render("s42");
        assert!(r.contains("📖 чтение ×2"), "got: {r}");
        assert!(r.contains("🔧 команда ×1"), "got: {r}");
        assert!(r.starts_with("▶️ s42 работает · "), "got: {r}");
    }

    #[test]
    fn compact_counts_is_label_times_count_no_emoji() {
        let mut f = ProgressFold::new();
        // Empty fold → no compact summary.
        assert_eq!(f.compact_counts(), None);
        f.apply(&started_tool("t1", "Read", json!({"file_path": "/a"})));
        f.apply(&started_tool("t2", "Read", json!({"file_path": "/b"})));
        f.apply(&started_tool("t3", "Bash", json!({"command": "ls"})));
        // `/status`-shaped: labels + counts only, joined by `·`, no emoji/space.
        assert_eq!(f.compact_counts().as_deref(), Some("чтение×2·команда×1"));
    }

    #[test]
    fn dedups_tool_start_and_complete_by_id() {
        let mut f = ProgressFold::new();
        assert!(f.apply(&started_tool("t1", "Bash", json!({"command": "ls"}))));
        // The matching completion carries the same id → must NOT recount.
        assert!(!f.apply(&completed_tool("t1", "Bash")));
        assert!(f.render("s42").contains("🔧 команда ×1"));
    }

    #[test]
    fn truncates_arg_preview_phone_sized() {
        let mut f = ProgressFold::new();
        let long = "x".repeat(500);
        f.apply(&started_tool("t1", "Bash", json!({ "command": long })));
        let r = f.render("s42");
        // The arg preview is capped to PREVIEW_MAX chars; the whole line
        // adds only the `🔧 Bash(…)` wrapper + indent, so it stays far
        // below the untruncated 500.
        let detail = r
            .split("```Terminal\n")
            .nth(1)
            .and_then(|body| body.lines().next())
            .expect("output detail");
        assert!(
            detail.chars().count() < PREVIEW_MAX + 32,
            "detail not truncated: {} chars",
            detail.chars().count()
        );
        assert!(detail.contains('…'));
    }

    #[test]
    fn reasoning_sets_thinking_without_counting() {
        let mut f = ProgressFold::new();
        let ev = ThreadEvent::ItemUpdated {
            item: ThreadItem {
                id: "r1".into(),
                details: ThreadItemDetails::Reasoning("hmm".into()),
            },
        };
        assert!(f.apply(&ev));
        assert!(!f.apply(&ev)); // second reasoning is a no-op
        assert!(f.render("s42").starts_with("▶️ s42 работает · "));
        assert!(f.has_activity());
    }

    #[test]
    fn codex_command_and_file_change_fold() {
        let mut f = ProgressFold::new();
        f.apply(&ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "c1".into(),
                details: ThreadItemDetails::CommandExecution {
                    cmd: "cargo build".into(),
                    status: "ok".into(),
                },
            },
        });
        f.apply(&ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "fc1".into(),
                details: ThreadItemDetails::FileChange {
                    path: "/src/lib.rs".into(),
                    kind: "modified".into(),
                },
            },
        });
        let r = f.render("s42");
        assert!(r.contains("🔧 команда ×1"), "got: {r}");
        assert!(r.contains("✏️ правка ×1"), "got: {r}");
        assert!(r.contains("$ `cargo build`"), "got: {r}");
        assert!(r.contains("modified `/src/lib.rs`"), "got: {r}");
    }

    #[test]
    fn done_renders_summary_counts() {
        let mut f = ProgressFold::new();
        f.apply(&started_tool("t1", "Bash", json!({"command": "ls"})));
        f.apply(&started_tool("t2", "Edit", json!({"file_path": "/a"})));
        f.mark_done();
        let rendered = f.render("s42");
        assert!(rendered.starts_with("✅ s42 готово · "));
        assert!(rendered.contains("2 инструмента · 1 файл"));
    }

    #[test]
    fn draft_id_is_stable_positive_and_nonzero() {
        assert_eq!(draft_id("s42"), draft_id("s42"));
        assert!(draft_id("s42") > 0);
        assert_ne!(draft_id("s42"), draft_id("s43"));
    }

    #[test]
    fn draft_render_has_thinking_header_and_terminal_progress() {
        let mut f = ProgressFold::new();
        f.apply(&started_tool("t1", "Bash", json!({"command": "ls"})));
        let rendered = f.render_draft("s42");
        assert!(rendered.starts_with("<tg-thinking>Thinking...</tg-thinking>\n\n"));
        assert!(rendered.contains("▶️ s42 работает · "));
        assert!(rendered.contains("```Terminal\n"));
    }

    #[test]
    fn done_count_words_follow_russian_plural_rules() {
        assert_eq!(
            russian_count_word(1, "инструмент", "инструмента", "инструментов"),
            "инструмент"
        );
        assert_eq!(
            russian_count_word(2, "инструмент", "инструмента", "инструментов"),
            "инструмента"
        );
        assert_eq!(
            russian_count_word(5, "инструмент", "инструмента", "инструментов"),
            "инструментов"
        );
        assert_eq!(
            russian_count_word(11, "инструмент", "инструмента", "инструментов"),
            "инструментов"
        );
        assert_eq!(
            russian_count_word(21, "инструмент", "инструмента", "инструментов"),
            "инструмент"
        );
    }

    #[test]
    fn elapsed_uses_compact_russian_terminal_units() {
        assert_eq!(render_elapsed(Duration::from_secs(5)), "5с");
        assert_eq!(render_elapsed(Duration::from_secs(83)), "1м23с");
        assert_eq!(render_elapsed(Duration::from_secs(3_723)), "1ч2м3с");
    }

    // ----- v0.8.19 shared activity summarizer -----

    #[test]
    fn activity_for_maps_tool_call_started() {
        // The shared summarizer turns an ItemStarted{ToolCall} into a
        // structured SessionActivity with the SAME preview the fold computes.
        let ev = started_tool("t1", "Bash", json!({"command": "ls -la"}));
        let act = activity_for(&ev).expect("tool call is activity");
        assert_eq!(act.kind, ActivityKind::ToolCall);
        assert_eq!(act.name, "Bash");
        assert_eq!(act.summary, "Bash(ls -la)");
        assert_eq!(act.status, ActivityStatus::Started);
        assert_eq!(act.item_id, "t1");
    }

    #[test]
    fn activity_for_maps_tool_call_completed_status() {
        // The lifecycle variant drives `status`: a completion → Completed.
        let act = activity_for(&completed_tool("t1", "Bash")).expect("activity");
        assert_eq!(act.kind, ActivityKind::ToolCall);
        assert_eq!(act.status, ActivityStatus::Completed);
        assert_eq!(act.item_id, "t1");
    }

    #[test]
    fn activity_for_maps_reasoning_to_thinking() {
        let ev = ThreadEvent::ItemUpdated {
            item: ThreadItem {
                id: "r1".into(),
                details: ThreadItemDetails::Reasoning("let me think about this".into()),
            },
        };
        let act = activity_for(&ev).expect("reasoning is activity");
        assert_eq!(act.kind, ActivityKind::Thinking);
        assert!(act.name.is_empty(), "thinking has no name");
        assert_eq!(act.summary, "let me think about this");
        assert_eq!(act.status, ActivityStatus::Update);
        assert_eq!(act.item_id, "r1");
    }

    #[test]
    fn activity_for_maps_codex_command_and_file_change_and_websearch() {
        let cmd = activity_for(&ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "c1".into(),
                details: ThreadItemDetails::CommandExecution {
                    cmd: "cargo build".into(),
                    status: "ok".into(),
                },
            },
        })
        .expect("command exec is activity");
        assert_eq!(cmd.kind, ActivityKind::CommandExec);
        assert_eq!(cmd.summary, "$ `cargo build`");

        let fc = activity_for(&ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "fc1".into(),
                details: ThreadItemDetails::FileChange {
                    path: "/src/lib.rs".into(),
                    kind: "modified".into(),
                },
            },
        })
        .expect("file change is activity");
        assert_eq!(fc.kind, ActivityKind::FileChange);
        assert_eq!(fc.summary, "modified `/src/lib.rs`");

        let ws = activity_for(&ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "w1".into(),
                details: ThreadItemDetails::WebSearch {
                    query: "rust serde".into(),
                },
            },
        })
        .expect("web search is activity");
        assert_eq!(ws.kind, ActivityKind::WebSearch);
        assert_eq!(ws.summary, "rust serde");
    }

    #[test]
    fn activity_for_returns_none_for_agent_message_and_lifecycle() {
        // The answer is NOT activity (the pump routes it as an Answer event).
        let answer = ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "a1".into(),
                details: ThreadItemDetails::AgentMessage("final reply".into()),
            },
        };
        assert!(activity_for(&answer).is_none());
        // Errors are not activity either.
        let err = ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "e1".into(),
                details: ThreadItemDetails::Error("boom".into()),
            },
        };
        assert!(activity_for(&err).is_none());
        // Non-item lifecycle events (turn boundaries) are not activity.
        assert!(activity_for(&ThreadEvent::TurnStarted {
            turn_id: "turn-1".into()
        })
        .is_none());
    }

    #[test]
    fn activity_for_summary_matches_the_fold_preview() {
        // THE no-drift guarantee: the activity summary and the fold's detail
        // line both derive from the same `preview_args` helper, so the arg
        // preview embedded in one is embedded in the other too (the fold
        // additionally prefixes the localized category label).
        let ev = started_tool("t1", "Read", json!({"file_path": "/etc/hosts"}));
        let act = activity_for(&ev).expect("activity");
        let mut f = ProgressFold::new();
        f.apply(&ev);
        let detail = f.render("s42");
        assert_eq!(act.summary, "Read(/etc/hosts)");
        assert!(
            detail.contains("📖 чтение: /etc/hosts"),
            "fold detail: {detail}"
        );
    }

    #[test]
    fn bash_tool_detail_line_uses_category_label_not_raw_name() {
        let mut f = ProgressFold::new();
        f.apply(&started_tool(
            "t1",
            "Bash",
            json!({"command": "ls graphify-out/graph.json"}),
        ));
        let r = f.render("s42");
        assert!(
            r.contains("🔧 команда: ls graphify-out/graph.json"),
            "got: {r}"
        );
        assert!(!r.contains("Bash("), "raw tool name leaked: {r}");
    }

    #[test]
    fn unknown_tool_detail_line_keeps_raw_name_after_fallback_label() {
        let mut f = ProgressFold::new();
        f.apply(&started_tool(
            "t1",
            "ToolSearch",
            json!({"query": "select:..."}),
        ));
        let r = f.render("s42");
        // Same lowercase label the footer's `toolsearch×1` bucket uses
        // (review round 1 — detail line and bucket must never drift apart).
        assert!(
            r.contains("🔧 инструмент toolsearch: select:..."),
            "got: {r}"
        );
        assert!(r.contains("🔧 toolsearch ×1"), "got: {r}");
    }

    #[test]
    fn answer_and_lifecycle_events_are_not_progress() {
        let mut f = ProgressFold::new();
        let answer = ThreadEvent::ItemCompleted {
            item: ThreadItem {
                id: "a1".into(),
                details: ThreadItemDetails::AgentMessage("final".into()),
            },
        };
        assert!(!f.apply(&answer));
        assert!(!f.has_activity());
    }

    #[test]
    fn line_count_capped() {
        let mut f = ProgressFold::new();
        for i in 0..30 {
            f.apply(&started_tool(
                &format!("t{i}"),
                "Bash",
                json!({"command": "ls"}),
            ));
        }
        let rendered = f.render("s42");
        let block = rendered
            .split("```Terminal\n")
            .nth(1)
            .and_then(|body| body.split("\n```").next())
            .expect("fenced output block");
        assert_eq!(block.lines().count(), 6);
    }

    #[test]
    fn terminal_render_keeps_recent_output_inside_a_fenced_text_block() {
        let mut f = ProgressFold::new();
        for i in 0..14 {
            f.apply(&started_tool(
                &format!("t{i}"),
                "Bash",
                json!({"command": format!("\u{1b}[31mline-{i} {}", "x".repeat(200))}),
            ));
        }

        let rendered = f.render("s42");
        assert!(
            rendered.starts_with("▶️ s42 работает · "),
            "got: {rendered}"
        );
        assert!(rendered.contains("```Terminal\n"), "got: {rendered}");
        assert!(!rendered.contains('\u{1b}'), "ANSI leaked: {rendered:?}");

        let block = rendered
            .split("```Terminal\n")
            .nth(1)
            .and_then(|body| body.split("\n```").next())
            .expect("fenced output block");
        assert_eq!(block.lines().count(), 6);
        assert!(block.lines().all(|line| line.chars().count() <= 120));
        assert!(block.encode_utf16().count() <= 3000);
        assert!(block.contains("line-13"));
        assert!(!block.contains("line-7"));
        assert!(rendered.contains("🔧 команда ×14"));

        let html = crate::telegram_html::render_markdown(&rendered).html;
        assert!(
            html.contains("<pre><code class=\"language-Terminal\">"),
            "got: {html}"
        );
    }
}
