//! Pure rich-message renderers for the IM gateway views.

use std::path::PathBuf;

use crate::transport::{ButtonStyle, MessageOption, ReplyKeyboard};

/// A rendered response with Telegram-rich and universal plain representations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichReply {
    /// Rich Messages Markdown sent to Telegram after the transport merge.
    pub markdown: String,
    /// Plain fallback for every channel and failed rich send.
    pub plain: String,
    /// Telegram button rows, with at most eight buttons per row.
    pub button_rows: Vec<Vec<MessageOption>>,
    /// The markdown already contains the button rows; classic fallback still
    /// uses `button_rows` as its reply markup.
    pub inline_buttons: bool,
    /// Persistent Telegram reply keyboard request.
    pub reply_keyboard: Option<ReplyKeyboard>,
}

impl RichReply {
    /// Wrap a legacy plain response without interactive controls.
    pub fn plain(plain: impl Into<String>) -> Self {
        let plain = plain.into();
        Self {
            markdown: plain.clone(),
            plain,
            button_rows: Vec::new(),
            inline_buttons: false,
            reply_keyboard: None,
        }
    }

    /// Attach a persistent Telegram reply keyboard request.
    pub fn with_reply_keyboard(mut self, reply_keyboard: ReplyKeyboard) -> Self {
        self.reply_keyboard = Some(reply_keyboard);
        self
    }
}

/// A plain-text reply carrying the same "🔄 Обновить" / "✏️ Новая" footer
/// every non-empty `/sessions`/`/status` card ends with. R2-1 — the empty
/// states of those two screens used to fall back to bare [`RichReply::plain`]
/// (no `button_rows`), and a §3.5c in-place edit forwards `button_rows`
/// verbatim on both the classic (`reply_markup`) and Rich Messages
/// (`<tg-button-row>` tags embedded via `inline_buttons`) legs — so tapping
/// "🔄 Обновить" on the LAST session's list, or a fresh `/status` with none
/// running, stripped the tapped message down to plain text with no way back
/// in. `refresh_command` lets each screen point the footer's first button at
/// itself (`/sessions` vs `/status`).
pub fn plain_with_refresh(text: impl Into<String>, refresh_command: &str) -> RichReply {
    let plain = text.into();
    let row = command_row([
        redraw_button("🔄 Обновить", refresh_command),
        command_button("✏️ Новая", "?/new"),
    ]);
    let inline_buttons = !row.is_empty();
    let mut markdown = escape_status_markdown(&plain);
    if inline_buttons {
        append_inline_rows(&mut markdown, std::slice::from_ref(&row));
    }
    RichReply {
        markdown,
        plain,
        button_rows: vec![row],
        inline_buttons,
        reply_keyboard: None,
    }
}

/// Facts needed to render the focused session card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusView {
    /// Stable ccteam session id.
    pub sid: String,
    /// Project slug.
    pub project: String,
    /// Harness vendor.
    pub vendor: String,
    /// Current lifecycle label.
    pub state: String,
    /// Active model, or an honest placeholder.
    pub model: String,
    /// Requested reasoning effort, or an honest placeholder.
    pub effort: String,
    /// Context usage percent, or an honest placeholder.
    pub context: String,
    /// Project directory.
    pub path: String,
    /// Session host.
    pub host: String,
    /// Detail sections, one line each; empty strings separate sections.
    pub detail_lines: Vec<String>,
    /// Project-scoped trailing 24-hour cost from the progress ledger.
    pub cost_24h: String,
    /// Vendor resume UUID, or an honest placeholder.
    pub resume: String,
    /// Direct children and the detail line that describes each one.
    pub children: Vec<StatusChild>,
}

/// A direct child rendered in the status detail section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChild {
    /// Stable ccteam session id.
    pub sid: String,
    /// Index into [`StatusView::detail_lines`].
    pub detail_line_index: usize,
}

/// One compact session-list row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Stable ccteam session id.
    pub sid: String,
    /// Vendor and model in one compact cell.
    pub vendor_model: String,
    /// Raw activity classification — `"working"` / `"idle"` / `"stale"` /
    /// `"stuck"` (§3.5b) — NOT a pre-rendered display string. Rendered via
    /// [`activity_badge`]; any other value (including a legacy
    /// pre-rendered string like `"🟢 ожидание"`) falls through to its `⚪
    /// неизвестно` fallback (R2-4 — a caller that passes a display string
    /// here silently gets that fallback instead of the state it meant).
    pub status: String,
    /// Context usage percent, or an honest placeholder.
    pub context: String,
    /// Optional user-facing title for the session button.
    pub title: Option<String>,
    /// Whether this is the caller's focused session.
    pub current: bool,
    /// Delegation-tree depth within the visible list.
    pub tree_depth: usize,
    /// Non-local execution host, when present.
    pub host: Option<String>,
}

/// One live body that survived a daemon restart and cannot be driven yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedSessionRow {
    /// Stable ccteam session id.
    pub sid: String,
    /// Surviving process id.
    pub pid: u32,
}

/// Facts needed to render the session picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsView {
    /// Current project slug.
    pub project: String,
    /// Sessions ordered by the gateway's existing priority rules.
    pub sessions: Vec<SessionRow>,
    /// Accessible sessions in other projects.
    pub elsewhere: usize,
    /// Detached session bodies rendered in the expandable tail.
    pub detached: Vec<DetachedSessionRow>,
}

/// One project-list row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    /// Project slug.
    pub slug: String,
    /// Project directory.
    pub path: String,
    /// Number of visible live sessions.
    pub sessions: usize,
    /// Whether this is the caller's current project.
    pub current: bool,
}

/// Facts needed to render the project picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectsView {
    /// Projects visible to the caller.
    pub projects: Vec<ProjectRow>,
}

/// One filesystem entry rendered in the operator folder browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntryView {
    /// Directory name.
    pub name: String,
    /// Registered project slug, when this directory is already a project —
    /// ANY local project, regardless of whether `slug` is ACL-visible to the
    /// browsing chat (FS-SEC-R2-1: `slug` must reflect registration truthfully
    /// so a hidden project never renders as an empty, enterable folder).
    pub slug: Option<String>,
    /// `false` only when `slug` names a project this chat cannot see
    /// (`Gateway::visible_project_slugs`) — such an entry renders inert: no
    /// "✅ Переключиться" (it isn't this chat's to switch into) and no
    /// "📁 Открыть" either (it's registered, not a plain folder to browse
    /// into). Meaningless when `slug` is `None`.
    pub visible: bool,
}

/// Facts needed to render one page of the operator filesystem folder browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsBrowserView {
    /// Browser root, e.g. `/root/projects`.
    pub root_display: String,
    /// Path below the root currently shown; empty at the root.
    pub rel: PathBuf,
    /// Directory entries on this page.
    pub entries: Vec<FsEntryView>,
    /// Current page number, matching the displayed "k" in "страница k/n".
    pub page: usize,
    /// Total page count.
    pub pages: usize,
    /// The listed directory itself, when it is already a registered project.
    pub current_slug: Option<String>,
    /// The chat's current project slug; the matching entry renders bold.
    pub chat_project: Option<String>,
    /// Directory-read error, replacing the entry list when present.
    pub error: Option<String>,
    /// [`crate::fs_browser::fingerprint`] of `rel` + `entries` — embedded in
    /// each `nav:fs:i:<n>:<fp>`/`nav:fs:pick:<n>:<fp>` callback so a tap is
    /// validated against the exact page it was rendered from (FS-SEC-2).
    pub page_fingerprint: String,
}

/// One gateway command's help metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandView {
    /// Command name including the leading slash.
    pub name: String,
    /// Optional argument syntax.
    pub arg_hint: Option<String>,
    /// One-line Russian description.
    pub help: String,
}

/// Render the focused-session card.
pub fn render_status(view: &StatusView) -> RichReply {
    let mut details = view.detail_lines.clone();
    details.push(String::new());
    details.push(view.cost_24h.clone());
    details.push(String::new());
    details.push(format!("🔁 resume {}", view.resume));
    let mut markdown_detail_lines = Vec::with_capacity(details.len() * 2);
    let mut inline_rows = Vec::new();
    for (index, line) in details.iter().enumerate() {
        markdown_detail_lines.push(escape_status_markdown(line));
        if let Some(child) = view
            .children
            .iter()
            .find(|child| child.detail_line_index == index)
        {
            if let Some(stop) = command_button_styled(
                &format!("⛔ {}", child.sid),
                format!("?/stop {}", child.sid),
                Some(ButtonStyle::Danger),
            ) {
                let row = vec![stop];
                if let Some(rendered) = inline_button_row(row.iter().cloned().map(Some)) {
                    markdown_detail_lines.push(rendered);
                }
                inline_rows.push(row);
            }
        }
    }
    let markdown_details = markdown_detail_lines.join("\n");
    let mut markdown_lines = vec![
        format!(
            "🧭 {} · {} · {}",
            escape_status_markdown(&view.sid),
            escape_status_markdown(&view.project),
            escape_status_markdown(&view.vendor),
        ),
        format!(
            "{} · {} · {} · ctx {}",
            escape_status_markdown(&view.state),
            escape_status_markdown(&view.model),
            escape_status_markdown(&view.effort),
            escape_status_markdown(&view.context),
        ),
        format!("📁 {}", escape_status_markdown(&view.path)),
    ];
    if view.host != "local" && !view.host.is_empty() {
        markdown_lines.push(format!("🖥 host: {}", escape_status_markdown(&view.host)));
    }
    markdown_lines.push(String::new());
    markdown_lines.push(markdown_details);
    let mut markdown = markdown_lines.join("\n");
    let mut plain_lines = vec![
        format!("🧭 {} · {} · {}", view.sid, view.project, view.vendor),
        format!(
            "{} · {} · {} · ctx {}",
            view.state, view.model, view.effort, view.context
        ),
        format!("📁 {}", view.path),
    ];
    if view.host != "local" && !view.host.is_empty() {
        plain_lines.push(format!("🖥 host: {}", view.host));
    }
    plain_lines.push(String::new());
    plain_lines.extend(details);
    let plain = plain_lines.join("\n");
    let stop = command_button_styled(
        "⛔ Стоп",
        format!("?/stop {}", view.sid),
        Some(ButtonStyle::Danger),
    );
    let mut button_rows = vec![
        command_row([
            command_button("📋 Сессии", "/sessions"),
            command_button("📁 Проекты", "/projects"),
            redraw_button("🔄 Обновить", "/status"),
        ]),
        command_row([command_button("✏️ Новая", "?/new"), stop]),
    ];
    let mut trailing_rows = Vec::new();
    if !view.children.is_empty() {
        // `/stop children` (NOT `/stop all`, which stops every session
        // visible to this chat, including the parent itself) — direct
        // children of the CURRENT session only.
        if let Some(stop_all) = command_button_styled(
            "⛔ Остановить все дочерние",
            "?/stop children",
            Some(ButtonStyle::Danger),
        ) {
            trailing_rows.push(vec![stop_all]);
        }
    }
    let global_rows = button_rows.clone();
    let inline_buttons = !inline_rows.is_empty();
    button_rows = inline_rows;
    button_rows.extend(global_rows.clone());
    button_rows.extend(trailing_rows.clone());
    if inline_buttons {
        let mut all_trailing = global_rows;
        all_trailing.extend(trailing_rows);
        append_inline_rows(&mut markdown, &all_trailing);
    }
    RichReply {
        markdown,
        plain,
        button_rows,
        inline_buttons,
        reply_keyboard: None,
    }
}

/// Render compact session cards and their switch buttons.
pub fn render_sessions(view: &SessionsView) -> RichReply {
    let shown = view.sessions.iter().take(10).collect::<Vec<_>>();
    let hidden = view.sessions.iter().skip(10).collect::<Vec<_>>();
    let mut markdown = format!("**Сессии** · `{}`", escape_code(&view.project));
    let mut plain = format!("Сессии · {}", view.project);
    let mut button_rows = Vec::new();
    for session in &shown {
        markdown.push_str("\n\n");
        markdown.push_str(&markdown_session_line(session));
        button_rows.push(session_button_options(session));
        plain.push_str("\n\n");
        plain.push_str(&plain_session_line(session));
    }
    let mut tail = hidden
        .iter()
        .map(|row| plain_session_line(row))
        .collect::<Vec<_>>();
    tail.extend(view.detached.iter().map(|detached| {
        format!(
            "⏳ {} · pid {} — сообщения встанут в очередь; /stop {} завершит",
            detached.sid, detached.pid, detached.sid
        )
    }));
    if !tail.is_empty() {
        markdown.push_str("\n\n<blockquote expandable>");
        markdown.push_str(&tail.join("\n"));
        markdown.push_str("</blockquote>");
        plain.push_str("\nПодробнее:\n");
        plain.push_str(&tail.join("\n"));
    }
    let global_rows = vec![command_row([
        redraw_button("🔄 Обновить", "/sessions"),
        command_button("✏️ Новая", "?/new"),
    ])];
    let mut trailing_rows = global_rows.clone();
    if view.elsewhere > 0 {
        let footer = format!("ещё {} в других проектах → /sessions all", view.elsewhere);
        markdown.push_str(&format!("\n\n{footer}"));
        plain.push_str(&format!("\n{footer}"));
        if let Some(button) = command_button(
            &format!("Ещё {} → /sessions all", view.elsewhere),
            "/sessions all",
        ) {
            trailing_rows.push(vec![button]);
        }
    }
    button_rows.extend(trailing_rows.iter().cloned());
    let inline_buttons = !button_rows.is_empty() && !shown.is_empty();
    if inline_buttons {
        append_inline_rows(&mut markdown, &trailing_rows);
    }
    RichReply {
        markdown,
        plain,
        button_rows,
        inline_buttons,
        reply_keyboard: None,
    }
}

/// Render the compact project table and project-switch buttons.
pub fn render_projects(view: &ProjectsView) -> RichReply {
    let mut markdown = String::from("**Проекты**\n\n| slug | путь | сессий |\n| --- | --- | --- |");
    let mut plain = String::from("Проекты\nslug | путь | сессий");
    let mut buttons = Vec::new();
    for project in &view.projects {
        markdown.push_str(&format!(
            "\n| **{}** | `{}` | {} |",
            escape_markdown(&project.slug),
            escape_code(&project.path),
            project.sessions
        ));
        plain.push_str(&format!(
            "\n{} | {} | {}",
            project.slug, project.path, project.sessions
        ));
        let label = if project.current {
            format!("✓ {}", project.slug)
        } else {
            project.slug.clone()
        };
        if let Some(button) = command_button(&label, format!("/cd {}", project.slug)) {
            buttons.push(button);
        }
    }
    RichReply {
        markdown,
        plain,
        button_rows: buttons.chunks(8).map(|row| row.to_vec()).collect(),
        inline_buttons: false,
        reply_keyboard: None,
    }
}

/// Render one page of the operator filesystem folder browser.
pub fn render_fs_browser(view: &FsBrowserView) -> RichReply {
    let rel_display = view.rel.to_string_lossy();
    let rel_display = rel_display.trim_matches('/');
    let has_rel = !rel_display.is_empty();
    let header_plain = if has_rel {
        format!("📂 {}/{}", view.root_display, rel_display)
    } else {
        format!("📂 {}", view.root_display)
    };

    // R2-3 — `up`/`here` carry the SAME page fingerprint the `i:`/`pick:`
    // buttons do, so a tap on an older browser message is validated against
    // a fresh render of `rel`/`page` instead of acting on whatever the
    // chat's shared nav cursor currently points at (which a newer browser
    // message from the same chat may since have moved).
    let fp = &view.page_fingerprint;
    let up_button = has_rel
        .then(|| nav_option("⬆️ Вверх", format!("fs:up:{fp}"), None))
        .flatten();

    let mut markdown = escape_status_markdown(&header_plain);
    let mut plain = header_plain;
    let mut button_rows = Vec::new();
    if let Some(button) = &up_button {
        markdown.push(' ');
        markdown.push_str(&inline_text_button(button));
        button_rows.push(vec![button.clone()]);
    }

    if let Some(error) = &view.error {
        let error_line = format!("⛔ нет доступа: {error}");
        markdown.push_str("\n\n");
        markdown.push_str(&escape_status_markdown(&error_line));
        plain.push_str("\n\n");
        plain.push_str(&error_line);
        return RichReply {
            markdown,
            plain,
            button_rows,
            inline_buttons: true,
            reply_keyboard: None,
        };
    }

    if view.pages > 1 {
        let page_line = format!("страница {}/{}", view.page, view.pages);
        markdown.push('\n');
        markdown.push_str(&escape_status_markdown(&page_line));
        plain.push('\n');
        plain.push_str(&page_line);
    }

    for (index, entry) in view.entries.iter().enumerate() {
        markdown.push_str("\n\n");
        plain.push_str("\n\n");
        let bold = entry.slug.is_some() && view.chat_project.as_deref() == entry.slug.as_deref();
        let name_markdown = escape_status_markdown(&entry.name);
        let name_markdown = if bold {
            format!("**{name_markdown}**")
        } else {
            name_markdown
        };
        // FS-SEC-R2-1 — a registered-but-not-visible project (a tenant's,
        // hidden from this operator by `visible_project_slugs`) renders
        // inert: `icon` alone, no nav option at all. It must NOT fall into
        // the `entry.slug.is_none()` "📁 Открыть" branch (that would let
        // the operator descend into and then "make a project" of a
        // directory that already IS someone else's project) or the
        // "✅ Переключиться" branch (nothing to switch this chat into).
        let nav_choice = if entry.slug.is_some() && !entry.visible {
            None
        } else if entry.slug.is_some() {
            Some((
                "✅",
                "Переключиться",
                format!("fs:pick:{index}:{fp}"),
                "переключиться",
            ))
        } else {
            Some(("📁", "Открыть", format!("fs:i:{index}:{fp}"), "открыть"))
        };
        let icon = nav_choice.as_ref().map_or("🔒", |(icon, ..)| icon);
        markdown.push_str(icon);
        markdown.push(' ');
        markdown.push_str(&name_markdown);
        plain.push_str(icon);
        plain.push(' ');
        plain.push_str(&entry.name);
        if let Some((icon, label, action, classic_suffix)) = nav_choice {
            if let Some(button) = nav_option(label, action, None) {
                markdown.push(' ');
                markdown.push_str(&inline_text_button(&button));
                button_rows.push(vec![MessageOption {
                    label: format!("{icon} {} → {classic_suffix}", entry.name),
                    ..button
                }]);
            }
        }
    }

    let mut trailing = Vec::new();
    if view.pages > 1 {
        // SPEC-3.5 — both arrows always render when there's more than one
        // page (matching the root mock). Tapping ◀ on page 1 or ▶ on the
        // last page re-requests the page already on screen: `fs:pg:` clamps
        // into `1..=pages` (`fs_browser::list_capped`), and the daemon now
        // treats Telegram's resulting "message is not modified" as success
        // rather than falling back to a duplicate new message (F3's
        // original concern, since resolved) — so an edge tap is a harmless
        // no-op, not the broken send F3 was guarding against.
        if let Some(button) =
            nav_option("◀", format!("fs:pg:{}", view.page.saturating_sub(1)), None)
        {
            trailing.push(button);
        }
        if let Some(button) = nav_option("▶", format!("fs:pg:{}", view.page + 1), None) {
            trailing.push(button);
        }
    }
    if has_rel && view.current_slug.is_none() {
        if let Some(button) = nav_option("📌 Сделать проектом", format!("fs:here:{fp}"), None)
        {
            trailing.push(button);
        }
    }
    if !trailing.is_empty() {
        if let Some(rendered) = inline_button_row(trailing.iter().cloned().map(Some)) {
            markdown.push_str("\n\n");
            markdown.push_str(&rendered);
        }
        button_rows.push(trailing);
    }

    RichReply {
        markdown,
        plain,
        button_rows,
        inline_buttons: true,
        reply_keyboard: None,
    }
}

/// Render grouped gateway help from command metadata.
pub fn render_help(commands: &[CommandView]) -> RichReply {
    const GROUPS: [(&str, &[&str]); 3] = [
        (
            "Навигация",
            &[
                "/commander",
                "/status",
                "/sessions",
                "/projects",
                "/new",
                "/use",
                "/cd",
            ],
        ),
        (
            "Управление",
            &["/role", "/stop", "/interrupt", "/rename", "/newproject"],
        ),
        ("Прочее", &["/mcp", "/inbox", "/keys", "/help"]),
    ];
    let mut markdown = String::from("**Команды шлюза**");
    let mut plain = String::from("Команды шлюза");
    for (title, names) in GROUPS {
        let group = commands
            .iter()
            .filter(|command| names.contains(&command.name.as_str()))
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        markdown.push_str(&format!("\n\n**{title}**"));
        plain.push_str(&format!("\n\n{title}"));
        for command in group {
            let usage = command_usage(command);
            markdown.push_str(&format!(
                "\n`{}` — {}",
                escape_code(&usage),
                escape_markdown(&command.help)
            ));
            plain.push_str(&format!("\n{usage} — {}", command.help));
        }
    }
    markdown.push_str("\n\nЛюбая другая `/command` передаётся агенту текущей сессии.");
    plain.push_str("\n\nЛюбая другая /command передаётся агенту текущей сессии.");
    RichReply {
        markdown,
        plain,
        button_rows: Vec::new(),
        inline_buttons: false,
        reply_keyboard: None,
    }
}

fn command_row<const N: usize>(buttons: [Option<MessageOption>; N]) -> Vec<MessageOption> {
    buttons.into_iter().flatten().collect()
}

fn command_button(label: &str, command: impl AsRef<str>) -> Option<MessageOption> {
    command_button_styled(label, command, None)
}

/// A self-refresh button (§3.5c) — runs `command` exactly like
/// [`command_button`] does, but tags the callback `cmd:~<command>`
/// ([`crate::im_callbacks::CallbackAction::Redraw`]) instead of plain
/// `cmd:<command>`. R2-2 — this namespace is what lets the gateway edit the
/// tapped message in place: it is reserved for a screen's OWN "🔄 Обновить"
/// button, never for a button that merely happens to run the same command
/// from a DIFFERENT screen (e.g. the `/status` card's "📋 Сессии" link,
/// which must open a fresh `/sessions` message, not overwrite the card).
fn redraw_button(label: &str, command: impl AsRef<str>) -> Option<MessageOption> {
    let data = format!("cmd:~{}", command.as_ref());
    (data.len() <= 64).then(|| MessageOption {
        data,
        label: label.to_string(),
        id: command.as_ref().to_string(),
        style: None,
    })
}

fn command_button_styled(
    label: &str,
    command: impl AsRef<str>,
    style: Option<ButtonStyle>,
) -> Option<MessageOption> {
    let data = format!("cmd:{}", command.as_ref());
    (data.len() <= 64).then(|| MessageOption {
        data,
        label: label.to_string(),
        id: command.as_ref().to_string(),
        style,
    })
}

fn markdown_session_line(session: &SessionRow) -> String {
    let (icon, state) = session_state(session);
    let sid = session_display_sid(session);
    let vendor_model = session_vendor_model_compact(session);
    let prefix = format!("{icon} {sid} · {vendor_model} · {state}");
    let sid_markdown = if session.current {
        format!("**{}**", escape_markdown(&sid))
    } else {
        escape_markdown(&sid)
    };
    let mut line = format!(
        "{icon} {} · {} · {}{}",
        sid_markdown,
        escape_markdown(&vendor_model),
        escape_markdown(&state),
        session_title_suffix(&session_title(session), prefix.chars().count()),
    );
    for button in session_inline_button_options(session) {
        line.push(' ');
        line.push_str(&inline_text_button(&button));
    }
    line
}

/// The two inline paragraph buttons for a session line: bare `⛔` (danger)
/// and `Переключиться` (default → rendered `style="link"`), same callback
/// data as [`session_button_options`]'s classic-fallback labels.
fn session_inline_button_options(session: &SessionRow) -> Vec<MessageOption> {
    let mut buttons = session_button_options(session);
    if let Some(stop) = buttons.first_mut() {
        stop.label = "⛔".to_string();
    }
    if let Some(switch) = buttons.get_mut(1) {
        switch.label = "Переключиться".to_string();
    }
    buttons
}

/// One state icon + label for a session/child activity classification
/// (§3.5b) — the single source every place that renders session state reads
/// from, so `working`/`idle`/`stale`/`stuck` never render two contradicting
/// icons on the same line again (`/sessions` used to render `🔵 работает`,
/// activity labels `🟡 работает`, each classifier guessing on its own).
/// Exactly the spec's five rows — a session pinned to the top of
/// `/sessions` for a pending HITL approval still falls through to its real
/// `working`/`idle`/`stuck` classification here (SPEC-3.5b-waiting-arm —
/// an undocumented `waiting` arm used to alias `idle`'s "ожидание" word
/// under a different, ⏳, icon reserved for the restart/detached tail).
pub fn activity_badge(activity: &str) -> (&'static str, &'static str) {
    match activity {
        "working" => ("🔵", "работает"),
        "idle" => ("🟢", "ожидание"),
        "stale" => ("🟠", "устарело"),
        "stuck" => ("🔴", "зависание"),
        // SPEC-3.5b — `⏳` is deliberately absent here: it's reserved for
        // "session finishing a turn started before restart" / the detached
        // tail (neither is a live-session activity), never a member of this
        // palette. See `activity_badge_covers_the_documented_palette` below.
        _ => ("⚪", "неизвестно"),
    }
}

fn plain_session_line(session: &SessionRow) -> String {
    let (icon, label) = activity_badge(&session.status);
    let status_text = format!("{icon} {label}");
    let title = session_title(session);
    let title_or_context = if title == "—" {
        session.context.clone()
    } else {
        let prefix = format!(
            "{} | {} | {status_text} | ",
            session_display_sid(session),
            session_vendor_model(session),
        );
        truncate_session_title_to(&title, 60usize.saturating_sub(prefix.chars().count()))
    };
    format!(
        "{} | {} | {status_text} | {}",
        session_display_sid(session),
        session_vendor_model(session),
        title_or_context,
    )
}

fn session_display_sid(session: &SessionRow) -> String {
    if session.tree_depth == 0 {
        session.sid.clone()
    } else {
        format!("{}└─ {}", "   ".repeat(session.tree_depth - 1), session.sid)
    }
}

fn session_vendor_model(session: &SessionRow) -> String {
    match session.host.as_deref().filter(|host| !host.is_empty()) {
        Some(host) => format!("{} @{host}", session.vendor_model),
        None => session.vendor_model.clone(),
    }
}

fn session_vendor_model_compact(session: &SessionRow) -> String {
    let vendor_model = session_vendor_model(session);
    vendor_model
        .split_once('.')
        .map(|(vendor, model)| format!("{vendor}/{model}"))
        .unwrap_or(vendor_model)
}

fn session_title(session: &SessionRow) -> String {
    session
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .map(truncate_session_title)
        .unwrap_or_else(|| "—".to_string())
}

/// Icon + display text for one session line — the single place
/// `markdown_session_line` reads the leading icon from, so it can never
/// disagree with the state word next to it (F4/SPEC-2).
fn session_state(session: &SessionRow) -> (&'static str, String) {
    let (icon, label) = activity_badge(&session.status);
    let text = if session.context == "—" {
        label.to_string()
    } else {
        format!("{label} · ctx {}", session.context)
    };
    (icon, text)
}

fn truncate_session_title(title: &str) -> String {
    truncate_session_title_to(title, 20)
}

fn truncate_session_title_to(title: &str, max: usize) -> String {
    let title = title.trim();
    if title.chars().count() <= max {
        return title.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}…", title.chars().take(max - 1).collect::<String>())
}

fn session_button_options(session: &SessionRow) -> Vec<MessageOption> {
    command_row([
        command_button_styled(
            "⛔ Стоп",
            format!("?/stop {}", session.sid),
            Some(ButtonStyle::Danger),
        ),
        command_button("💬 Переключиться", format!("/use {}", session.sid)),
    ])
}

fn session_title_suffix(title: &str, prefix_len: usize) -> String {
    if title == "—" {
        String::new()
    } else {
        let max = 60usize.saturating_sub(prefix_len + 3).min(20);
        let title = truncate_session_title_to(title, max);
        if title.is_empty() {
            String::new()
        } else {
            format!(" — {}", escape_markdown(&title))
        }
    }
}

fn append_inline_rows(markdown: &mut String, rows: &[Vec<MessageOption>]) {
    for row in rows {
        if let Some(rendered) = inline_button_row(row.iter().cloned().map(Some)) {
            markdown.push_str("\n\n");
            markdown.push_str(&rendered);
        }
    }
}

fn inline_button_row<I>(buttons: I) -> Option<String>
where
    I: IntoIterator<Item = Option<MessageOption>>,
{
    let buttons = buttons.into_iter().flatten().collect::<Vec<_>>();
    if buttons.is_empty() {
        return None;
    }
    let mut row = String::from("<tg-button-row>");
    for button in buttons {
        row.push_str("<tg-button type=\"callback_data\" data=\"");
        row.push_str(&escape_button_attribute(&button.data));
        row.push('"');
        if let Some(style) = button.style {
            row.push_str(" style=\"");
            row.push_str(match style {
                ButtonStyle::Primary => "primary",
                ButtonStyle::Success => "success",
                ButtonStyle::Danger => "danger",
            });
            row.push('"');
        }
        row.push('>');
        row.push_str(&escape_button_attribute(&button.label));
        row.push_str("</tg-button>");
    }
    row.push_str("</tg-button-row>");
    Some(row)
}

/// Build a `nav:`-prefixed callback option for the filesystem browser
/// (mirrors [`command_button_styled`]'s `cmd:` prefix for gateway commands).
fn nav_option(
    label: &str,
    action: impl Into<String>,
    style: Option<ButtonStyle>,
) -> Option<MessageOption> {
    let action = action.into();
    let data = format!("nav:{action}");
    (data.len() <= 64).then(|| MessageOption {
        data,
        label: label.to_string(),
        id: action,
        style,
    })
}

/// Render one `<tg-button>` embedded mid-paragraph (RichTextButton), as
/// opposed to [`inline_button_row`]'s block `<tg-button-row>`. Defaults to
/// `style="link"` when the option carries no explicit style, matching the
/// Telegram Rich Messages convention for inline text buttons.
fn inline_text_button(option: &MessageOption) -> String {
    let mut tag = String::from("<tg-button type=\"callback_data\" data=\"");
    tag.push_str(&escape_button_attribute(&option.data));
    tag.push('"');
    let style = match option.style {
        Some(ButtonStyle::Primary) => "primary",
        Some(ButtonStyle::Success) => "success",
        Some(ButtonStyle::Danger) => "danger",
        None => "link",
    };
    tag.push_str(" style=\"");
    tag.push_str(style);
    tag.push('"');
    tag.push('>');
    tag.push_str(&escape_button_attribute(&option.label));
    tag.push_str("</tg-button>");
    tag
}

fn escape_button_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn command_usage(command: &CommandView) -> String {
    command
        .arg_hint
        .as_deref()
        .filter(|hint| !hint.is_empty())
        .map(|hint| format!("{} {hint}", command.name))
        .unwrap_or_else(|| command.name.clone())
}

fn escape_code(value: &str) -> String {
    value.replace('`', "\\`").replace(['\r', '\n'], " ")
}

pub(crate) fn escape_status_markdown(value: &str) -> String {
    escape_markdown(value)
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' | '*' | '_' | '`' | '|' | '[' | ']' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::transport::ButtonStyle;

    use super::{
        activity_badge, render_fs_browser, render_help, render_projects, render_sessions,
        render_status, CommandView, DetachedSessionRow, FsBrowserView, FsEntryView, ProjectRow,
        ProjectsView, SessionRow, SessionsView, StatusChild, StatusView,
    };

    /// SPEC-4/SPEC-3.5b — pins the whole documented palette (§3.5b's table
    /// plus the unknown fallback) so a future edit can't silently drift an
    /// icon or a label without a test failing, and can't re-add a
    /// `waiting`-shaped arm without deliberately updating this table too.
    #[test]
    fn activity_badge_covers_the_documented_palette() {
        assert_eq!(activity_badge("working"), ("🔵", "работает"));
        assert_eq!(activity_badge("idle"), ("🟢", "ожидание"));
        assert_eq!(activity_badge("stale"), ("🟠", "устарело"));
        assert_eq!(activity_badge("stuck"), ("🔴", "зависание"));
        assert_eq!(activity_badge("waiting"), ("⚪", "неизвестно"));
        assert_eq!(activity_badge("anything-else"), ("⚪", "неизвестно"));
    }

    /// Icons production code must never hardcode outside [`activity_badge`]
    /// itself: its five live outputs (`working`/`idle`/`stale`/`stuck` and
    /// the `⚪` "неизвестно" fallback) plus the retired `🟡`
    /// `waiting`-shaped icon SPEC-3.5b removed. The single list both guard
    /// tests below scan for, so they cannot drift apart (R2-3 — before this
    /// constant existed, each guard hand-rolled its own five-character
    /// array and both omitted `⚪`, so a bare `⚪ неизвестно` written
    /// directly into production code instead of routed through
    /// `activity_badge` passed both gates silently). Deliberately a
    /// TEST-ONLY constant, not exposed to production code: hoisting it
    /// there would put its own `⚪`/`🔵`/… literals inside the very
    /// production-code window `im_views_production_code_has_no_hardcoded_activity_icons`
    /// scans, self-tripping the guard it exists to serve.
    const ACTIVITY_ICON_GUARD_CHARS: [char; 6] = ['🔵', '🟢', '🟠', '🔴', '⚪', '🟡'];

    /// SPEC-4 — `gateway.rs`'s session/status render sites must source every
    /// state icon from [`activity_badge`], never re-inline one of its
    /// literals. Scans gateway.rs's PRODUCTION code (everything before its
    /// own `#[cfg(test)]\nmod tests {` — comment lines, which legitimately
    /// reference these icons in prose, are skipped) for
    /// [`ACTIVITY_ICON_GUARD_CHARS`] as bare characters; `waiting`'s old
    /// `🟡`-shaped hardcoding at gateway.rs:15230/15233/15240/20582
    /// (pre-fix) is exactly the class of regression this catches.
    #[test]
    fn gateway_production_code_has_no_hardcoded_activity_icons() {
        let source = include_str!("gateway.rs");
        let boundary = source
            .find("#[cfg(test)]\nmod tests {")
            .expect("gateway.rs must have its main `mod tests` block");
        let production = &source[..boundary];
        let offenders: Vec<&str> = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| ACTIVITY_ICON_GUARD_CHARS.iter().any(|c| line.contains(*c)))
            .collect();
        assert!(
            offenders.is_empty(),
            "gateway.rs production code hardcodes a state icon outside activity_badge: {offenders:?}"
        );
    }

    /// review-fix F4 — the same guard as
    /// `gateway_production_code_has_no_hardcoded_activity_icons`, but for
    /// THIS file: it renders `markdown_session_line`/`plain_session_line`,
    /// the `/sessions` row §3.5b names, so a literal icon slipped in here
    /// (e.g. a stray `🟡` pin) would re-introduce the exact
    /// two-icons-per-line drift §3.5b removed while the gateway.rs-only
    /// scan above stays green. Excludes `activity_badge`'s own body (the
    /// one legitimate place the palette literals live) and this file's
    /// `#[cfg(test)]` tail (assertions legitimately quote the icons).
    #[test]
    fn im_views_production_code_has_no_hardcoded_activity_icons() {
        let source = include_str!("im_views.rs");
        let boundary = source
            .find("#[cfg(test)]\nmod tests {")
            .expect("im_views.rs must have its main `mod tests` block");
        let production = &source[..boundary];
        let badge_start = production
            .find("pub fn activity_badge")
            .expect("activity_badge must still live in production code");
        let badge_end = production[badge_start..]
            .find("\n}\n")
            .map(|rel| badge_start + rel + "\n}\n".len())
            .expect("activity_badge must have a closing brace on its own line");
        let scanned = format!("{}{}", &production[..badge_start], &production[badge_end..]);
        let offenders: Vec<&str> = scanned
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| ACTIVITY_ICON_GUARD_CHARS.iter().any(|c| line.contains(*c)))
            .collect();
        assert!(
            offenders.is_empty(),
            "im_views.rs production code hardcodes a state icon outside activity_badge: {offenders:?}"
        );
    }

    #[test]
    fn status_has_russian_card_and_commands() {
        let reply = render_status(&StatusView {
            sid: "s42".into(),
            project: "ccteam".into(),
            vendor: "claude".into(),
            state: "🟢 ожидание".into(),
            model: "opus".into(),
            effort: "high".into(),
            context: "38%".into(),
            path: "/root/projects/ccteam".into(),
            host: "local".into(),
            detail_lines: vec![
                "Запущено: ожидает · Роль: reviewer".into(),
                "".into(),
                "⚡️ Использование:".into(),
                "CC: 5h 17% (19:00) · неделя 78% (06/29) · max".into(),
                "".into(),
                "👥 Дочерние (2):".into(),
                "  • s56 · codex · gpt-5.6-terra · 🔵 работает · title".into(),
                "".into(),
                "  • s57 · codex · gpt-5.6-luna · 🟢 ожидание · other".into(),
                "".into(),
                "↓ Другие сессии проекта: 2 → /sessions".into(),
                "↓ Все проекты: 3 → /projects".into(),
            ],
            cost_24h: "💰 Расход проекта 24ч: $1.23".into(),
            resume: "123e4567-e89b-12d3-a456-426614174000".into(),
            children: vec![StatusChild {
                sid: "s56".into(),
                detail_line_index: 6,
            }],
        });

        assert_eq!(
            reply.markdown,
            "🧭 s42 · ccteam · claude\n🟢 ожидание · opus · high · ctx 38%\n📁 /root/projects/ccteam\n\nЗапущено: ожидает · Роль: reviewer\n\n⚡️ Использование:\nCC: 5h 17% (19:00) · неделя 78% (06/29) · max\n\n👥 Дочерние (2):\n  • s56 · codex · gpt-5.6-terra · 🔵 работает · title\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/stop s56\" style=\"danger\">⛔ s56</tg-button></tg-button-row>\n\n  • s57 · codex · gpt-5.6-luna · 🟢 ожидание · other\n\n↓ Другие сессии проекта: 2 → /sessions\n↓ Все проекты: 3 → /projects\n\n💰 Расход проекта 24ч: $1.23\n\n🔁 resume 123e4567-e89b-12d3-a456-426614174000\n\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:/sessions\">📋 Сессии</tg-button><tg-button type=\"callback_data\" data=\"cmd:/projects\">📁 Проекты</tg-button><tg-button type=\"callback_data\" data=\"cmd:~/status\">🔄 Обновить</tg-button></tg-button-row>\n\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/new\">✏️ Новая</tg-button><tg-button type=\"callback_data\" data=\"cmd:?/stop s42\" style=\"danger\">⛔ Стоп</tg-button></tg-button-row>\n\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/stop children\" style=\"danger\">⛔ Остановить все дочерние</tg-button></tg-button-row>"
        );
        assert_eq!(reply.plain.lines().next(), Some("🧭 s42 · ccteam · claude"));
        assert!(reply
            .plain
            .contains("🔁 resume 123e4567-e89b-12d3-a456-426614174000"));
        assert_eq!(reply.button_rows[2][0].data, "cmd:?/new");
        assert_eq!(reply.button_rows[2][1].data, "cmd:?/stop s42");
        assert!(reply.markdown.contains("<tg-button-row>"));
        assert!(
            reply.markdown.find("s56").unwrap()
                < reply.markdown.find("data=\"cmd:?/stop s56\"").unwrap()
        );
        assert!(reply.plain.contains("s56"));
        assert!(!reply.plain.contains("<tg-button-row>"));
        assert_eq!(reply.button_rows[3][0].label, "⛔ Остановить все дочерние");
        assert_eq!(reply.button_rows[3][0].data, "cmd:?/stop children");
        assert_eq!(reply.button_rows[3][0].style, Some(ButtonStyle::Danger));
        assert!(reply.button_rows.iter().all(|row| row.len() <= 8));
        assert!(reply
            .button_rows
            .iter()
            .flatten()
            .all(|button| button.data.len() <= 64));
        assert_eq!(super::escape_markdown("x*y[]"), "x\\*y\\[\\]");
    }

    #[test]
    fn status_escapes_html_tags_in_child_titles() {
        let reply = render_status(&StatusView {
            sid: "s78".into(),
            project: "ccteam".into(),
            vendor: "codex".into(),
            state: "🟢 ожидание".into(),
            model: "gpt-5.6-terra".into(),
            effort: "—".into(),
            context: "—".into(),
            path: "/tmp/ccteam".into(),
            host: "local".into(),
            detail_lines: vec!["  • s79 · x</blockquote><tg-button-row>".into()],
            cost_24h: "💰 Расход проекта 24ч: $0.00".into(),
            resume: "—".into(),
            children: Vec::new(),
        });

        assert!(!reply.markdown.contains("<blockquote"));
        assert!(!reply.markdown.contains("</blockquote>"));
        assert!(!reply.markdown.contains("🖥 host: local"));
        assert!(!reply.markdown.contains("<tg-button-row>"));
        assert!(reply
            .markdown
            .contains("&lt;/blockquote&gt;&lt;tg-button-row&gt;"));
    }

    #[test]
    fn status_shows_non_local_host() {
        let reply = render_status(&StatusView {
            sid: "s1".into(),
            project: "ccteam".into(),
            vendor: "codex".into(),
            state: "🟢 ожидание".into(),
            model: "gpt-5.6-terra".into(),
            effort: "medium".into(),
            context: "23%".into(),
            path: "/root/projects/ccteam".into(),
            host: "edge".into(),
            detail_lines: vec!["Запущено: ожидание · Роль: —".into()],
            cost_24h: "💰 Расход проекта 24ч: $0.00".into(),
            resume: "—".into(),
            children: Vec::new(),
        });

        assert!(reply.markdown.contains("🖥 host: edge"));
        assert!(reply.plain.contains("🖥 host: edge"));
    }

    #[test]
    fn sessions_are_compact_and_button_rows_stay_within_telegram_limits() {
        let sessions = (1..=11)
            .map(|n| SessionRow {
                sid: format!("s{n}"),
                vendor_model: "claude.opus".into(),
                status: if n == 1 { "working" } else { "idle" }.into(),
                context: "38%".into(),
                title: (n == 1).then(|| "active work".into()),
                current: n == 1,
                tree_depth: usize::from(n == 2),
                host: (n == 2).then(|| "edge".into()),
            })
            .collect();
        let reply = render_sessions(&SessionsView {
            project: "ccteam".into(),
            sessions,
            elsewhere: 2,
            detached: vec![DetachedSessionRow {
                sid: "s77".into(),
                pid: 4242,
            }],
        });

        assert!(!reply.markdown.contains("| sid |"));
        assert!(reply
            .markdown
            .contains("🔵 **s1** · claude/opus · работает · ctx 38% — active work"));
        assert!(reply.markdown.contains("data=\"cmd:?/stop s1\""));
        assert!(reply.markdown.contains("data=\"cmd:/use s1\""));
        assert!(reply.markdown.contains("<blockquote expandable>"));
        assert!(reply
            .markdown
            .contains("🟢 s10 · claude/opus · ожидание · ctx 38%"));
        assert!(reply
            .markdown
            .contains("🟢 └─ s2 · claude/opus @edge · ожидание · ctx 38%"));
        assert!(reply.markdown.contains("pid 4242"));
        assert!(reply
            .markdown
            .contains("сообщения встанут в очередь; /stop s77 завершит"));
        assert_eq!(reply.button_rows[10][0].data, "cmd:~/sessions");
        assert_eq!(reply.button_rows[10][1].data, "cmd:?/new");
        assert!(reply
            .plain
            .contains("ещё 2 в других проектах → /sessions all"));
        assert!(!reply.plain.contains("<tg-button-row>"));
        assert!(reply.button_rows.iter().all(|row| row.len() <= 8));
        assert!(reply
            .button_rows
            .iter()
            .flatten()
            .all(|button| button.data.len() <= 64));
    }

    #[test]
    fn sessions_render_exact_inline_cards_and_one_global_row() {
        let reply = render_sessions(&SessionsView {
            project: "ccteam".into(),
            sessions: vec![
                SessionRow {
                    sid: "s1".into(),
                    vendor_model: "claude.opus-4-8".into(),
                    status: "idle".into(),
                    context: "38%".into(),
                    title: Some("active work".into()),
                    current: true,
                    tree_depth: 0,
                    host: None,
                },
                SessionRow {
                    sid: "s2".into(),
                    vendor_model: "codex.gpt-5.6-terra".into(),
                    status: "working".into(),
                    context: "—".into(),
                    title: None,
                    current: false,
                    tree_depth: 0,
                    host: None,
                },
            ],
            elsewhere: 0,
            detached: Vec::new(),
        });

        assert_eq!(
            reply.markdown,
            concat!(
                "**Сессии** · `ccteam`\n\n",
                "🟢 **s1** · claude/opus-4-8 · ожидание · ctx 38% — active work ",
                "<tg-button type=\"callback_data\" data=\"cmd:?/stop s1\" style=\"danger\">⛔</tg-button> ",
                "<tg-button type=\"callback_data\" data=\"cmd:/use s1\" style=\"link\">Переключиться</tg-button>\n\n",
                "🔵 s2 · codex/gpt-5.6-terra · работает ",
                "<tg-button type=\"callback_data\" data=\"cmd:?/stop s2\" style=\"danger\">⛔</tg-button> ",
                "<tg-button type=\"callback_data\" data=\"cmd:/use s2\" style=\"link\">Переключиться</tg-button>\n\n",
                "<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:~/sessions\">🔄 Обновить</tg-button><tg-button type=\"callback_data\" data=\"cmd:?/new\">✏️ Новая</tg-button></tg-button-row>"
            )
        );
        assert_eq!(reply.button_rows[0][0].data, "cmd:?/stop s1");
        assert_eq!(reply.button_rows[0][0].label, "⛔ Стоп");
        assert_eq!(reply.button_rows[0][1].data, "cmd:/use s1");
        assert_eq!(reply.button_rows[0][1].label, "💬 Переключиться");
        assert!(!reply.plain.contains("<tg-button-row>"));
    }

    /// TG-FMT-1 review round 1 — bold lives ONLY in `.markdown` (the Telegram
    /// Rich Messages field); `.plain` is the universal fallback every
    /// channel (Lark, Slack, the classic Telegram send) reads verbatim and
    /// must stay unformatted. `render_markdown` (the Telegram classic-path
    /// HTML converter) must strip rich-only button tags while escaping
    /// HTML-special characters riding along in the card.
    #[test]
    fn sessions_markdown_bold_sid_survives_html_special_chars_in_other_cells() {
        let sessions = vec![SessionRow {
            sid: "s1".into(),
            vendor_model: "claude.<script>&\"".into(),
            status: "idle".into(),
            context: "38%".into(),
            title: None,
            current: true,
            tree_depth: 0,
            host: None,
        }];
        let reply = render_sessions(&SessionsView {
            project: "ccteam".into(),
            sessions,
            elsewhere: 0,
            detached: Vec::new(),
        });
        assert!(
            !reply.plain.contains('*'),
            ".plain must stay unformatted for non-Telegram channels: {}",
            reply.plain
        );
        let html = crate::telegram_html::render_markdown(&reply.markdown).html;
        assert!(html.contains("<b>s1</b>"), "sid must stay bold: {html}");
        assert!(!html.contains("tg-button"), "rich controls leaked: {html}");
        assert!(
            html.contains("claude/&lt;script&gt;&amp;\""),
            "special chars must be escaped, not left raw: {html}"
        );
        assert!(!html.contains("<script>"), "unescaped tag leaked: {html}");
    }

    #[test]
    fn fs_browser_root_page_has_no_up_or_here_but_has_pager() {
        let reply = render_fs_browser(&FsBrowserView {
            root_display: "/root/projects".into(),
            rel: PathBuf::new(),
            entries: vec![
                FsEntryView {
                    name: "4g".into(),
                    slug: Some("4g".into()),
                    visible: true,
                },
                FsEntryView {
                    name: "ccteam".into(),
                    slug: Some("ccteam".into()),
                    visible: true,
                },
                FsEntryView {
                    name: "RKN".into(),
                    slug: None,
                    visible: true,
                },
            ],
            page: 1,
            pages: 6,
            current_slug: None,
            chat_project: Some("ccteam".into()),
            error: None,
            page_fingerprint: "abcd".into(),
        });

        assert!(reply
            .markdown
            .starts_with("📂 /root/projects\nстраница 1/6"));
        assert!(!reply.markdown.contains("nav:fs:up"));
        assert!(!reply.markdown.contains("nav:fs:here"));
        // SPEC-3.5 — page 1 of 6: both arrows render (◀ clamps back to page
        // 1, a harmless no-op re-render — see the F3 comment in
        // `render_fs_browser`).
        assert!(reply.markdown.contains("data=\"nav:fs:pg:0\""));
        assert!(reply.markdown.contains("data=\"nav:fs:pg:2\""));
        assert!(reply.markdown.contains("**ccteam**"));
        assert!(reply.markdown.contains("data=\"nav:fs:pick:0:abcd\""));
        assert!(reply.markdown.contains("data=\"nav:fs:pick:1:abcd\""));
        assert!(reply.markdown.contains("data=\"nav:fs:i:2:abcd\""));
        assert!(reply.plain.contains("страница 1/6"));
        assert!(!reply.plain.contains('<'));
        assert_eq!(reply.button_rows[0][0].data, "nav:fs:pick:0:abcd");
        assert_eq!(reply.button_rows[1][0].data, "nav:fs:pick:1:abcd");
        assert_eq!(reply.button_rows[2][0].data, "nav:fs:i:2:abcd");
        assert_eq!(reply.button_rows[2][0].label, "📁 RKN → открыть");
        let pager = &reply.button_rows[3];
        assert_eq!(pager.len(), 2, "page 1 of 6 must render both ◀ and ▶");
        assert_eq!(pager[0].data, "nav:fs:pg:0");
        assert_eq!(pager[1].data, "nav:fs:pg:2");
        assert!(reply.inline_buttons);
    }

    #[test]
    fn fs_browser_last_page_pager_has_both_buttons() {
        let reply = render_fs_browser(&FsBrowserView {
            root_display: "/root/projects".into(),
            rel: PathBuf::new(),
            entries: vec![FsEntryView {
                name: "zzz".into(),
                slug: None,
                visible: true,
            }],
            page: 3,
            pages: 3,
            current_slug: None,
            chat_project: None,
            error: None,
            page_fingerprint: "beef".into(),
        });
        let pager = reply.button_rows.last().unwrap();
        // SPEC-3.5 — the last page still renders ▶ (clamps back to page 3,
        // a harmless re-render no-op).
        assert_eq!(pager.len(), 2, "last page must render both ◀ and ▶");
        assert_eq!(pager[0].data, "nav:fs:pg:2");
        assert_eq!(pager[1].data, "nav:fs:pg:4");
    }

    #[test]
    fn fs_browser_nested_page_has_up_and_here_but_no_pager() {
        let reply = render_fs_browser(&FsBrowserView {
            root_display: "/root/projects".into(),
            rel: PathBuf::from("ccteam-wt"),
            entries: vec![
                FsEntryView {
                    name: "dev".into(),
                    slug: None,
                    visible: true,
                },
                FsEntryView {
                    name: "fs-browser".into(),
                    slug: None,
                    visible: true,
                },
            ],
            page: 1,
            pages: 1,
            current_slug: None,
            chat_project: None,
            error: None,
            page_fingerprint: "abcd".into(),
        });

        assert!(reply
            .markdown
            .starts_with("📂 /root/projects/ccteam-wt <tg-button"));
        assert!(reply.markdown.contains("data=\"nav:fs:up:abcd\""));
        assert!(!reply.markdown.contains("страница"));
        assert!(reply.markdown.contains("data=\"nav:fs:here:abcd\""));
        assert!(!reply.markdown.contains("nav:fs:pg:"));
        assert_eq!(reply.button_rows[0][0].data, "nav:fs:up:abcd");
        let last = reply.button_rows.last().unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].data, "nav:fs:here:abcd");
    }

    #[test]
    fn fs_browser_hidden_registered_entry_is_inert() {
        // FS-SEC-R2-1 — a directory that IS a registered project, but not
        // visible to this chat (a tenant-owned project the operator can't
        // see), must render as a locked, non-interactive entry: no
        // "✅ Переключиться" (nothing to switch this chat into) and no
        // "📁 Открыть" either (it's a real project, not a plain folder the
        // operator should be offered to bootstrap over).
        let reply = render_fs_browser(&FsBrowserView {
            root_display: "/root/projects".into(),
            rel: PathBuf::new(),
            entries: vec![FsEntryView {
                name: "tenant-secret".into(),
                slug: Some("tenant-secret".into()),
                visible: false,
            }],
            page: 1,
            pages: 1,
            current_slug: None,
            chat_project: None,
            error: None,
            page_fingerprint: "abcd".into(),
        });

        assert!(reply.markdown.contains("🔒"));
        assert!(reply.markdown.contains("tenant-secret"));
        assert!(!reply.markdown.contains("fs:pick:"));
        assert!(!reply.markdown.contains("fs:i:"));
        assert!(
            reply.button_rows.is_empty(),
            "a hidden-registered entry offers no button at all: {:?}",
            reply.button_rows
        );
    }

    #[test]
    fn fs_browser_registered_dir_in_current_project_suppresses_here() {
        let reply = render_fs_browser(&FsBrowserView {
            root_display: "/root/projects".into(),
            rel: PathBuf::from("ccteam"),
            entries: Vec::new(),
            page: 1,
            pages: 1,
            current_slug: Some("ccteam".into()),
            chat_project: None,
            error: None,
            page_fingerprint: "abcd".into(),
        });

        assert!(!reply.markdown.contains("nav:fs:here"));
        assert!(reply
            .button_rows
            .iter()
            .all(|row| row[0].data != "nav:fs:here"));
    }

    #[test]
    fn fs_browser_read_error_replaces_entries_and_drops_nav_row() {
        let reply = render_fs_browser(&FsBrowserView {
            root_display: "/root/projects".into(),
            rel: PathBuf::from("secret"),
            entries: Vec::new(),
            page: 1,
            pages: 1,
            current_slug: None,
            chat_project: None,
            error: Some("Permission denied (os error 13)".into()),
            page_fingerprint: "abcd".into(),
        });

        assert!(reply
            .markdown
            .contains("⛔ нет доступа: Permission denied (os error 13)"));
        assert!(reply.markdown.contains("data=\"nav:fs:up:abcd\""));
        assert!(!reply.markdown.contains("nav:fs:here"));
        assert_eq!(reply.button_rows.len(), 1);
        assert_eq!(reply.button_rows[0][0].data, "nav:fs:up:abcd");
        assert!(reply
            .plain
            .contains("⛔ нет доступа: Permission denied (os error 13)"));
    }

    #[test]
    fn projects_render_a_table_and_switch_commands() {
        let reply = render_projects(&ProjectsView {
            projects: vec![ProjectRow {
                slug: "ccteam".into(),
                path: "/root/projects/ccteam".into(),
                sessions: 2,
                current: true,
            }],
        });

        assert!(reply
            .markdown
            .contains("| **ccteam** | `/root/projects/ccteam` | 2 |"));
        assert_eq!(reply.button_rows[0][0].data, "cmd:/cd ccteam");
        assert_eq!(reply.button_rows[0][0].label, "✓ ccteam");
        assert_eq!(
            reply.plain,
            "Проекты\nslug | путь | сессий\nccteam | /root/projects/ccteam | 2"
        );
    }

    #[test]
    fn help_groups_russian_command_specs() {
        let reply = render_help(&[
            CommandView {
                name: "/status".into(),
                arg_hint: None,
                help: "состояние сессии".into(),
            },
            CommandView {
                name: "/stop".into(),
                arg_hint: Some("<id>".into()),
                help: "остановить сессию".into(),
            },
        ]);

        assert!(reply
            .markdown
            .contains("**Навигация**\n`/status` — состояние сессии"));
        assert!(reply
            .markdown
            .contains("**Управление**\n`/stop <id>` — остановить сессию"));
        assert!(reply.plain.contains("Команды шлюза"));
        assert!(reply.button_rows.is_empty());
    }
}
