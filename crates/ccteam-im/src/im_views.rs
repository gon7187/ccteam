//! Pure rich-message renderers for the IM gateway views.

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
    /// Current lifecycle label.
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
            command_button("🔄 Обновить", "/status"),
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
        let row = session_button_options(session);
        if !row.is_empty() {
            markdown.push('\n');
            markdown.push_str(
                &inline_button_row(row.iter().cloned().map(Some)).expect("non-empty row"),
            );
            button_rows.push(row);
        }
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
        command_button("🔄 Обновить", "/sessions"),
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

/// Render grouped gateway help from command metadata.
pub fn render_help(commands: &[CommandView]) -> RichReply {
    const GROUPS: [(&str, &[&str]); 3] = [
        (
            "Навигация",
            &["/status", "/sessions", "/projects", "/new", "/use", "/cd"],
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
    let sid = session_display_sid(session);
    let vendor_model = session_vendor_model_compact(session);
    let state = session_state(session);
    let prefix = format!("{sid} · {vendor_model} · {state}");
    format!(
        "**{}** · {} · {}{}",
        escape_markdown(&sid),
        escape_markdown(&vendor_model),
        escape_markdown(&state),
        session_title_suffix(&session_title(session), prefix.chars().count()),
    )
}

fn plain_session_line(session: &SessionRow) -> String {
    let title = session_title(session);
    let title_or_context = if title == "—" {
        session.context.clone()
    } else {
        let prefix = format!(
            "{} | {} | {} | ",
            session_display_sid(session),
            session_vendor_model(session),
            session.status
        );
        truncate_session_title_to(&title, 60usize.saturating_sub(prefix.chars().count()))
    };
    format!(
        "{} | {} | {} | {}",
        session_display_sid(session),
        session_vendor_model(session),
        session.status,
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

fn session_state(session: &SessionRow) -> String {
    if session.context == "—" {
        session.status.clone()
    } else {
        format!("{} · ctx {}", session.status, session.context)
    }
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

fn escape_status_markdown(value: &str) -> String {
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
    use crate::transport::ButtonStyle;

    use super::{
        render_help, render_projects, render_sessions, render_status, CommandView,
        DetachedSessionRow, ProjectRow, ProjectsView, SessionRow, SessionsView, StatusChild,
        StatusView,
    };

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
                "  • s56 · codex · gpt-5.6-terra · 🟡 работает · title".into(),
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
            "🧭 s42 · ccteam · claude\n🟢 ожидание · opus · high · ctx 38%\n📁 /root/projects/ccteam\n\nЗапущено: ожидает · Роль: reviewer\n\n⚡️ Использование:\nCC: 5h 17% (19:00) · неделя 78% (06/29) · max\n\n👥 Дочерние (2):\n  • s56 · codex · gpt-5.6-terra · 🟡 работает · title\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/stop s56\" style=\"danger\">⛔ s56</tg-button></tg-button-row>\n\n  • s57 · codex · gpt-5.6-luna · 🟢 ожидание · other\n\n↓ Другие сессии проекта: 2 → /sessions\n↓ Все проекты: 3 → /projects\n\n💰 Расход проекта 24ч: $1.23\n\n🔁 resume 123e4567-e89b-12d3-a456-426614174000\n\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:/sessions\">📋 Сессии</tg-button><tg-button type=\"callback_data\" data=\"cmd:/projects\">📁 Проекты</tg-button><tg-button type=\"callback_data\" data=\"cmd:/status\">🔄 Обновить</tg-button></tg-button-row>\n\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/new\">✏️ Новая</tg-button><tg-button type=\"callback_data\" data=\"cmd:?/stop s42\" style=\"danger\">⛔ Стоп</tg-button></tg-button-row>\n\n<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/stop children\" style=\"danger\">⛔ Остановить все дочерние</tg-button></tg-button-row>"
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
                status: if n == 1 {
                    "⏳ ожидание"
                } else {
                    "🟢 ожидание"
                }
                .into(),
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
            .contains("**s1** · claude/opus · ⏳ ожидание · ctx 38% — active work"));
        assert!(reply.markdown.contains("data=\"cmd:?/stop s1\""));
        assert!(reply.markdown.contains("data=\"cmd:/use s1\""));
        assert!(reply.markdown.contains("<blockquote expandable>"));
        assert!(reply
            .markdown
            .contains("**s10** · claude/opus · 🟢 ожидание · ctx 38%"));
        assert!(reply
            .markdown
            .contains("**└─ s2** · claude/opus @edge · 🟢 ожидание · ctx 38%"));
        assert!(reply.markdown.contains("pid 4242"));
        assert!(reply
            .markdown
            .contains("сообщения встанут в очередь; /stop s77 завершит"));
        assert_eq!(reply.button_rows[10][0].data, "cmd:/sessions");
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
                    status: "🟢 ожидание".into(),
                    context: "38%".into(),
                    title: Some("active work".into()),
                    current: true,
                    tree_depth: 0,
                    host: None,
                },
                SessionRow {
                    sid: "s2".into(),
                    vendor_model: "codex.gpt-5.6-terra".into(),
                    status: "🔵 работает".into(),
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
                "**s1** · claude/opus-4-8 · 🟢 ожидание · ctx 38% — active work\n",
                "<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/stop s1\" style=\"danger\">⛔ Стоп</tg-button><tg-button type=\"callback_data\" data=\"cmd:/use s1\">💬 Переключиться</tg-button></tg-button-row>\n\n",
                "**s2** · codex/gpt-5.6-terra · 🔵 работает\n",
                "<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:?/stop s2\" style=\"danger\">⛔ Стоп</tg-button><tg-button type=\"callback_data\" data=\"cmd:/use s2\">💬 Переключиться</tg-button></tg-button-row>\n\n",
                "<tg-button-row><tg-button type=\"callback_data\" data=\"cmd:/sessions\">🔄 Обновить</tg-button><tg-button type=\"callback_data\" data=\"cmd:?/new\">✏️ Новая</tg-button></tg-button-row>"
            )
        );
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
            status: "🟢 ожидание".into(),
            context: "38%".into(),
            title: None,
            current: false,
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
