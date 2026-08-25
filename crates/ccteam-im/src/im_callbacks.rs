//! Pure parsing for inline-button `callback_data` in the `cmd:` namespace
//! (TG-GATE-V2 W3). No `Gateway` borrow, no I/O — the gateway resolves the
//! parsed [`CallbackAction`] against chat state; this module only decides
//! *which* action a raw payload names.
//!
//! Namespaces (see `docs-local/versions/tg-gate-v2/brief.md`):
//! - `{token}:{idx}` / `nav:...` — the existing choice-reply and
//!   project/session picker payloads. Untouched by this module: they fall
//!   through as [`CallbackAction::Choice`] for the gateway's existing
//!   `resolve_selection` / `resolve_nav_selection` path.
//! - `cmd:<command>` — run `<command>` (with its args) exactly as if the
//!   user had typed it.
//! - `cmd:?<command>` — ask for confirmation before running `<command>`.
//! - `cmd:noop` — the confirmation prompt's "cancel" button.

/// One inline-button callback payload, parsed into the action it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackAction {
    /// Not a `cmd:` callback — the raw payload, unchanged, for the existing
    /// token/nav resolution path.
    Choice {
        /// The original `callback_data` string.
        data: String,
    },
    /// Run this command (with its args) exactly as typed text.
    Command(String),
    /// Ask for confirmation before running this command.
    Confirm(String),
    /// Cancel a pending confirmation.
    Noop,
}

/// Parse one inline-button `callback_data` payload into a [`CallbackAction`].
///
/// Only payloads under the `cmd:` namespace are interpreted here; everything
/// else (including today's `{token}:{idx}` choice replies and `nav:` picks)
/// comes back as [`CallbackAction::Choice`] untouched.
pub fn parse(data: &str) -> CallbackAction {
    let Some(rest) = data.strip_prefix("cmd:") else {
        return CallbackAction::Choice {
            data: data.to_string(),
        };
    };
    if rest == "noop" {
        return CallbackAction::Noop;
    }
    match rest.strip_prefix('?') {
        Some(cmd) => CallbackAction::Confirm(cmd.to_string()),
        None => CallbackAction::Command(rest.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_command() {
        assert_eq!(
            parse("cmd:/status"),
            CallbackAction::Command("/status".to_string())
        );
    }

    #[test]
    fn parses_command_with_args() {
        assert_eq!(
            parse("cmd:/stop s42"),
            CallbackAction::Command("/stop s42".to_string())
        );
    }

    #[test]
    fn parses_confirm() {
        assert_eq!(
            parse("cmd:?/stop s42"),
            CallbackAction::Confirm("/stop s42".to_string())
        );
    }

    #[test]
    fn parses_noop() {
        assert_eq!(parse("cmd:noop"), CallbackAction::Noop);
    }

    #[test]
    fn non_cmd_payload_is_choice_unchanged() {
        assert_eq!(
            parse("tok123:0"),
            CallbackAction::Choice {
                data: "tok123:0".to_string()
            }
        );
        assert_eq!(
            parse("nav:cd:beta"),
            CallbackAction::Choice {
                data: "nav:cd:beta".to_string()
            }
        );
    }

    #[test]
    fn empty_payload_is_choice() {
        assert_eq!(
            parse(""),
            CallbackAction::Choice {
                data: String::new()
            }
        );
    }
}
