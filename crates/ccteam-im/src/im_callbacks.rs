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
//! - `cmd:!<token>` / `cmd:x<token>` — approve / cancel a gateway-held
//!   confirmation. The token is opaque and never contains the command.

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
    /// Approve a pending confirmation by its opaque token.
    Confirmed(String),
    /// Cancel a pending confirmation by its opaque token.
    Cancelled(String),
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
    match rest.chars().next() {
        Some('?') => CallbackAction::Confirm(rest[1..].to_string()),
        Some('!') => CallbackAction::Confirmed(rest[1..].to_string()),
        Some('x') => CallbackAction::Cancelled(rest[1..].to_string()),
        _ => CallbackAction::Command(rest.to_string()),
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
    fn confirmation_tokens_are_not_direct_commands() {
        assert_eq!(
            parse("cmd:!deadbeef"),
            CallbackAction::Confirmed("deadbeef".to_string()),
        );
        assert_eq!(
            parse("cmd:xdeadbeef"),
            CallbackAction::Cancelled("deadbeef".to_string()),
        );
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
