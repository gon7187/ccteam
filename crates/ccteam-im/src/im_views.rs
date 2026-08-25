//! Pure rich-message renderers for the IM gateway views.

use crate::transport::{ButtonStyle, MessageOption};

/// A rendered response with Telegram-rich and universal plain representations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichReply {
    /// Rich Messages Markdown sent to Telegram after the transport merge.
    pub markdown: String,
    /// Plain fallback for every channel and failed rich send.
    pub plain: String,
    /// Telegram button rows, with at most eight buttons per row.
    pub button_rows: Vec<Vec<MessageOption>>,
}

impl RichReply {
    /// Wrap a legacy plain response without interactive controls.
    pub fn plain(plain: impl Into<String>) -> Self {
        let plain = plain.into();
        Self {
            markdown: plain.clone(),
            plain,
            button_rows: Vec::new(),
        }
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
    /// Expandable low-priority facts, one line each.
    pub detail_lines: Vec<String>,
    /// Project-scoped trailing 24-hour cost from the progress ledger.
    pub cost_24h: String,
    /// Direct children eligible for one-tap confirmed stopping.
    pub child_stop_sids: Vec<String>,
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
    details.push(view.cost_24h.clone());
    let markdown_details = details
        .iter()
        .map(|line| escape_markdown(line))
        .collect::<Vec<_>>()
        .join("\n");
    let plain_details = details.join("\n");
    let markdown = format!(
        "🧭 **{}** · {} · {}\n{} · {} · {} · ctx {}\n📁 `{}`\n🖥 host: {}\n<blockquote expandable>{}</blockquote>",
        escape_markdown(&view.sid),
        escape_markdown(&view.project),
        escape_markdown(&view.vendor),
        escape_markdown(&view.state),
        escape_markdown(&view.model),
        escape_markdown(&view.effort),
        escape_markdown(&view.context),
        escape_code(&view.path),
        escape_markdown(&view.host),
        markdown_details,
    );
    let plain = format!(
        "🧭 {} · {} · {}\n{} · {} · {} · ctx {}\n📁 {}\n🖥 host: {}\n{}",
        view.sid,
        view.project,
        view.vendor,
        view.state,
        view.model,
        view.effort,
        view.context,
        view.path,
        view.host,
        plain_details,
    );
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
    let child_stops = view
        .child_stop_sids
        .iter()
        .filter_map(|sid| {
            command_button_styled(
                &format!("⛔ {sid}"),
                format!("?/stop {sid}"),
                Some(ButtonStyle::Danger),
            )
        })
        .collect::<Vec<_>>();
    button_rows.extend(child_stops.chunks(8).map(|row| row.to_vec()));
    if !view.child_stop_sids.is_empty() {
        // `/stop children` (NOT `/stop all`, which stops every session
        // visible to this chat, including the parent itself) — direct
        // children of the CURRENT session only.
        if let Some(stop_all) = command_button_styled(
            "⛔ Остановить все дочерние",
            "?/stop children",
            Some(ButtonStyle::Danger),
        ) {
            button_rows.push(vec![stop_all]);
        }
    }
    RichReply {
        markdown,
        plain,
        button_rows,
    }
}

/// Render the compact session table and its switch buttons.
pub fn render_sessions(view: &SessionsView) -> RichReply {
    let shown = view.sessions.iter().take(10).collect::<Vec<_>>();
    let hidden = view.sessions.iter().skip(10).collect::<Vec<_>>();
    let mut markdown = format!(
        "**Сессии** · `{}`\n\n| sid | vendor.model | статус | ctx |\n| --- | --- | --- | --- |",
        escape_code(&view.project)
    );
    let mut plain = format!(
        "Сессии · {}\nsid | vendor.model | статус | ctx",
        view.project
    );
    for session in &shown {
        markdown.push_str(&markdown_session_row(session));
        plain.push_str(&plain_session_row(session));
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
    let mut button_rows = shown
        .iter()
        .filter_map(|session| {
            let vendor = session
                .vendor_model
                .split('.')
                .next()
                .unwrap_or(session.vendor_model.as_str());
            let title = session
                .title
                .as_deref()
                .map(truncate_button_title)
                .filter(|title| !title.is_empty())
                .map(|title| format!(" · {title}"))
                .unwrap_or_default();
            command_button(
                &format!(
                    "{}{} {}{}",
                    if session.current { "▶ " } else { "" },
                    session.sid,
                    vendor,
                    title
                ),
                format!("/use {}", session.sid),
            )
        })
        .collect::<Vec<_>>()
        .chunks(8)
        .map(|row| row.to_vec())
        .collect::<Vec<_>>();
    if view.elsewhere > 0 {
        let footer = format!("ещё {} в других проектах → /sessions all", view.elsewhere);
        markdown.push_str(&format!("\n\n{footer}"));
        plain.push_str(&format!("\n{footer}"));
        if let Some(button) = command_button(
            &format!("Ещё {} → /sessions all", view.elsewhere),
            "/sessions all",
        ) {
            button_rows.push(vec![button]);
        }
    }
    RichReply {
        markdown,
        plain,
        button_rows,
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
        ("Прочее", &["/mcp", "/inbox", "/help"]),
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

fn markdown_session_row(session: &SessionRow) -> String {
    format!(
        "\n| **{}** | {} | {} | {} |",
        escape_markdown(&session_display_sid(session)),
        escape_markdown(&session_vendor_model(session)),
        escape_markdown(&session.status),
        escape_markdown(&session.context),
    )
}

fn plain_session_row(session: &SessionRow) -> String {
    format!("\n{}", plain_session_line(session))
}

fn plain_session_line(session: &SessionRow) -> String {
    format!(
        "{} | {} | {} | {}",
        session_display_sid(session),
        session_vendor_model(session),
        session.status,
        session.context
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

fn truncate_button_title(title: &str) -> String {
    const MAX: usize = 12;
    let title = title.trim();
    if title.chars().count() <= MAX {
        return title.to_string();
    }
    format!("{}…", title.chars().take(MAX - 1).collect::<String>())
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

fn escape_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' | '*' | '_' | '`' | '|' => {
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
        DetachedSessionRow, ProjectRow, ProjectsView, SessionRow, SessionsView, StatusView,
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
                "Запущено: ожидает".into(),
                "Роль: reviewer".into(),
                "resume 123e4567-e89b-12d3-a456-426614174000".into(),
                "🤖 Выполняется: workflow (1)".into(),
                "⚡ Использование: 5h 17% (→19:00)".into(),
                "👥 Прямые дочерние сессии: s56 · codex · gpt-5.6-terra · 🟡 работает · title"
                    .into(),
                "↓ Другие сессии проекта: 2 → /sessions".into(),
                "↓ Все проекты: 3 → /projects".into(),
            ],
            cost_24h: "Расход проекта 24ч: $1.23".into(),
            child_stop_sids: vec![
                "s56".into(),
                "s78".into(),
                "s79".into(),
                "s80".into(),
                "s81".into(),
                "s82".into(),
                "s83".into(),
                "s84".into(),
                "s85".into(),
            ],
        });

        assert_eq!(
            reply.markdown,
            "🧭 **s42** · ccteam · claude\n🟢 ожидание · opus · high · ctx 38%\n📁 `/root/projects/ccteam`\n🖥 host: local\n<blockquote expandable>Запущено: ожидает\nРоль: reviewer\nresume 123e4567-e89b-12d3-a456-426614174000\n🤖 Выполняется: workflow (1)\n⚡ Использование: 5h 17% (→19:00)\n👥 Прямые дочерние сессии: s56 · codex · gpt-5.6-terra · 🟡 работает · title\n↓ Другие сессии проекта: 2 → /sessions\n↓ Все проекты: 3 → /projects\nРасход проекта 24ч: $1.23</blockquote>"
        );
        assert_eq!(reply.plain.lines().next(), Some("🧭 s42 · ccteam · claude"));
        assert_eq!(reply.button_rows[1][0].data, "cmd:?/new");
        assert_eq!(reply.button_rows[1][1].data, "cmd:?/stop s42");
        assert_eq!(reply.button_rows[2][0].label, "⛔ s56");
        assert_eq!(reply.button_rows[2][0].data, "cmd:?/stop s56");
        assert_eq!(reply.button_rows[2][0].style, Some(ButtonStyle::Danger));
        assert_eq!(reply.button_rows[2].len(), 8);
        assert_eq!(reply.button_rows[3][0].label, "⛔ s85");
        assert_eq!(reply.button_rows[4][0].label, "⛔ Остановить все дочерние");
        assert_eq!(reply.button_rows[4][0].data, "cmd:?/stop children");
        assert_eq!(reply.button_rows[4][0].style, Some(ButtonStyle::Danger));
        assert!(reply.button_rows.iter().all(|row| row.len() <= 8));
        assert!(reply
            .button_rows
            .iter()
            .flatten()
            .all(|button| button.data.len() <= 64));
        assert_eq!(super::escape_markdown("x*y"), "x\\*y");
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

        assert!(reply
            .markdown
            .contains("| **s1** | claude.opus | ⏳ ожидание | 38% |"));
        assert!(reply.markdown.contains("<blockquote expandable>"));
        assert!(reply
            .markdown
            .contains("s11 | claude.opus | 🟢 ожидание | 38%"));
        assert!(reply.markdown.contains("| **└─ s2** | claude.opus @edge |"));
        assert!(reply.markdown.contains("pid 4242"));
        assert!(reply
            .markdown
            .contains("сообщения встанут в очередь; /stop s77 завершит"));
        assert_eq!(reply.button_rows[0][0].label, "▶ s1 claude · active work");
        assert!(reply
            .plain
            .contains("ещё 2 в других проектах → /sessions all"));
        assert!(reply.button_rows.iter().all(|row| row.len() <= 8));
        assert!(reply
            .button_rows
            .iter()
            .flatten()
            .all(|button| button.data.len() <= 64));
    }

    /// TG-FMT-1 review round 1 — bold lives ONLY in `.markdown` (the Telegram
    /// Rich Messages field); `.plain` is the universal fallback every
    /// channel (Lark, Slack, the classic Telegram send) reads verbatim and
    /// must stay unformatted. `render_markdown` (the Telegram classic-path
    /// HTML converter) must turn the sid's `**` into `<b>` while escaping
    /// HTML-special characters riding along in another cell, without
    /// corrupting the surrounding tag.
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
        assert!(html.contains("<b>s1</b>"), "got: {html}");
        assert!(
            html.contains("claude.&lt;script&gt;&amp;\""),
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
