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
    /// Running or waiting detail.
    pub started: String,
    /// Accounted 24-hour cost when the gateway has one.
    pub cost_24h: Option<String>,
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
    let cost = view.cost_24h.as_deref().unwrap_or("—");
    let markdown = format!(
        "🧭 **{}** · {} · {}\n{} · {} · {} · ctx {}\n📁 `{}`\n🖥 host: {}\n<blockquote expandable>Запущено: {} / Расход 24ч: {}</blockquote>",
        escape_markdown(&view.sid),
        escape_markdown(&view.project),
        escape_markdown(&view.vendor),
        escape_markdown(&view.state),
        escape_markdown(&view.model),
        escape_markdown(&view.effort),
        escape_markdown(&view.context),
        escape_code(&view.path),
        escape_markdown(&view.host),
        escape_markdown(&view.started),
        escape_markdown(cost),
    );
    let plain = format!(
        "🧭 {} · {} · {}\n{} · {} · {} · ctx {}\n📁 {}\n🖥 host: {}\nЗапущено: {} / Расход 24ч: {}",
        view.sid,
        view.project,
        view.vendor,
        view.state,
        view.model,
        view.effort,
        view.context,
        view.path,
        view.host,
        view.started,
        cost,
    );
    let stop = command_button_styled(
        "⛔ Стоп",
        format!("?/stop {}", view.sid),
        Some(ButtonStyle::Danger),
    );
    RichReply {
        markdown,
        plain,
        button_rows: vec![
            command_row([
                command_button("📋 Сессии", "/sessions"),
                command_button("📁 Проекты", "/projects"),
                command_button("🔄 Обновить", "/status"),
            ]),
            command_row([command_button("✏️ Новая", "/new"), stop]),
        ],
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
    if !hidden.is_empty() {
        markdown.push_str("\n\n<blockquote expandable>");
        markdown.push_str(
            &hidden
                .iter()
                .map(|row| plain_session_line(row))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        markdown.push_str("</blockquote>");
        plain.push_str("\nЕщё сессии:\n");
        plain.push_str(
            &hidden
                .iter()
                .map(|row| plain_session_line(row))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    let mut button_rows = shown
        .iter()
        .filter_map(|session| {
            command_button(
                &format!("{} ▶", session.sid),
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
            "\n| `{}` | `{}` | {} |",
            escape_code(&project.slug),
            escape_code(&project.path),
            project.sessions
        ));
        plain.push_str(&format!(
            "\n{} | {} | {}",
            project.slug, project.path, project.sessions
        ));
        if let Some(button) = command_button(&project.slug, format!("/cd {}", project.slug)) {
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
        "\n| `{}` | {} | {} | {} |",
        escape_code(&session.sid),
        escape_markdown(&session.vendor_model),
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
        session.sid, session.vendor_model, session.status, session.context
    )
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
    use super::{
        render_help, render_projects, render_sessions, render_status, CommandView, ProjectRow,
        ProjectsView, SessionRow, SessionsView, StatusView,
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
            started: "ожидает".into(),
            cost_24h: None,
        });

        assert_eq!(
            reply.markdown,
            "🧭 **s42** · ccteam · claude\n🟢 ожидание · opus · high · ctx 38%\n📁 `/root/projects/ccteam`\n🖥 host: local\n<blockquote expandable>Запущено: ожидает / Расход 24ч: —</blockquote>"
        );
        assert_eq!(reply.plain.lines().next(), Some("🧭 s42 · ccteam · claude"));
        assert_eq!(reply.button_rows[1][1].data, "cmd:?/stop s42");
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
            })
            .collect();
        let reply = render_sessions(&SessionsView {
            project: "ccteam".into(),
            sessions,
            elsewhere: 2,
        });

        assert!(reply
            .markdown
            .contains("| `s1` | claude.opus | ⏳ ожидание | 38% |"));
        assert!(reply.markdown.contains("<blockquote expandable>"));
        assert!(reply
            .markdown
            .contains("s11 | claude.opus | 🟢 ожидание | 38%"));
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

    #[test]
    fn projects_render_a_table_and_switch_commands() {
        let reply = render_projects(&ProjectsView {
            projects: vec![ProjectRow {
                slug: "ccteam".into(),
                path: "/root/projects/ccteam".into(),
                sessions: 2,
            }],
        });

        assert!(reply
            .markdown
            .contains("| `ccteam` | `/root/projects/ccteam` | 2 |"));
        assert_eq!(reply.button_rows[0][0].data, "cmd:/cd ccteam");
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
