//! Channel trait + per-platform providers.
//!
//! V0.6.0 Wave 2 Option-C implementation
//! (see `docs/versions/v0-6-0/wave-2-decisions.md` §3). The [`Channel`] trait
//! and the three providers (telegram / slack / discord) are vendored
//! from `references/openhuman/src/openhuman/channels/` with shell
//! reductions:
//!
//! - **No event_bus / security / config coupling.** ccteam-im has
//!   its own credentials + ACL + sanitize layers; providers stay
//!   plain reqwest clients.
//! - **No Socket Mode / gateway WebSockets.** Slack + Discord both
//!   use HTTP polling. Telegram uses `getUpdates` long-polling. None
//!   of the V0.6 scope needs a public HTTPS endpoint, which keeps
//!   ops surface to "edit credentials.json, run daemon".
//!
//! See `providers/mock.rs` for the in-memory test channel.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod providers;

/// Internal marker used to carry Telegram's draft-stop update through the
/// channel-neutral inbound message until the daemon resolves its sid.
pub const STOPPED_DRAFT_PREFIX: &str = "__ccteam_stopped_message_generation:";

/// Kind of an inbound [`ChannelAttachment`] (V0.8.4 P2a).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// A photo/image the agent should `Read` to see (e.g. an error
    /// screenshot).
    Image,
    /// A non-image document/file (pdf, log, …) the agent may `Read`.
    File,
}

/// An inbound file/image carried by a [`ChannelMessage`] (V0.8.4 P2a).
///
/// The channel listener downloads the bytes to a staging dir and records
/// the absolute `local_path` here; the gateway then names that path in
/// the turn text so the agent can `Read` it. (send-keys can only carry
/// text — there is no base64 content-block path — so attachments are
/// always "download → give path → `Read`".)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelAttachment {
    /// Image vs. generic file.
    pub kind: AttachmentKind,
    /// Sanitized original file name (no path separators / control chars).
    pub file_name: String,
    /// Absolute path on the daemon/agent **shared** filesystem.
    pub local_path: String,
    /// MIME type, when the platform reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Size in bytes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// `image_path` / `file_path` — the attribute key for an attachment kind.
/// Shared by the IM `<channel …>` first-attachment attribute, the IM
/// `[attachment …]` extra lines, and the web turn's attachment lines, so
/// the grammar the ccteam MCP instructions teach every vendor session
/// ("Read the file at image_path/file_path") never drifts between entry
/// surfaces.
pub fn attachment_path_key(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image_path",
        AttachmentKind::File => "file_path",
    }
}

/// The turn-text line naming one attachment for the agent to `Read` —
/// `[attachment image_path="…"]` / `[attachment file_path="…"]`. The ONE
/// line grammar both entry surfaces (IM extra attachments + web turn
/// attachments) emit; see [`attachment_path_key`].
pub fn attachment_line(att: &ChannelAttachment) -> String {
    format!(
        "[attachment {}=\"{}\"]",
        attachment_path_key(att.kind),
        att.local_path
    )
}

/// A message received from or sent to a channel. Trait surface lifted
/// from `references/openhuman/src/openhuman/channels/traits.rs` with
/// `serde` added so the daemon can persist inbound events for
/// debugging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelMessage {
    /// Platform-unique message id (e.g. Telegram `update_id`).
    pub id: String,
    /// Sender's platform user id.
    pub sender: String,
    /// Where to address replies (channel id / chat id / DM target).
    pub reply_target: String,
    /// Raw text payload.
    pub content: String,
    /// Platform name ("telegram" / "slack" / ...).
    pub channel: String,
    /// Unix-epoch seconds of receipt.
    pub timestamp: u64,
    /// Platform thread id (Slack `ts`, Discord thread, etc.).
    pub thread_ts: Option<String>,
    /// Inbound attachments (images / files) already downloaded to disk.
    /// Empty for text-only messages and non-Telegram channels. (P2a)
    #[serde(default)]
    pub attachments: Vec<ChannelAttachment>,
    /// Set when this inbound event is an option click (v0.8.5 D3) — e.g. a
    /// Telegram `callback_query` or a web chip click — instead of free
    /// text. `None` for ordinary messages.
    #[serde(default)]
    pub selection: Option<ChoiceReply>,
}

/// v0.8.20 F2 — a per-tenant IM bot's channel name is `"<platform>@<tenant_id>"`
/// (the `@` keeps the channel-map key unique per bot, so outbound replies route
/// to the RIGHT bot). The platform prefix (before `@`) is what platform-keyed
/// logic — the inbound ACL, `setMyCommands` — must use; the full name is the
/// routing key. The global/admin bot keeps a bare platform name (`"telegram"`).
pub fn platform_of(channel: &str) -> &str {
    channel.split('@').next().unwrap_or(channel)
}

/// Stable log correlation without exposing opaque gateway/event ids.
pub(crate) fn safe_correlation_id(id: &str) -> String {
    let telegram_suffix = id.strip_prefix("tg-cb-").or_else(|| id.strip_prefix("tg-"));
    if telegram_suffix.is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return id.to_string();
    }
    let hash = id.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("id-{:08x}", hash as u32)
}

/// Whether a channel name belongs to a per-tenant bot (`"<platform>@<tenant>"`)
/// rather than the global/admin bot (`"telegram"`/`"lark"`/…) or web (`"web"`).
pub fn is_tenant_bot_channel(channel: &str) -> bool {
    channel.contains('@')
}

/// v0.8.20 — the tenant id of a per-tenant bot channel (`"<platform>@<tenant>"`),
/// or `None` for the global/admin bot + web. Used to converge a tenant's IM bot
/// onto its web identity `user:<tenant>` (web↔IM convergence).
pub fn tenant_of_bot_channel(channel: &str) -> Option<&str> {
    channel.split_once('@').map(|(_platform, tid)| tid)
}

/// Setup-helper event: an inbound message whose sender was parsed successfully
/// but rejected by the provider-level allowlist gate.
///
/// The daemon records these to a small JSONL file so the web Settings flow can
/// show a user the id to allow — their own Lark `ou_...` open_id, or their
/// Telegram `chat_id` — without asking them to read server logs. The message is
/// still denied and never reaches the gateway.
///
/// One shape for every platform on purpose: the discovery flow is identical, so
/// a second struct would be the same fact living in two homes. [`Self::channel`]
/// says which bot saw it and therefore how to read [`Self::sender_id`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RejectedSenderProbe {
    /// Channel key that saw the event: `"lark"` / `"telegram"` for the
    /// global/admin bot, or `"<platform>@<tenant_id>"` for a per-user bot.
    pub channel: String,
    /// The id to place in the allowlist — a Lark sender `open_id` (`ou_...`),
    /// or a Telegram `chat_id`.
    pub sender_id: String,
    /// Conversation id (Lark `oc_...`; for Telegram the same value as
    /// [`Self::sender_id`]), kept for diagnostics and masked by the web API.
    pub chat_id: String,
    /// Raw provider message id (Lark `om_...`; Telegram the numeric id).
    pub message_id: String,
    /// Event time in Unix seconds, copied from the message when present.
    pub timestamp: u64,
}

impl RejectedSenderProbe {
    /// Append this probe to `path` as one JSONL line, creating the parent dir.
    ///
    /// Best effort by construction: a discovery aid must never interfere with
    /// message handling, so every failure logs a WARN and returns. Lives here
    /// rather than in a provider so both Lark and Telegram write the one
    /// format the web setup flow reads.
    pub async fn append_to(&self, path: &std::path::Path) {
        use tokio::io::AsyncWriteExt;
        let line = match serde_json::to_string(self) {
            Ok(line) => format!("{line}\n"),
            Err(err) => {
                tracing::warn!(error = %err, "rejected-sender probe encode failed");
                return;
            }
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::warn!(
                    path = %parent.display(), error = %err,
                    "rejected-sender probe dir create failed"
                );
                return;
            }
        }
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(line.as_bytes()).await {
                    tracing::warn!(
                        path = %path.display(), error = %err,
                        "rejected-sender probe append failed"
                    );
                } else if let Err(err) = file.flush().await {
                    tracing::warn!(
                        path = %path.display(), error = %err,
                        "rejected-sender probe flush failed"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(), error = %err,
                    "rejected-sender probe open failed"
                );
            }
        }
    }
}

/// Shared provider-edge handling for an inbound sender rejected by an IM
/// allowlist.
///
/// Rejection stays fail-closed: this helper has no inbound sender and cannot
/// forward the rejected payload to the gateway. It only preserves the setup
/// probe and sends one static, actionable notice per sender for this channel
/// listener's lifetime. Keeping both actions here prevents Telegram, Lark, and
/// future providers from drifting back to a silent message black hole.
#[cfg(any(feature = "telegram", feature = "lark"))]
#[derive(Debug, Default)]
pub(crate) struct RejectedSenderNotifier {
    probe_path: Option<std::path::PathBuf>,
    notice_state: tokio::sync::Mutex<RejectedSenderNoticeState>,
}

/// A rejected-sender flood must not grow daemon memory or bot replies without
/// bound. Normal personal bots have one or a handful of candidates; reaching
/// this ceiling means the listener is under abuse, so it stays fail-closed and
/// keeps writing setup probes but stops notifying new identities until reload.
#[cfg(any(feature = "telegram", feature = "lark"))]
const MAX_REJECTED_SENDERS_PER_LISTENER: usize = 1024;
#[cfg(any(feature = "telegram", feature = "lark"))]
const MAX_REJECTED_NOTICES_PER_MINUTE: usize = 20;
#[cfg(any(feature = "telegram", feature = "lark"))]
const REJECTED_NOTICE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(any(feature = "telegram", feature = "lark"))]
#[derive(Debug, Default)]
struct RejectedSenderNoticeState {
    notified_senders: std::collections::HashSet<String>,
    notice_times: std::collections::VecDeque<std::time::Instant>,
    capacity_warning_emitted: bool,
    rate_warning_emitted: bool,
}

#[cfg(any(feature = "telegram", feature = "lark"))]
#[derive(Debug, PartialEq, Eq)]
enum RejectedSenderNoticeDecision {
    Notify,
    AlreadyNotified,
    AtCapacity { emit_warning: bool },
    RateLimited { emit_warning: bool },
}

#[cfg(any(feature = "telegram", feature = "lark"))]
impl RejectedSenderNoticeState {
    fn admit(&mut self, sender_id: &str, now: std::time::Instant) -> RejectedSenderNoticeDecision {
        if self.notified_senders.contains(sender_id) {
            return RejectedSenderNoticeDecision::AlreadyNotified;
        }
        if self.notified_senders.len() >= MAX_REJECTED_SENDERS_PER_LISTENER {
            let emit_warning = !self.capacity_warning_emitted;
            self.capacity_warning_emitted = true;
            return RejectedSenderNoticeDecision::AtCapacity { emit_warning };
        }

        self.notice_times
            .retain(|sent_at| now.duration_since(*sent_at) < REJECTED_NOTICE_WINDOW);
        if self.notice_times.len() >= MAX_REJECTED_NOTICES_PER_MINUTE {
            let emit_warning = !self.rate_warning_emitted;
            self.rate_warning_emitted = true;
            return RejectedSenderNoticeDecision::RateLimited { emit_warning };
        }

        self.rate_warning_emitted = false;
        self.notified_senders.insert(sender_id.to_string());
        self.notice_times.push_back(now);
        RejectedSenderNoticeDecision::Notify
    }
}

#[cfg(any(feature = "telegram", feature = "lark"))]
impl RejectedSenderNotifier {
    pub(crate) fn with_probe_path(path: std::path::PathBuf) -> Self {
        Self {
            probe_path: Some(path),
            notice_state: tokio::sync::Mutex::new(RejectedSenderNoticeState::default()),
        }
    }

    /// Record every rejected event for the web binding flow, then notify the
    /// sender at most once. Both operations are best effort and never weaken
    /// the allowlist decision made by the caller.
    pub(crate) async fn record_and_notify<C>(&self, channel: &C, probe: RejectedSenderProbe)
    where
        C: Channel + ?Sized,
    {
        if let Some(path) = self.probe_path.as_ref() {
            probe.append_to(path).await;
        }

        let decision = {
            self.notice_state
                .lock()
                .await
                .admit(&probe.sender_id, std::time::Instant::now())
        };
        match decision {
            RejectedSenderNoticeDecision::Notify => {}
            RejectedSenderNoticeDecision::AlreadyNotified => {
                tracing::debug!(
                    channel = %probe.channel,
                    sender_id = %probe.sender_id,
                    "dropping another event from rejected IM sender"
                );
                return;
            }
            RejectedSenderNoticeDecision::AtCapacity { emit_warning } => {
                if emit_warning {
                    tracing::warn!(
                        channel = %probe.channel,
                        max_senders = MAX_REJECTED_SENDERS_PER_LISTENER,
                        "rejected-sender notice capacity reached; suppressing notices for new identities until listener reload"
                    );
                }
                tracing::debug!(
                    channel = %probe.channel,
                    sender_id = %probe.sender_id,
                    "rejected-sender binding notice suppressed at capacity"
                );
                return;
            }
            RejectedSenderNoticeDecision::RateLimited { emit_warning } => {
                if emit_warning {
                    tracing::warn!(
                        channel = %probe.channel,
                        max_notices = MAX_REJECTED_NOTICES_PER_MINUTE,
                        "rejected-sender notice rate limit reached; suppressing this burst"
                    );
                }
                tracing::debug!(
                    channel = %probe.channel,
                    sender_id = %probe.sender_id,
                    "rejected-sender binding notice suppressed by rate limit"
                );
                return;
            }
        }

        tracing::warn!(
            channel = %probe.channel,
            sender_id = %probe.sender_id,
            "dropping event from rejected IM sender; sending one binding notice"
        );
        let notice = rejected_sender_notice(&probe.sender_id);
        if let Err(err) = channel
            .send(&SendMessage::new(notice, probe.chat_id.clone()))
            .await
        {
            tracing::warn!(
                channel = %probe.channel,
                sender_id = %probe.sender_id,
                error = %err,
                "rejected-sender binding notice send failed"
            );
        }
    }
}

#[cfg(any(feature = "telegram", feature = "lark"))]
fn rejected_sender_notice(sender_id: &str) -> String {
    format!(
        "Этот IM-идентификатор ещё не привязан, сообщение не передано агенту.\nID для привязки: {sender_id}\nОткройте в аккаунте ccteam, которому принадлежит этот бот, «Настройки → Подключение», привяжите этот ID и повторите попытку."
    )
}

/// How an [`OutboundFile`] should be sent (V0.8.4 P2b).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutboundFileKind {
    /// Compressed image (Telegram `sendPhoto`).
    Photo,
    /// Generic file (Telegram `sendDocument`).
    Document,
}

/// A file to send back to a chat as an attachment on an outbound message
/// (V0.8.4 P2b). `path` is on the daemon/agent **shared** filesystem
/// (a remote `ProcessBackend` would need an "upload bytes" variant — a
/// recorded assumption, not designed here).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboundFile {
    /// Project-upload handle for browser delivery: the staged basename under
    /// `<project>/.ccteam/uploads/`. Empty on IM-only deliveries, whose
    /// providers continue reading `path` directly.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// Byte length of the project-owned staged copy. Captured once when web
    /// staging commits the file; zero for IM-only delivery (and valid for an
    /// empty staged file, distinguished by its non-empty `id`).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub size: u64,
    /// Absolute path to the file on disk.
    pub path: String,
    /// Optional caption (placed on the first attachment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Photo vs. document.
    pub kind: OutboundFileKind,
}

impl OutboundFile {
    /// Purely convert staged metadata to the reference-only browser/transcript
    /// shape. The returned type cannot carry source paths or bytes; callers
    /// may serialize it directly. A project upload handle remains mandatory.
    pub fn attachment_ref(
        &self,
    ) -> Result<ccteam_harness::execution::turns_mirror::AttachmentRef, &'static str> {
        use ccteam_harness::execution::turns_mirror::{AttachmentRef, AttachmentRefKind};
        if self.id.is_empty() {
            return Err("outbound attachment has no project upload id");
        }
        let source = std::path::Path::new(&self.path);
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .map(sanitize_attachment_name)
            .unwrap_or_else(|| "file".to_string());
        let kind = match self.kind {
            OutboundFileKind::Photo => AttachmentRefKind::Image,
            OutboundFileKind::Document => AttachmentRefKind::File,
        };
        Ok(AttachmentRef {
            id: self.id.clone(),
            name,
            kind,
            size: self.size,
        })
    }
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Visual style of a rendered button (TG-GATE-V2 W1). Maps to Telegram Rich
/// Message `RichMessageButton.style` (`"danger"` / `"success"` / `"primary"`)
/// when a message is sent via `sendRichMessage`; ignored by the classic
/// `InlineKeyboardButton` path (no per-button style there) and by every
/// non-Telegram channel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ButtonStyle {
    /// Primary/emphasized action.
    Primary,
    /// Positive/confirming action.
    Success,
    /// Destructive/dangerous action.
    Danger,
}

/// A single selectable option rendered on an outbound [`SendMessage`]
/// (v0.8.5 D3). Channel-local + deliberately opaque: `data` is whatever
/// the gateway minted (always `"{token}:{idx}"`) and rides the platform's
/// callback channel verbatim (Telegram `callback_data`, web chip id);
/// `label` is the button text. The channel never interprets either — it
/// renders `label`, returns `data` on click. Kept distinct from the
/// harness-layer `ChoiceOption` so the channel axis never imports a
/// harness type (two-axis decoupling discipline; not a compile barrier —
/// `ccteam-im` already depends on `ccteam-harness`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageOption {
    /// Opaque callback payload, always `"{token}:{idx}"`. MUST stay short
    /// (≤ ~20 bytes) — Telegram caps `callback_data` at 64 bytes.
    pub data: String,
    /// Human-readable button label.
    pub label: String,
    /// v0.8.7 review-fix (R-H1) — the stable option `id` from the source
    /// [`ChoiceOption`] (e.g. `"allow"` / `"deny"`). IM channels ignore it
    /// (they render `label`, echo `data` on click); it exists so a tokenless
    /// web client can carry the option's real id through its own
    /// `POST /sessions/{sid}/resolve {token, selection=id}` path — resolving
    /// the SAME token-keyed pending the IM callback does, never a turn.
    #[serde(default)]
    pub id: String,
    /// TG-GATE-V2 W1 — optional visual style (danger/success/primary).
    /// `None` = default styling. Only Telegram's Rich Message buttons block
    /// honors it; the classic inline-keyboard fallback and other channels
    /// ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ButtonStyle>,
}

/// A Telegram reply keyboard request. Other channels ignore it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplyKeyboard {
    /// Persistent rows of plain-text buttons.
    Buttons(Vec<Vec<String>>),
    /// Remove the current reply keyboard.
    Remove,
}

/// An inbound option click carried on a [`ChannelMessage`] (v0.8.5 D3).
/// `data` echoes the [`MessageOption::data`] the user clicked; the gateway
/// splits it on the first `:` into `(token, idx)` and resolves `idx` back
/// to the real option id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChoiceReply {
    /// The clicked option's opaque callback payload (`"{token}:{idx}"`).
    pub data: String,
    /// Telegram callback context for a one-shot ephemeral reply. `None` for
    /// ordinary button systems and Telegram private chats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_ephemeral: Option<CallbackEphemeral>,
}

/// Callback facts required by Telegram's one-shot ephemeral reply API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallbackEphemeral {
    /// Telegram callback query that triggered the response.
    pub callback_query_id: String,
    /// Telegram user who alone receives the response.
    pub receiver_user_id: i64,
    /// Whether Telegram may replace the callback's source message.
    pub replace_callback_query_message: bool,
    /// Source ephemeral message, when this callback came from one.
    pub ephemeral_message_id: Option<i64>,
}

/// Outcome of a Telegram callback-bound ephemeral operation.
///
/// `Unknown` means the request may have reached Telegram despite the missing
/// response, so retrying would duplicate user-visible output.
#[derive(Debug)]
pub enum EphemeralDelivery {
    /// Telegram accepted the operation.
    Delivered(Option<String>),
    /// Telegram explicitly rejected the operation; a fallback is safe.
    Rejected(anyhow::Error),
    /// The request may have applied, so no follow-up send is safe.
    Unknown(anyhow::Error),
}

/// Message to send through a channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SendMessage {
    /// Payload (post-sanitize).
    pub content: String,
    /// Platform-specific recipient.
    pub recipient: String,
    /// Optional subject (email / push variants); ignored on TG/Slack/Discord.
    pub subject: Option<String>,
    /// Threading id (Slack `thread_ts`); when set the platform should
    /// post as a reply.
    pub thread_ts: Option<String>,
    /// Outbound file attachments (V0.8.4 P2b). Empty ⇒ a plain text send
    /// (`sendMessage`); non-empty ⇒ `sendPhoto`/`sendDocument`.
    #[serde(default)]
    pub attachments: Vec<OutboundFile>,
    /// Selectable options rendered as buttons / chips (v0.8.5 D3). Empty ⇒
    /// an ordinary message (zero behavior change). Channels without native
    /// buttons fall back to a numbered text list.
    #[serde(default)]
    pub options: Vec<MessageOption>,
    /// TG-GATE-V2 W1 — Rich Messages markdown (Bot API 10.3
    /// `InputRichMessage.markdown`). When `Some`, Telegram tries
    /// `sendRichMessage` first; on rejection it renders this field for a
    /// fitting classic message, otherwise sends the row's plain `content`
    /// (splitting it inside the transport when needed). `None` ⇒ zero
    /// behavior change (classic `sendMessage`/HTML path on `content` only).
    /// Ignored by every other channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich_markdown: Option<String>,
    /// TG-GATE-V2 W1 — button rows (1..=8 buttons per row), rendered ABOVE
    /// [`Self::options`] on both the Rich Message buttons block and the
    /// classic `inline_keyboard` fallback. `options` (one-per-row, choice
    /// replies) is untouched and stays a separate concept — `button_rows`
    /// is for multi-per-row command/navigation buttons. Empty ⇒ no extra
    /// rows (zero behavior change). Non-Telegram channels fold rows into a
    /// numbered text list the same way `options` already does today.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub button_rows: Vec<Vec<MessageOption>>,
    /// Rich markdown already contains the button rows; the classic fallback
    /// still uses `button_rows` as its inline keyboard.
    #[serde(default)]
    pub inline_buttons: bool,
    /// Telegram reply keyboard request; unsupported channels ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_keyboard: Option<ReplyKeyboard>,
    /// Telegram-only response to a group callback. Other channels ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_ephemeral: Option<CallbackEphemeral>,
}

impl SendMessage {
    /// Helper: build a plain message with no subject / thread / files.
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
            attachments: Vec::new(),
            options: Vec::new(),
            rich_markdown: None,
            button_rows: Vec::new(),
            inline_buttons: false,
            reply_keyboard: None,
            callback_ephemeral: None,
        }
    }

    /// Builder-style: attach a thread id.
    pub fn in_thread(mut self, thread_ts: Option<String>) -> Self {
        self.thread_ts = thread_ts;
        self
    }

    /// Builder-style: attach outbound files.
    pub fn with_attachments(mut self, attachments: Vec<OutboundFile>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Builder-style: attach selectable options (v0.8.5 D3).
    pub fn with_options(mut self, options: Vec<MessageOption>) -> Self {
        self.options = options;
        self
    }

    /// Builder-style: attach Rich Messages markdown (TG-GATE-V2 W1).
    pub fn with_rich_markdown(mut self, rich_markdown: impl Into<String>) -> Self {
        self.rich_markdown = Some(rich_markdown.into());
        self
    }

    /// Builder-style: attach button rows (TG-GATE-V2 W1).
    pub fn with_button_rows(mut self, button_rows: Vec<Vec<MessageOption>>) -> Self {
        self.button_rows = button_rows;
        self
    }

    /// Builder-style: mark button rows already embedded in Rich markdown.
    pub fn with_inline_buttons(mut self, inline_buttons: bool) -> Self {
        self.inline_buttons = inline_buttons;
        self
    }

    /// Builder-style: attach a Telegram reply keyboard request.
    pub fn with_reply_keyboard(mut self, reply_keyboard: ReplyKeyboard) -> Self {
        self.reply_keyboard = Some(reply_keyboard);
        self
    }

    /// Render this callback response only to the user who tapped the button.
    pub fn with_callback_ephemeral(
        mut self,
        callback_query_id: impl Into<String>,
        receiver_user_id: i64,
    ) -> Self {
        self.callback_ephemeral = Some(CallbackEphemeral {
            callback_query_id: callback_query_id.into(),
            receiver_user_id,
            replace_callback_query_message: true,
            ephemeral_message_id: None,
        });
        self
    }
}

/// A gateway-owned command advertised in a channel's command menu
/// (v0.8.5 P1). Registered once at daemon startup via
/// [`Channel::register_commands`]; only channels with a native menu
/// (Telegram `setMyCommands`) act on it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandSpec {
    /// Command name including the leading `/`.
    pub name: String,
    /// One-line description shown in the channel's command menu.
    pub description: String,
}

/// Core channel trait — implement for any IM platform.
///
/// **Listening contract**: implementations of [`Channel::listen`] run
/// for the daemon's lifetime, pushing one [`ChannelMessage`] per
/// inbound event to the supplied tokio mpsc sender. The future
/// returns `Ok(())` only on graceful shutdown (the sender was
/// dropped) and any other return value is treated as a fatal channel
/// error by the supervisor.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable platform name (matches credentials.json key).
    fn name(&self) -> &str;

    /// Whether this channel can render `SendMessage::rich_markdown` (TG-GATE-V2
    /// W7a). **Default `false`** — the daemon only sets `rich_markdown` (and
    /// skips its own `split_for_channel` pre-splitting) for a channel that
    /// answers `true` here; every other channel keeps today's plain-`content`
    /// split + durable per-part ledger behavior byte-for-byte. Telegram is the
    /// only channel that overrides this (its Bot API 10.3 Rich Messages).
    fn supports_rich_messages(&self) -> bool {
        false
    }

    /// Send or refresh a transient rich-message draft. Channels without a
    /// native draft API keep the ordinary progress send/edit path.
    async fn send_draft(
        &self,
        _recipient: &str,
        _draft_id: i64,
        _markdown: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("draft messages unsupported")
    }

    /// Send a single message. Returns the platform-side message id
    /// when available (Slack `ts`, Discord message id, …) for echo
    /// suppression in the outbound tailer.
    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>>;

    /// Send a callback-bound ephemeral message. Providers without Telegram's
    /// acknowledgement ambiguity treat a send error as an explicit rejection.
    async fn send_ephemeral(&self, message: &SendMessage) -> EphemeralDelivery {
        match self.send(message).await {
            Ok(id) => EphemeralDelivery::Delivered(id),
            Err(err) => EphemeralDelivery::Rejected(err),
        }
    }

    /// Complete the client-side callback spinner. Telegram overrides this;
    /// other channels have no equivalent operation.
    async fn answer_callback(&self, _callback_query_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Edit a Telegram ephemeral message. Other transports deliberately do
    /// not implement this separate identifier space.
    async fn edit_ephemeral_message(
        &self,
        _chat_id: &str,
        _callback: &CallbackEphemeral,
        _content: &str,
        _button_rows: &[Vec<MessageOption>],
    ) -> EphemeralDelivery {
        EphemeralDelivery::Rejected(anyhow::anyhow!("ephemeral message edits are unsupported"))
    }

    /// Long-running inbound listener (see trait-level docs).
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()>;

    /// Quick liveness probe (default: assume healthy). Supervisor uses
    /// this for periodic re-checks; failure logs a warn but doesn't
    /// tear the channel down (transient API outages are normal).
    async fn health_check(&self) -> bool {
        true
    }

    /// Per-message length ceiling, in **UTF-16 code units**, or `None`
    /// for no limit. When `Some(limit)`, the daemon splits an overflowing
    /// outbound reply into ordered sub-messages via
    /// [`crate::sanitize::split_for_channel`] before sending. The actual
    /// constant lives in the provider (e.g. Telegram's 4096) so the
    /// gateway/daemon stay channel-neutral — no `4096` or `"telegram"`
    /// branch leaks up. Default `None` = single send, today's behavior.
    fn max_message_len(&self) -> Option<usize> {
        None
    }

    /// Edit a previously-sent message in place (V0.8.4 P1 — live progress
    /// status). Returns the platform message id (usually `message_id`
    /// unchanged). The **default degrades gracefully** to appending a new
    /// message via [`Channel::send`], so a channel without edit support
    /// still shows progress (just as extra messages rather than one live
    /// status). Telegram overrides with `editMessageText`.
    ///
    /// `button_rows` (TG-GATE-V2 W5) carries the same shape as
    /// [`SendMessage::button_rows`] — e.g. the live progress edit's
    /// `[⛔ Прервать]` button. The default degrades by folding it into
    /// [`SendMessage::button_rows`] on the fallback `send`; a channel with
    /// no native buttons ignores it or folds it into a numbered text list
    /// (mirrors `options`' existing fallback).
    ///
    /// `rich_markdown`/`inline_buttons` (F5/F2) mirror
    /// [`SendMessage::rich_markdown`]/[`SendMessage::inline_buttons`] —
    /// `content` stays the plain/classic text, `rich_markdown` is the
    /// separate Rich Messages representation (which may embed `<tg-button>`
    /// tags) a rich-capable channel alone should read; `inline_buttons`
    /// tells it those tags already cover every button, so it must not also
    /// append `button_rows` as a trailing block. The default degrades by
    /// folding `rich_markdown` into `SendMessage` the same way `send`'s
    /// `Answer` path already does.
    async fn edit_message(
        &self,
        recipient: &str,
        _message_id: &str,
        content: &str,
        rich_markdown: Option<&str>,
        inline_buttons: bool,
        button_rows: &[Vec<MessageOption>],
    ) -> anyhow::Result<Option<String>> {
        let mut out = SendMessage::new(content, recipient).with_button_rows(button_rows.to_vec());
        if let Some(markdown) = rich_markdown {
            out = out
                .with_rich_markdown(markdown)
                .with_inline_buttons(inline_buttons);
        }
        self.send(&out).await
    }

    /// Register the gateway's own commands in the channel's command menu
    /// (v0.8.5 P1). **Default no-op** (same pattern as
    /// [`Channel::max_message_len`]): only a channel with a native menu
    /// overrides it (Telegram → `setMyCommands`). Keeps the daemon
    /// channel-neutral — no `"telegram"` branch leaks up.
    async fn register_commands(&self, _cmds: &[CommandSpec]) -> anyhow::Result<()> {
        Ok(())
    }

    /// Add the transient "seen/ack" reaction (👀-equivalent) to a message —
    /// the instant "received, processing" acknowledgement the gateway adds the
    /// moment an inbound message is dispatched to a session as a turn, filling
    /// the silent time-to-first-token gap. Removed by [`remove_reaction`] when
    /// the session's first turn event appears (the "💭 thinking…" sign).
    ///
    /// The 👀-equivalent is hardcoded per provider (the only use is this ack,
    /// so there is no generic emoji argument). Returns an opaque `handle` the
    /// provider needs to remove it later (e.g. Feishu's `reaction_id`), or
    /// `None` when the provider clears a reaction by `(chat, message)` alone
    /// (Telegram). **Default no-op `Ok(None)`** — web/discord/slack/mock/ws
    /// keep it (reactions are an IM-only affordance), so they need no change.
    /// Fire-and-forget at the call site: a reaction failure must NEVER break or
    /// delay turn/answer delivery.
    async fn add_reaction(
        &self,
        _chat_id: &str,
        _message_id: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Remove the reaction [`add_reaction`] added. `handle` is whatever that
    /// returned (the provider's reaction handle, e.g. Feishu's `reaction_id`),
    /// or `None` for providers that clear by `(chat, message)` alone.
    /// **Default no-op** — same providers as [`add_reaction`]. Fire-and-forget
    /// at the call site.
    async fn remove_reaction(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _handle: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared inbound-attachment staging helpers (V0.8.4 P2a). Every provider
// that downloads inbound files (telegram, lark, …) routes through these so
// the staging layout and the name-sanitization rule stay identical across
// channels — a fix in one place protects every channel's `image_path="…"`
// turn-text attribute.
// ─────────────────────────────────────────────────────────────────────────────

/// Staging dir for downloaded inbound attachments (channel-scoped — the
/// routing to a project/role happens later in the gateway).
#[cfg(any(feature = "telegram", feature = "lark"))]
pub(crate) fn inbound_staging_dir() -> std::path::PathBuf {
    crate::default_ccteam_root_public()
        .join("state")
        .join("im")
        .join("attachments")
        .join("inbound")
}

/// Pure: strip path separators / control chars and cap length so a
/// platform-supplied name can't traverse out of the staging dir. Also
/// drops `" < >` so the name can't break (or inject into) the
/// `<channel image_path="…">` turn-text attribute the gateway builds.
pub fn sanitize_attachment_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | '"' | '<' | '>'))
        .take(128)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The one project-scoped upload directory used by both inbound web-composer
/// uploads and outbound browser attachments.
pub fn project_uploads_dir(project_dir: &std::path::Path) -> std::path::PathBuf {
    project_dir.join(".ccteam").join("uploads")
}

/// Allocate the existing `{millis}-{sanitized-name}` project-upload shape.
/// A same-millisecond collision gains the established numeric bump rather
/// than clobbering an earlier asset. The caller owns directory creation and
/// the actual atomic write/copy.
pub fn next_project_upload_path(
    project_dir: &std::path::Path,
    name: &str,
    millis: i64,
) -> (std::path::PathBuf, String) {
    let name = sanitize_attachment_name(name);
    let dir = project_uploads_dir(project_dir);
    let mut path = dir.join(format!("{millis}-{name}"));
    let mut bump = 0u32;
    while path.exists() {
        bump += 1;
        path = dir.join(format!("{millis}-{bump}-{name}"));
    }
    (path, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "telegram", feature = "lark"))]
    use crate::transport::providers::mock::MockChannel;

    /// v0.8.20 F2 — the platform prefix is what platform-keyed logic (ACL,
    /// menus) uses; the `@`-suffixed full name is the per-bot routing key.
    #[test]
    fn platform_of_and_tenant_bot_channel() {
        assert_eq!(platform_of("telegram"), "telegram");
        assert_eq!(platform_of("telegram@uabc"), "telegram");
        assert_eq!(platform_of("lark@u123"), "lark");
        assert_eq!(platform_of("web"), "web");
        assert!(!is_tenant_bot_channel("telegram"));
        assert!(!is_tenant_bot_channel("web"));
        assert!(is_tenant_bot_channel("telegram@uabc"));
        assert!(is_tenant_bot_channel("lark@u123"));
    }

    #[test]
    fn safe_correlation_id_keeps_telegram_cids_and_hashes_opaque_ids() {
        assert_eq!(safe_correlation_id("tg-42"), "tg-42");
        assert_eq!(safe_correlation_id("tg-cb-99"), "tg-cb-99");
        assert_eq!(
            safe_correlation_id("gateway-ephemeral-confirm-1-secret"),
            "id-cc2d0f13"
        );
    }

    #[test]
    fn sanitize_attachment_name_blocks_traversal_and_control() {
        assert_eq!(sanitize_attachment_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_attachment_name("a/b/c.png"), "c.png");
        assert_eq!(sanitize_attachment_name("ok\u{0000}name.txt"), "okname.txt");
        assert_eq!(sanitize_attachment_name(""), "file");
        assert_eq!(sanitize_attachment_name("   "), "file");
        // Quotes/angle brackets would break the `image_path="…"` attr.
        assert_eq!(sanitize_attachment_name("foo\"bar.pdf"), "foobar.pdf");
        assert_eq!(sanitize_attachment_name("a<b>c.png"), "abc.png");
    }

    #[test]
    fn project_upload_path_reuses_millis_name_and_bumps_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project_uploads_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        let (first, name) = next_project_upload_path(tmp.path(), "../chart.png", 42);
        assert_eq!(name, "chart.png");
        assert_eq!(first.file_name().unwrap(), "42-chart.png");
        std::fs::write(&first, b"one").unwrap();
        let (second, _) = next_project_upload_path(tmp.path(), "chart.png", 42);
        assert_eq!(second.file_name().unwrap(), "42-1-chart.png");
    }

    #[test]
    fn attachment_ref_uses_staged_size_without_reading_source_and_requires_id() {
        let mut file = OutboundFile {
            id: "42-chart.png".into(),
            size: 17,
            path: "/definitely/missing/chart.png".into(),
            caption: None,
            kind: OutboundFileKind::Photo,
        };

        let reference = file.attachment_ref().unwrap();
        assert_eq!(reference.id, "42-chart.png");
        assert_eq!(reference.name, "chart.png");
        assert_eq!(reference.size, 17);
        assert_eq!(
            reference.kind,
            ccteam_harness::execution::turns_mirror::AttachmentRefKind::Image
        );

        file.id.clear();
        assert_eq!(
            file.attachment_ref().unwrap_err(),
            "outbound attachment has no project upload id"
        );
    }

    #[tokio::test]
    #[cfg(any(feature = "telegram", feature = "lark"))]
    async fn rejected_sender_is_probed_each_time_but_notified_once() {
        let tmp = tempfile::tempdir().unwrap();
        let probe_path = tmp.path().join("rejected-senders.jsonl");
        let notifier = RejectedSenderNotifier::with_probe_path(probe_path.clone());
        let channel = MockChannel::new().with_name("telegram@u123");
        let first = RejectedSenderProbe {
            channel: "telegram@u123".into(),
            sender_id: "339498819".into(),
            chat_id: "339498819".into(),
            message_id: "1".into(),
            timestamp: 10,
        };
        let mut second = first.clone();
        second.message_id = "2".into();
        second.timestamp = 11;

        notifier.record_and_notify(&channel, first.clone()).await;
        notifier.record_and_notify(&channel, second.clone()).await;

        let outbox = channel.outbox().await;
        assert_eq!(outbox.len(), 1, "repeat rejects must not spam the sender");
        assert_eq!(outbox[0].recipient, "339498819");
        assert_eq!(
            outbox[0].content,
            "Этот IM-идентификатор ещё не привязан, сообщение не передано агенту.\nID для привязки: 339498819\nОткройте в аккаунте ccteam, которому принадлежит этот бот, «Настройки → Подключение», привяжите этот ID и повторите попытку."
        );

        let lines = tokio::fs::read_to_string(&probe_path).await.unwrap();
        let probes = lines
            .lines()
            .map(|line| serde_json::from_str::<RejectedSenderProbe>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            probes,
            vec![first, second],
            "every reject remains discoverable"
        );

        // The helper can only send outbound. It cannot inject the rejected
        // payload (or its notice) into the gateway-facing inbound stream.
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        channel.listen(tx).await.unwrap();
        assert!(rx.recv().await.is_none());
    }

    #[test]
    #[cfg(any(feature = "telegram", feature = "lark"))]
    fn rejected_sender_notice_state_is_rate_and_memory_bounded() {
        let mut state = RejectedSenderNoticeState::default();
        let now = std::time::Instant::now();

        for index in 0..MAX_REJECTED_NOTICES_PER_MINUTE {
            assert_eq!(
                state.admit(&format!("ou_{index}"), now),
                RejectedSenderNoticeDecision::Notify
            );
        }
        assert_eq!(
            state.admit("ou_burst", now),
            RejectedSenderNoticeDecision::RateLimited { emit_warning: true }
        );
        assert_eq!(
            state.admit("ou_burst", now),
            RejectedSenderNoticeDecision::RateLimited {
                emit_warning: false
            },
            "one burst must only emit one warning"
        );

        let after_window = now + REJECTED_NOTICE_WINDOW + std::time::Duration::from_secs(1);
        assert_eq!(
            state.admit("ou_burst", after_window),
            RejectedSenderNoticeDecision::Notify,
            "a legitimate sender may retry after the burst window"
        );

        for index in state.notified_senders.len()..MAX_REJECTED_SENDERS_PER_LISTENER {
            state.notified_senders.insert(format!("fill_{index}"));
        }
        assert_eq!(
            state.notified_senders.len(),
            MAX_REJECTED_SENDERS_PER_LISTENER
        );
        assert_eq!(
            state.admit("ou_over_capacity", after_window),
            RejectedSenderNoticeDecision::AtCapacity { emit_warning: true }
        );
        assert_eq!(
            state.admit("ou_over_capacity", after_window),
            RejectedSenderNoticeDecision::AtCapacity {
                emit_warning: false
            }
        );
        assert_eq!(
            state.notified_senders.len(),
            MAX_REJECTED_SENDERS_PER_LISTENER
        );
    }
}
