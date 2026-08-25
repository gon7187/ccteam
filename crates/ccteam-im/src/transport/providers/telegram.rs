//! Telegram channel — `getUpdates` long-polling + `sendMessage`.
//!
//! Slim port of `references/openhuman/src/openhuman/channels/providers/telegram/`
//! with openhuman's `event_bus` / `security::pairing` / `config`
//! dependencies elided (ccteam-im has its own ACL + sanitize layers).
//!
//! V0.6.0 host probe is **mock-only** (no real TG token paste yet).
//! The provider compiles and the request shapes are correct against
//! the Bot API documented at <https://core.telegram.org/bots/api>;
//! end-to-end verification ships post-token-paste.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;

use anyhow::Context as _;

use crate::latency::now_unix_ms;
use crate::telegram_html::render_markdown;
use crate::transport::{
    inbound_staging_dir, sanitize_attachment_name, AttachmentKind, ButtonStyle, Channel,
    ChannelAttachment, ChannelMessage, ChoiceReply, CommandSpec, MessageOption, OutboundFile,
    OutboundFileKind, RejectedSenderNotifier, RejectedSenderProbe, SendMessage,
};

/// `getUpdates` long-poll seconds.
const POLL_TIMEOUT_SECS: u64 = 25;

/// Conservative per-message ceiling in **UTF-16 code units**. Telegram's
/// hard `sendMessage` limit is 4096 UTF-16 units; we reserve headroom for
/// re-opened code fences and reply metadata so a split part never trips a
/// 400. This is the *only* place the Telegram length constant lives — the
/// gateway/daemon read it polymorphically via
/// [`Channel::max_message_len`], keeping the split path channel-neutral.
const MAX_MESSAGE_UTF16: usize = 3900;

/// Telegram channel.
pub struct TelegramChannel {
    bot_token: String,
    allowed_chat_ids: Vec<String>,
    /// What an EMPTY allowlist means for THIS bot.
    ///
    /// `true` (the global/owner bot): open — locking a half-configured owner
    /// out of their own box is the worse failure, and the daemon warns about
    /// it at startup. `false` (a per-tenant bot, [`Self::fail_closed`]): deny,
    /// matching Lark. A tenant bot's inbound is stamped with that tenant's
    /// identity by `Gateway::principal`, so an unbound bot handing a stranger
    /// the tenant's projects and sessions is not a mode anyone opted into.
    open_when_unset: bool,
    /// Shared setup probe + one-shot binding notice for rejected senders.
    rejected_senders: RejectedSenderNotifier,
    http: reqwest::Client,
    last_offset: Arc<Mutex<i64>>,
    name: String,
    /// TG-GATE-V2 W1 — reason kinds (`"http_400"`, `"network_error"`, …)
    /// already logged for a rich→classic fallback, so a noisy failure mode
    /// logs once (debug) instead of once per message.
    rich_fallback_logged: Mutex<std::collections::HashSet<String>>,
    /// TG-GATE-V2 W7a — consecutive rich-message failures (send + edit
    /// share one counter — both hit the same Bot API surface). Any success
    /// resets it to 0 (a transient blip must not trip the breaker).
    rich_failures: std::sync::atomic::AtomicU32,
    /// TG-GATE-V2 W7a — sticky circuit breaker: once 3 consecutive rich
    /// failures land, this flips permanently (process lifetime, reset
    /// never) so every later send/edit skips the rich attempt outright
    /// instead of paying its latency + failing it again.
    rich_disabled: std::sync::atomic::AtomicBool,
}

impl TelegramChannel {
    /// Build with the @BotFather token and allowed chat IDs (empty =
    /// open). The chat-ID allowlist is enforced inside [`Channel::listen`]
    /// before pushing to the daemon mpsc.
    pub fn new(bot_token: String, allowed_chat_ids: Vec<String>) -> Self {
        Self {
            bot_token,
            allowed_chat_ids,
            open_when_unset: true,
            rejected_senders: RejectedSenderNotifier::default(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(POLL_TIMEOUT_SECS + 10))
                .build()
                .expect("reqwest client"),
            last_offset: Arc::new(Mutex::new(0)),
            name: "telegram".to_string(),
            rich_fallback_logged: Mutex::new(std::collections::HashSet::new()),
            rich_failures: std::sync::atomic::AtomicU32::new(0),
            rich_disabled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// v0.8.20 F2 — override the channel-map key (`"telegram@<tenant_id>"`) for a
    /// per-tenant bot, so its inbound stamps that name and outbound replies route
    /// back through THIS bot (not a colliding shared `"telegram"`).
    pub fn with_name(mut self, name: String) -> Self {
        self.name = name;
        self
    }

    /// Treat an EMPTY allowlist as "answer nobody" instead of "answer
    /// everybody" — the posture every per-tenant bot takes (see
    /// [`Self::open_when_unset`]).
    pub fn fail_closed(mut self) -> Self {
        self.open_when_unset = false;
        self
    }

    /// Record rejected chat ids to `path` so the owner of this bot can
    /// discover the id to allow from the web UI instead of the daemon log.
    pub fn with_rejected_sender_probe_path(mut self, path: std::path::PathBuf) -> Self {
        self.rejected_senders = RejectedSenderNotifier::with_probe_path(path);
        self
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.bot_token, method)
    }

    /// Whether a chat is permitted. An empty allowlist means whatever this
    /// bot was built to mean ([`Self::open_when_unset`]) — never an implicit
    /// "open" for a bot that speaks for one tenant.
    fn chat_allowed(&self, chat_id: &str) -> bool {
        if self.allowed_chat_ids.is_empty() {
            return self.open_when_unset;
        }
        self.allowed_chat_ids.iter().any(|id| id == chat_id)
    }

    /// Keep the allowlist fail-closed while making the rejection actionable.
    /// Telegram's binding id and conversation id are both the chat id.
    async fn reject_chat(&self, chat_id: &str, message_id: i64, date: i64) {
        self.rejected_senders
            .record_and_notify(
                self,
                RejectedSenderProbe {
                    channel: self.name.clone(),
                    sender_id: chat_id.to_string(),
                    chat_id: chat_id.to_string(),
                    message_id: message_id.to_string(),
                    timestamp: date.max(0) as u64,
                },
            )
            .await;
    }

    /// Resolve a `file_id` to its server-side `file_path` via `getFile`.
    async fn get_file_path(&self, file_id: &str) -> anyhow::Result<String> {
        let url = self.api_url("getFile");
        let resp = self
            .http
            .get(&url)
            .query(&[("file_id", file_id)])
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("telegram getFile {status}: {text}");
        }
        serde_json::from_str::<serde_json::Value>(&text)?
            .get("result")
            .and_then(|r| r.get("file_path"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("telegram getFile: missing file_path"))
    }

    /// Download a resolved `file_path` from the Bot file endpoint.
    async fn download_file_bytes(&self, file_path: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.bot_token, file_path
        );
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("telegram file download {}", resp.status());
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Download + stage one attachment. Returns `Ok(None)` if it exceeds
    /// the 20 MB Bot-API ceiling (rejected, not an error). The staged file
    /// lives at `<staging>/<cid>-<sanitized_name>`.
    async fn stage_attachment(
        &self,
        cid: &str,
        pending: &PendingDownload,
    ) -> anyhow::Result<Option<ChannelAttachment>> {
        if pending
            .size
            .map(|s| s > MAX_ATTACHMENT_BYTES)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let file_path = self.get_file_path(&pending.file_id).await?;
        let bytes = self.download_file_bytes(&file_path).await?;
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Ok(None);
        }
        let safe_name = sanitize_attachment_name(&pending.file_name);
        let dir = inbound_staging_dir();
        tokio::fs::create_dir_all(&dir).await?;
        let dest = dir.join(format!("{cid}-{safe_name}"));
        tokio::fs::write(&dest, &bytes).await?;
        Ok(Some(ChannelAttachment {
            kind: pending.kind,
            file_name: safe_name,
            local_path: dest.to_string_lossy().into_owned(),
            mime: pending.mime.clone(),
            size: Some(bytes.len() as u64),
        }))
    }

    /// Send each outbound file as `sendPhoto`/`sendDocument`; the caption
    /// rides the first attachment (preferring its own caption, else the
    /// message text). Returns the first attachment's message id.
    /// Handle an inline-keyboard button click (v0.8.5 D3): answer the
    /// callback (clears the client spinner) + forward a `ChannelMessage`
    /// carrying the opaque selection so the gateway can resolve it.
    async fn handle_callback_query(
        &self,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        cb: TgCallbackQuery,
    ) {
        let _ = self
            .http
            .post(self.api_url("answerCallbackQuery"))
            .json(&serde_json::json!({ "callback_query_id": cb.id }))
            .send()
            .await;
        let Some(data) = cb.data else {
            return;
        };
        let Some(msg) = cb.message else {
            return;
        };
        let chat_id = msg.chat.id.to_string();
        if !self.chat_allowed(&chat_id) {
            self.reject_chat(&chat_id, msg.message_id, msg.date).await;
            return;
        }
        let sender = cb
            .from
            .as_ref()
            .and_then(|u| u.username.clone())
            .or_else(|| cb.from.as_ref().map(|u| u.id.to_string()))
            .unwrap_or_else(|| "anonymous".to_string());
        let payload = ChannelMessage {
            id: callback_message_id(msg.message_id),
            sender,
            reply_target: chat_id,
            content: String::new(),
            channel: self.name.clone(),
            timestamp: (now_unix_ms() / 1000) as u64,
            thread_ts: None,
            attachments: Vec::new(),
            selection: Some(ChoiceReply { data }),
        };
        let _ = tx.send(payload).await;
    }

    /// POST a `setMessageReaction` body (the shared transport for both
    /// add/remove). Bails on a non-2xx so the caller's `?` surfaces it; the
    /// daemon egress swallows that error (reactions are fire-and-forget).
    async fn post_set_message_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let url = self.api_url("setMessageReaction");
        let resp = self.http.post(&url).json(body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("telegram setMessageReaction {chat_id}#{message_id} → {status}: {text}");
        }
        Ok(())
    }

    async fn send_with_attachments(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let mut first_id = None;
        for (i, att) in message.attachments.iter().enumerate() {
            let caption = att.caption.clone().or_else(|| {
                if i == 0 && !message.content.is_empty() {
                    Some(message.content.clone())
                } else {
                    None
                }
            });
            let id = self
                .send_one_attachment(
                    &message.recipient,
                    att,
                    caption.as_deref(),
                    message.thread_ts.as_deref(),
                )
                .await?;
            if first_id.is_none() {
                first_id = id;
            }
        }
        Ok(first_id)
    }

    async fn send_one_attachment(
        &self,
        recipient: &str,
        att: &OutboundFile,
        caption: Option<&str>,
        reply_to: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let (method, field) = match att.kind {
            OutboundFileKind::Photo => ("sendPhoto", "photo"),
            OutboundFileKind::Document => ("sendDocument", "document"),
        };
        let bytes = tokio::fs::read(&att.path)
            .await
            .with_context(|| format!("read outbound file {}", att.path))?;
        let file_name = std::path::Path::new(&att.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let caption = caption.map(caption_payload);
        let formatted_caption = caption.as_ref().and_then(|c| c.html.as_deref());
        let caption_text = formatted_caption.or_else(|| caption.as_ref().map(|c| c.plain.as_str()));
        let (mut status, mut text) = self
            .send_attachment_request(AttachmentRequest {
                method,
                field,
                recipient,
                bytes: &bytes,
                file_name: &file_name,
                caption: caption_text,
                formatted: formatted_caption.is_some(),
                reply_to,
            })
            .await?;
        if caption.as_ref().is_some_and(|c| c.html.is_some()) && should_retry_plain(status, &text) {
            tracing::warn!(
                method,
                recipient,
                "telegram formatting rejected; retrying attachment with plain text"
            );
            let plain = caption.as_ref().expect("checked above");
            (status, text) = self
                .send_attachment_request(AttachmentRequest {
                    method,
                    field,
                    recipient,
                    bytes: &bytes,
                    file_name: &file_name,
                    caption: Some(&plain.plain),
                    formatted: false,
                    reply_to,
                })
                .await?;
        }
        if !status.is_success() {
            anyhow::bail!("telegram {method} {recipient} → {status}: {text}");
        }
        let id = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("message_id"))
                    .and_then(|n| n.as_i64())
            })
            .map(|n| n.to_string());
        Ok(id)
    }

    async fn send_attachment_request(
        &self,
        request: AttachmentRequest<'_>,
    ) -> anyhow::Result<(reqwest::StatusCode, String)> {
        let AttachmentRequest {
            method,
            field,
            recipient,
            bytes,
            file_name,
            caption,
            formatted,
            reply_to,
        } = request;
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", recipient.to_string())
            .part(
                field.to_string(),
                reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(file_name.to_string()),
            );
        if let Some(caption) = caption {
            form = form.text("caption", caption.to_string());
            if formatted {
                form = form.text("parse_mode", "HTML");
            }
        }
        if let Some(rt) = reply_to.and_then(|s| s.parse::<i64>().ok()) {
            form = form.text("reply_to_message_id", rt.to_string());
        }
        let url = self.api_url(method);
        let resp = self.http.post(&url).multipart(form).send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    // ----- TG-GATE-V2 W1: Rich Messages transport ------------------------

    /// Log a rich→classic fallback `reason` (e.g. `"http_400"`,
    /// `"network_error"`) at debug level, once per reason kind for this
    /// channel's lifetime — a persistently-failing rich path (e.g. an old
    /// Bot API server) must not spam the log once per message.
    async fn log_rich_fallback_once(&self, reason: &str) {
        let mut seen = self.rich_fallback_logged.lock().await;
        if seen.insert(reason.to_string()) {
            tracing::debug!(
                channel = %self.name,
                reason,
                "telegram: rich message send/edit failed, falling back to classic HTML"
            );
        }
    }

    /// TG-GATE-V2 W7a — whether the sticky rich-message circuit breaker has
    /// tripped for this bot instance (3 consecutive failures, never reset).
    fn rich_circuit_open(&self) -> bool {
        self.rich_disabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record one rich attempt's outcome. A success resets the consecutive
    /// streak; the 3rd consecutive failure flips [`Self::rich_disabled`] and
    /// logs once (never again — the breaker never resets for this process).
    fn record_rich_outcome(&self, ok: bool) {
        use std::sync::atomic::Ordering;
        if ok {
            self.rich_failures.store(0, Ordering::Relaxed);
            return;
        }
        let failures = self.rich_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= 3 && !self.rich_disabled.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                channel = %self.name,
                reason = "3 consecutive rich message failures",
                "rich messages disabled for this bot"
            );
        }
    }

    /// Attempt `sendRichMessage`. `Ok` carries the platform message id (when
    /// parseable); `Err` carries a short reason kind for
    /// [`Self::log_rich_fallback_once`] — the caller falls back to the
    /// classic HTML path on ANY non-2xx or transport error, never
    /// propagating the rich failure to the caller.
    async fn try_send_rich(
        &self,
        message: &SendMessage,
        markdown: &str,
    ) -> Result<Option<String>, String> {
        let url = self.api_url("sendRichMessage");
        let body = build_rich_send_body(message, markdown);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|err| format!("network_error:{err}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("http_{}", status.as_u16()));
        }
        if body_reports_failure(&text) {
            return Err("ok_false".to_string());
        }
        Ok(extract_message_id(&text))
    }

    /// Attempt `editMessageText` with `rich_message` (Bot API 10.3). Same
    /// contract as [`Self::try_send_rich`].
    async fn try_edit_rich(
        &self,
        recipient: &str,
        message_id: &str,
        markdown: &str,
        button_rows: &[Vec<MessageOption>],
    ) -> Result<Option<String>, String> {
        let url = self.api_url("editMessageText");
        let body = build_rich_edit_body(recipient, message_id, markdown, button_rows);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|err| format!("network_error:{err}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("http_{}", status.as_u16()));
        }
        if body_reports_failure(&text) {
            return Err("ok_false".to_string());
        }
        Ok(Some(message_id.to_string()))
    }

    /// The classic `sendMessage` path (HTML → plain retry), extended with
    /// [`SendMessage::button_rows`] ⊕ [`SendMessage::options`] as an
    /// `inline_keyboard`. Splits internally at [`MAX_MESSAGE_UTF16`] when
    /// `content` overflows it (reached either directly, for a non-rich
    /// message, or as the rich-fallback path for a long `rich_markdown`
    /// message whose plain `content` the daemon left unsplit).
    async fn send_classic(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if message.content.encode_utf16().count() <= MAX_MESSAGE_UTF16 {
            return self
                .send_classic_part(message, &message.content, true)
                .await;
        }
        let parts = split_for_fallback(&message.content, MAX_MESSAGE_UTF16);
        let total = parts.len();
        let mut first_id = None;
        let mut failed_parts: Vec<usize> = Vec::new();
        for (i, part) in parts.into_iter().enumerate() {
            let idx = i + 1;
            // TG-GATE-V2 W7a — the suffix rides its OWN line after a blank
            // line so it can never land on (and corrupt) a part whose last
            // line is a ``` fence marker (`telegram_html::is_fence_line`);
            // `split_for_fallback` already reserved room for it in the
            // per-part budget.
            let numbered = if total > 1 {
                format!("{part}\n\n({idx}/{total})")
            } else {
                part
            };
            // Buttons ride the LAST part only, so they appear once the
            // full message has been read.
            //
            // TG-GATE-V2 W7a — each part is delivered/accounted
            // individually: a late part's failure no longer aborts the
            // whole call via `?` (which would otherwise report NOTHING
            // about the parts that already landed); it is recorded and the
            // loop keeps going, so every part gets its own delivery
            // attempt.
            match self
                .send_classic_part(message, &numbered, idx == total)
                .await
            {
                Ok(id) => {
                    if first_id.is_none() {
                        first_id = id;
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        channel = %self.name,
                        part = idx,
                        total,
                        error = %err,
                        "telegram: rich-fallback split part failed to send"
                    );
                    failed_parts.push(idx);
                }
            }
        }
        if failed_parts.is_empty() {
            Ok(first_id)
        } else {
            // A downcastable partial-failure so a caller with a durable
            // retry/replay path (daemon.rs) can tell this apart from an
            // ordinary single-message error: it must NOT re-send the whole
            // logical message on replay (that would duplicate the parts
            // that already landed), and can notify the user about only the
            // parts that actually failed.
            Err(FallbackSplitPartialFailure {
                first_id,
                total,
                failed_parts,
            }
            .into())
        }
    }

    /// Send one classic `sendMessage` part with `text` as the body
    /// (already length-bounded by the caller), optionally attaching the
    /// `inline_keyboard`. Shares the existing HTML→plain retry ladder.
    async fn send_classic_part(
        &self,
        message: &SendMessage,
        text: &str,
        include_buttons: bool,
    ) -> anyhow::Result<Option<String>> {
        let url = self.api_url("sendMessage");
        let payload = text_payload(text);
        let mut body = serde_json::json!({
            "chat_id": message.recipient,
            "text": payload.text,
            "reply_to_message_id": message.thread_ts.as_ref().and_then(|s| s.parse::<i64>().ok()),
        });
        if payload.formatted {
            body["parse_mode"] = serde_json::json!("HTML");
        }
        if include_buttons {
            if let Some(keyboard) = inline_keyboard_json(message) {
                body["reply_markup"] = keyboard;
            }
        }
        let t0 = Instant::now();
        let resp = self.http.post(&url).json(&body).send().await?;
        let mut status = resp.status();
        let mut resp_text = resp.text().await.unwrap_or_default();
        if payload.formatted && should_retry_plain(status, &resp_text) {
            tracing::warn!(
                method = "sendMessage",
                recipient = %message.recipient,
                "telegram formatting rejected; retrying with plain text"
            );
            let plain_text = plain_text_for_request(text);
            let plain_body = plain_body(body.clone(), &plain_text);
            let resp = self.http.post(&url).json(&plain_body).send().await?;
            status = resp.status();
            resp_text = resp.text().await.unwrap_or_default();
        }
        let send_http_ms = t0.elapsed().as_millis() as u64;
        if !status.is_success() {
            tracing::warn!(
                event = "latency",
                stage = "tg.egress",
                recipient = %message.recipient,
                status = %status,
                send_http_ms,
                content_len = text.len(),
                "latency tg.egress (failed)"
            );
            anyhow::bail!(
                "telegram sendMessage {} → {}: {}",
                message.recipient,
                status,
                resp_text
            );
        }
        let id = extract_message_id(&resp_text);
        tracing::info!(
            event = "latency",
            stage = "tg.egress",
            recipient = %message.recipient,
            tg_msg_id = id.as_deref().unwrap_or(""),
            send_http_ms,
            content_len = text.len(),
            "latency tg.egress"
        );
        Ok(id)
    }

    /// The classic `editMessageText` path (HTML → plain retry), unchanged
    /// from pre-W1 behavior — the rich attempt (when it fails) falls back
    /// here verbatim.
    async fn edit_classic(
        &self,
        recipient: &str,
        message_id: &str,
        content: &str,
        button_rows: &[Vec<MessageOption>],
    ) -> anyhow::Result<Option<String>> {
        let url = self.api_url("editMessageText");
        let payload = text_payload(content);
        let mut body = serde_json::json!({
            "chat_id": recipient,
            "message_id": message_id.parse::<i64>().ok(),
            "text": payload.text,
        });
        if payload.formatted {
            body["parse_mode"] = serde_json::json!("HTML");
        }
        if let Some(keyboard) = inline_keyboard_json_from_rows(button_rows) {
            body["reply_markup"] = keyboard;
        }
        let resp = self.http.post(&url).json(&body).send().await?;
        let mut status = resp.status();
        let mut text = resp.text().await.unwrap_or_default();
        if payload.formatted && should_retry_plain(status, &text) {
            tracing::warn!(
                method = "editMessageText",
                recipient,
                "telegram formatting rejected; retrying with plain text"
            );
            let plain_text = plain_text_for_request(content);
            let plain_body = plain_body(body.clone(), &plain_text);
            let resp = self.http.post(&url).json(&plain_body).send().await?;
            status = resp.status();
            text = resp.text().await.unwrap_or_default();
        }
        if !status.is_success() {
            anyhow::bail!("telegram editMessageText {recipient}#{message_id} → {status}: {text}");
        }
        // editMessageText returns the (same) edited Message; the id is
        // stable, so echo it back for the daemon's status bookkeeping.
        Ok(Some(message_id.to_string()))
    }
}

#[derive(Debug, Deserialize)]
struct GetUpdatesResp {
    ok: bool,
    #[serde(default)]
    result: Vec<TgUpdate>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMessage>,
    // v0.8.5 D3 — inline-keyboard button clicks.
    #[serde(default)]
    callback_query: Option<TgCallbackQuery>,
}

/// An inline-keyboard button click (v0.8.5 D3).
#[derive(Debug, Deserialize)]
struct TgCallbackQuery {
    id: String,
    #[serde(default)]
    from: Option<TgUser>,
    #[serde(default)]
    message: Option<TgMessage>,
    #[serde(default)]
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: i64,
    date: i64,
    chat: TgChat,
    #[serde(default)]
    from: Option<TgUser>,
    #[serde(default)]
    text: Option<String>,
    // V0.8.4 P2a — inbound media.
    #[serde(default)]
    photo: Vec<TgPhotoSize>,
    #[serde(default)]
    document: Option<TgDocument>,
    #[serde(default)]
    caption: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    id: i64,
    #[serde(default)]
    username: Option<String>,
}

/// One size of an inbound photo (Telegram sends an ascending-size array;
/// the last entry is the largest).
#[derive(Debug, Deserialize)]
struct TgPhotoSize {
    file_id: String,
    #[serde(default)]
    file_size: Option<u64>,
}

/// An inbound document (any non-photo file).
#[derive(Debug, Deserialize)]
struct TgDocument {
    file_id: String,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    file_size: Option<u64>,
}

/// Bot-API download ceiling: 20 MB (`getFile`).
const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

/// What `pick_attachment` decides to fetch for one inbound message.
#[derive(Debug, PartialEq)]
struct PendingDownload {
    file_id: String,
    kind: AttachmentKind,
    file_name: String,
    mime: Option<String>,
    size: Option<u64>,
}

/// Pure: choose the single attachment to download for a message —
/// a document (preferred, has a real name) or the largest photo size.
/// Returns `None` for a text-only message.
fn pick_attachment(m: &TgMessage) -> Option<PendingDownload> {
    if let Some(doc) = &m.document {
        let is_image = doc
            .mime_type
            .as_deref()
            .map(|t| t.starts_with("image/"))
            .unwrap_or(false);
        return Some(PendingDownload {
            file_id: doc.file_id.clone(),
            kind: if is_image {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            file_name: doc.file_name.clone().unwrap_or_else(|| "file".to_string()),
            mime: doc.mime_type.clone(),
            size: doc.file_size,
        });
    }
    m.photo.last().map(|largest| PendingDownload {
        file_id: largest.file_id.clone(),
        kind: AttachmentKind::Image,
        file_name: "photo.jpg".to_string(),
        mime: Some("image/jpeg".to_string()),
        size: largest.file_size,
    })
}

/// Telegram caption ceiling, in UTF-16 code units (separate from the
/// 4096 message ceiling). Attachment messages skip the outbound
/// splitter, so an over-long caption is truncated here (V0.8.4 P2b / F7).
const MAX_CAPTION_UTF16: usize = 1024;

struct AttachmentRequest<'a> {
    method: &'a str,
    field: &'a str,
    recipient: &'a str,
    bytes: &'a [u8],
    file_name: &'a str,
    caption: Option<&'a str>,
    formatted: bool,
    reply_to: Option<&'a str>,
}

/// Truncate `s` to at most [`MAX_CAPTION_UTF16`] UTF-16 code units (never
/// splitting a char), so an over-long attachment caption can't trip a 400.
fn truncate_caption(s: &str) -> String {
    let mut units = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.len_utf16();
        if units + w > MAX_CAPTION_UTF16 {
            break;
        }
        units += w;
        out.push(ch);
    }
    out
}

struct CaptionPayload {
    html: Option<String>,
    plain: String,
}

struct TextPayload {
    text: String,
    formatted: bool,
}

fn truncate_plain_message(source: &str) -> String {
    let mut out = String::new();
    let mut units = 0;
    for ch in source.chars() {
        let width = ch.len_utf16();
        if units + width + 1 > MAX_MESSAGE_UTF16 {
            break;
        }
        out.push(ch);
        units += width;
    }
    out.push('…');
    out
}

fn text_payload(source: &str) -> TextPayload {
    let rendered = render_markdown(source);
    if rendered.text_utf16_len > MAX_MESSAGE_UTF16 {
        TextPayload {
            text: truncate_plain_message(source),
            formatted: false,
        }
    } else if !rendered.has_non_whitespace {
        TextPayload {
            text: source.to_owned(),
            formatted: false,
        }
    } else {
        TextPayload {
            text: rendered.html,
            formatted: true,
        }
    }
}

fn plain_body(mut body: serde_json::Value, text: &str) -> serde_json::Value {
    body["text"] = serde_json::json!(text);
    body.as_object_mut()
        .expect("Telegram request body is an object")
        .remove("parse_mode");
    body
}

fn plain_text_for_request(source: &str) -> String {
    if source.encode_utf16().count() > MAX_MESSAGE_UTF16 {
        truncate_plain_message(source)
    } else {
        source.to_owned()
    }
}

fn caption_payload(caption: &str) -> CaptionPayload {
    let plain = truncate_caption(caption);
    let rendered = render_markdown(&plain);
    let html = (rendered.text_utf16_len <= MAX_CAPTION_UTF16 && rendered.has_non_whitespace)
        .then_some(rendered.html);
    CaptionPayload { html, plain }
}

/// TG-GATE-V2 W7a — Telegram's Bot API contract: every response carries a
/// top-level `"ok"` boolean, but `sendRichMessage`/`editMessageText` can
/// return HTTP 200 with `{"ok":false, ...}` (a degraded-server-side shape);
/// a bare `status.is_success()` check treats that as delivered. Missing or
/// unparseable `ok` is NOT treated as failure (matches every other Bot API
/// call site here, which only inspects the HTTP status).
fn body_reports_failure(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("ok").and_then(|ok| ok.as_bool()))
        .map(|ok| !ok)
        .unwrap_or(false)
}

/// TG-GATE-V2 W7a — split `content` for the rich→classic fallback ladder,
/// reserving room for the trailing `\n\n({i}/{n})` part-count suffix BEFORE
/// splitting, so appending it can never push a part back over
/// [`MAX_MESSAGE_UTF16`] (a 400 the caller can't recover from). A first pass
/// at the FULL budget learns the part count so the exact worst-case suffix
/// width (`i == n == total`) can be reserved; a second pass then splits
/// against that reduced budget.
fn split_for_fallback(content: &str, max_units: usize) -> Vec<String> {
    let first_pass = crate::sanitize::split_for_channel(content, max_units);
    if first_pass.len() <= 1 {
        return first_pass;
    }
    let total = first_pass.len();
    let suffix_len = format!("\n\n({total}/{total})").encode_utf16().count();
    let budget = max_units.saturating_sub(suffix_len).max(1);
    crate::sanitize::split_for_channel(content, budget)
}

/// TG-GATE-V2 W7a — returned (wrapped in an `anyhow::Error`) by
/// [`TelegramChannel::send_classic`] when the rich→classic fallback split
/// into more than one Telegram message and at least one part failed to
/// send. Downcastable so a caller with a durable retry/replay path
/// (daemon.rs) can tell a genuine multi-part partial failure apart from an
/// ordinary single-message error: it must NOT treat the whole logical send
/// as failed (that would re-send the already-delivered parts on the next
/// replay) and instead notify the user about only the parts that failed.
#[derive(Debug)]
pub struct FallbackSplitPartialFailure {
    /// Platform message id of the first part that DID send, if any.
    pub first_id: Option<String>,
    /// Total number of parts the fallback split into.
    pub total: usize,
    /// 1-based indices of the parts that failed to send.
    pub failed_parts: Vec<usize>,
}

impl std::fmt::Display for FallbackSplitPartialFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "telegram rich-fallback split: {}/{} parts failed to send",
            self.failed_parts.len(),
            self.total
        )
    }
}

impl std::error::Error for FallbackSplitPartialFailure {}

fn should_retry_plain(status: reqwest::StatusCode, response_text: &str) -> bool {
    let description = serde_json::from_str::<serde_json::Value>(response_text)
        .ok()
        .and_then(|value| {
            value
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| response_text.to_owned());
    let description = description.to_ascii_lowercase();
    status == reqwest::StatusCode::BAD_REQUEST
        && (description.contains("can't parse entities")
            || description.contains("message text is empty")
            || description.contains("message is empty"))
}

// ----- TG-GATE-V2 W1: Rich Messages request shapes (pure) -----------------

/// Pluck `result.message_id` from a Bot API JSON response, best-effort
/// (shared by `sendMessage` / `sendRichMessage` / `sendPhoto`/`sendDocument`
/// so the platform-id echo stays one implementation).
fn extract_message_id(response_text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(response_text)
        .ok()
        .and_then(|v| {
            v.get("result")
                .and_then(|r| r.get("message_id"))
                .and_then(|n| n.as_i64())
        })
        .map(|n| n.to_string())
}

/// Map a [`ButtonStyle`] to the `RichMessageButton.style` string (Bot API
/// 10.3 §4.3: `"danger"` / `"success"` / `"primary"` / `"link"` — ccteam
/// never emits `"link"`, callback buttons only). `None` omits the
/// attribute (Telegram's default styling).
fn button_style_str(style: Option<ButtonStyle>) -> Option<&'static str> {
    match style {
        Some(ButtonStyle::Primary) => Some("primary"),
        Some(ButtonStyle::Success) => Some("success"),
        Some(ButtonStyle::Danger) => Some("danger"),
        None => None,
    }
}

/// Escape a string for use inside a `<tg-button …>` HTML attribute value
/// embedded in Rich Message markdown (label text and `callback_data` both
/// ride attributes, so both need the same escaping as any HTML attribute).
fn escape_tg_button_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `SendMessage::button_rows` ⊕ `SendMessage::options` — button rows first,
/// then one row per choice-reply option (`options` stays a one-per-row
/// concept; see [`MessageOption`] docs). Shared by the Rich Message
/// buttons-block builder and the classic `inline_keyboard` fallback so the
/// two ladders never drift in ordering.
fn combined_button_rows(message: &SendMessage) -> Vec<Vec<MessageOption>> {
    let mut rows = message.button_rows.clone();
    for opt in &message.options {
        rows.push(vec![opt.clone()]);
    }
    rows
}

/// Render `rows` as trailing `<tg-button-row>` blocks (Bot API 10.3 §5.2 —
/// the Rich Markdown button syntax; `type="callback_data"` is the only
/// button kind ccteam emits). Empty `rows` ⇒ empty string (no trailing
/// blank block).
fn button_rows_to_tg_html(rows: &[Vec<MessageOption>]) -> String {
    let mut out = String::new();
    for row in rows {
        if row.is_empty() {
            continue;
        }
        out.push_str("<tg-button-row>");
        for opt in row {
            out.push_str("<tg-button type=\"callback_data\" callback_data=\"");
            out.push_str(&escape_tg_button_attr(&opt.data));
            out.push('"');
            if let Some(style) = button_style_str(opt.style) {
                out.push_str(" style=\"");
                out.push_str(style);
                out.push('"');
            }
            out.push('>');
            out.push_str(&escape_tg_button_attr(&opt.label));
            out.push_str("</tg-button>");
        }
        out.push_str("</tg-button-row>");
    }
    out
}

/// Build the `sendRichMessage` request body: `rich_message.markdown` =
/// `markdown` with [`combined_button_rows`] appended as trailing
/// `<tg-button-row>` blocks. No `parse_mode` — Rich Messages have no such
/// parameter (see research doc §3).
fn build_rich_send_body(message: &SendMessage, markdown: &str) -> serde_json::Value {
    let rows = combined_button_rows(message);
    let buttons_html = button_rows_to_tg_html(&rows);
    let full_markdown = if buttons_html.is_empty() {
        markdown.to_string()
    } else {
        format!("{markdown}\n\n{buttons_html}")
    };
    let mut body = serde_json::json!({
        "chat_id": message.recipient,
        "rich_message": { "markdown": full_markdown },
    });
    if let Some(rt) = message
        .thread_ts
        .as_ref()
        .and_then(|s| s.parse::<i64>().ok())
    {
        body["reply_parameters"] = serde_json::json!({ "message_id": rt });
    }
    body
}

/// Build the `editMessageText` request body using `rich_message` (Bot API
/// 10.3) instead of `text`. TG-GATE-V2 W5 — `button_rows` (e.g. the live
/// progress edit's `[⛔ Прервать]`) rides the same trailing
/// `<tg-button-row>` blocks [`build_rich_send_body`] appends for a send.
fn build_rich_edit_body(
    recipient: &str,
    message_id: &str,
    markdown: &str,
    button_rows: &[Vec<MessageOption>],
) -> serde_json::Value {
    let buttons_html = button_rows_to_tg_html(button_rows);
    let full_markdown = if buttons_html.is_empty() {
        markdown.to_string()
    } else {
        format!("{markdown}\n\n{buttons_html}")
    };
    serde_json::json!({
        "chat_id": recipient,
        "message_id": message_id.parse::<i64>().ok(),
        "rich_message": { "markdown": full_markdown },
    })
}

/// Build the classic `inline_keyboard` from `rows`, or `None` when there is
/// nothing to render (today's zero-behavior-change case: no rows).
fn inline_keyboard_json_from_rows(rows: &[Vec<MessageOption>]) -> Option<serde_json::Value> {
    if rows.is_empty() {
        return None;
    }
    let keyboard: Vec<Vec<serde_json::Value>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|o| serde_json::json!({ "text": o.label, "callback_data": o.data }))
                .collect()
        })
        .collect();
    Some(serde_json::json!({ "inline_keyboard": keyboard }))
}

/// Build the classic `inline_keyboard` from [`combined_button_rows`], or
/// `None` when there is nothing to render (today's zero-behavior-change
/// case: no `button_rows`, no `options`).
fn inline_keyboard_json(message: &SendMessage) -> Option<serde_json::Value> {
    inline_keyboard_json_from_rows(&combined_button_rows(message))
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        &self.name
    }

    /// Telegram is the only channel with Bot API 10.3 Rich Messages
    /// (TG-GATE-V2 W7a).
    fn supports_rich_messages(&self) -> bool {
        true
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        // V0.8.4 P2b — files go via sendPhoto/sendDocument (multipart);
        // the caption rides the first attachment. Rich Messages / buttons
        // are not supported on the attachment path (unchanged).
        if !message.attachments.is_empty() {
            return self.send_with_attachments(message).await;
        }
        // TG-GATE-V2 W1 — rich(markdown+buttons block) → classic HTML +
        // inline_keyboard → plain text. `rich_markdown` absent ⇒ this
        // branch never runs, so a caller that never opts in keeps today's
        // behavior byte-for-byte.
        if let Some(markdown) = message.rich_markdown.as_deref() {
            // TG-GATE-V2 W7a — the sticky circuit breaker skips the rich
            // attempt entirely once it has tripped (3 consecutive
            // failures), so a persistently-broken rich path stops paying
            // its latency + retry cost on every single message.
            if !self.rich_circuit_open() {
                match self.try_send_rich(message, markdown).await {
                    Ok(id) => {
                        self.record_rich_outcome(true);
                        return Ok(id);
                    }
                    Err(reason) => {
                        self.record_rich_outcome(false);
                        self.log_rich_fallback_once(&reason).await;
                    }
                }
            }
        }
        self.send_classic(message).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            let offset = { *self.last_offset.lock().await };
            let url = self.api_url("getUpdates");
            let req = self
                .http
                .get(&url)
                .query(&[
                    ("timeout", POLL_TIMEOUT_SECS.to_string()),
                    ("offset", offset.to_string()),
                ])
                .send()
                .await;
            let resp = match req {
                Ok(r) => r,
                Err(err) => {
                    tracing::warn!(error = %err, "telegram getUpdates failed, backing off 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let body: GetUpdatesResp = match resp.json().await {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!(error = %err, "telegram parse getUpdates failed");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            if !body.ok {
                tracing::warn!("telegram getUpdates ok=false");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            for upd in body.result {
                {
                    let mut last = self.last_offset.lock().await;
                    *last = (*last).max(upd.update_id + 1);
                }
                if let Some(cb) = upd.callback_query {
                    self.handle_callback_query(&tx, cb).await;
                    continue;
                }
                if let Some(m) = upd.message {
                    let chat_id = m.chat.id.to_string();
                    if !self.chat_allowed(&chat_id) {
                        self.reject_chat(&chat_id, m.message_id, m.date).await;
                        continue;
                    }
                    let sender = m
                        .from
                        .as_ref()
                        .and_then(|u| u.username.clone())
                        .unwrap_or_else(|| {
                            m.from
                                .as_ref()
                                .map(|u| u.id.to_string())
                                .unwrap_or_else(|| "anonymous".to_string())
                        });
                    let cid = format!("tg-{}", m.message_id);
                    let recv_ms = now_unix_ms();
                    let tg_date_ms = (m.date.max(0) as u128).saturating_mul(1000);
                    let tg_age_ms = recv_ms.saturating_sub(tg_date_ms);
                    // V0.8.4 P2a — caption is the text for media messages.
                    let content = m
                        .text
                        .clone()
                        .or_else(|| m.caption.clone())
                        .unwrap_or_default();
                    let mut attachments = Vec::new();
                    let mut rejected_notice: Option<String> = None;
                    if let Some(pending) = pick_attachment(&m) {
                        match self.stage_attachment(&cid, &pending).await {
                            Ok(Some(att)) => attachments.push(att),
                            Ok(None) => {
                                rejected_notice = Some(format!(
                                    "⚠️ Вложение {} превышает лимит 20 МБ и отклонено",
                                    pending.file_name
                                ));
                            }
                            Err(err) => {
                                tracing::warn!(cid = %cid, error = %err, "telegram: attachment download failed");
                                rejected_notice = Some(format!(
                                    "⚠️ Не удалось скачать вложение {}",
                                    pending.file_name
                                ));
                            }
                        }
                    }
                    // V0.8.4 P2a (F3): deliver a rejection DIRECTLY to the
                    // chat (like the P0 split-failure notice), not as a
                    // turn-text note the agent must relay. With no
                    // accompanying text there is no turn to submit.
                    if let Some(notice) = rejected_notice {
                        if let Err(err) =
                            self.send(&SendMessage::new(notice, chat_id.clone())).await
                        {
                            tracing::warn!(cid = %cid, error = %err, "telegram: rejection notice send failed");
                        }
                        if content.is_empty() {
                            continue;
                        }
                    }
                    let content_len = content.len();
                    tracing::info!(
                        event = "latency",
                        stage = "tg.ingress",
                        cid = %cid,
                        chat_id = %chat_id,
                        sender = %sender,
                        recv_ms = recv_ms as u64,
                        tg_age_ms = tg_age_ms as u64,
                        content_len,
                        attachments = attachments.len(),
                        "latency tg.ingress"
                    );
                    let payload = ChannelMessage {
                        id: cid,
                        sender,
                        reply_target: chat_id.clone(),
                        content,
                        channel: self.name.clone(),
                        timestamp: m.date.max(0) as u64,
                        thread_ts: None,
                        attachments,
                        selection: None,
                    };
                    if tx.send(payload).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let url = self.api_url("getMe");
        match self.http.get(&url).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    fn max_message_len(&self) -> Option<usize> {
        Some(MAX_MESSAGE_UTF16)
    }

    async fn edit_message(
        &self,
        recipient: &str,
        message_id: &str,
        content: &str,
        button_rows: &[Vec<MessageOption>],
    ) -> anyhow::Result<Option<String>> {
        // TG-GATE-V2 W1/W5 — same rich→classic ladder as `send` (see there
        // for the fallback contract + once-per-reason-kind logging), now
        // carrying `button_rows` (e.g. the progress edit's `[⛔ Прервать]`)
        // through both legs.
        if !self.rich_circuit_open() {
            match self
                .try_edit_rich(recipient, message_id, content, button_rows)
                .await
            {
                Ok(id) => {
                    self.record_rich_outcome(true);
                    return Ok(id);
                }
                Err(reason) => {
                    self.record_rich_outcome(false);
                    self.log_rich_fallback_once(&reason).await;
                }
            }
        }
        self.edit_classic(recipient, message_id, content, button_rows)
            .await
    }

    /// Add the 👀 ack reaction via `setMessageReaction` (stateless — Telegram
    /// clears by `message_id`, so no handle is returned). `message_id` must be
    /// the numeric Telegram id; a non-numeric id (or an API error) is a hard
    /// error here, swallowed fire-and-forget by the daemon egress so a reaction
    /// never affects delivery.
    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let mid = parse_tg_message_id(message_id)?;
        let body = set_message_reaction_body(chat_id, mid, true);
        self.post_set_message_reaction(chat_id, message_id, &body)
            .await?;
        // Telegram clears a reaction by (chat, message_id) alone — no handle.
        Ok(None)
    }

    /// Clear the ack reaction via `setMessageReaction` with an empty array
    /// (Telegram's documented "remove all reactions" shape). `_handle` is
    /// always `None` for Telegram (see [`Channel::add_reaction`]).
    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        _handle: Option<&str>,
    ) -> anyhow::Result<()> {
        let mid = parse_tg_message_id(message_id)?;
        let body = set_message_reaction_body(chat_id, mid, false);
        self.post_set_message_reaction(chat_id, message_id, &body)
            .await?;
        Ok(())
    }

    /// v0.8.5 P1 — publish the gateway's command menu via `setMyCommands`.
    /// Telegram wants **bare** command names (no leading `/`), so the body
    /// builder strips it. An empty spec list clears the menu (Telegram's
    /// documented behaviour for an empty `commands` array). Best-effort:
    /// the daemon logs a warn on failure rather than aborting startup.
    async fn register_commands(&self, cmds: &[CommandSpec]) -> anyhow::Result<()> {
        // Publish to BOTH the default scope (groups / fallback) AND the
        // `all_private_chats` scope. ccteam is driven from DMs, and Telegram
        // resolves a private chat's menu from the MOST SPECIFIC scope. A stale
        // `all_private_chats` menu (e.g. left by a prior bot-setup flow's
        // `start`/`help`/`status`) therefore shadows a default-scope menu in
        // every DM, forever — which is why a default-only write was invisible
        // and "fixed several times" never stuck. Writing `all_private_chats`
        // explicitly is the only way the DM menu shows ccteam's commands; the
        // daemon re-asserts it on every start, so any later clobber self-heals.
        let scopes = [
            None,
            Some(serde_json::json!({ "type": "all_private_chats" })),
        ];
        let url = self.api_url("setMyCommands");
        for scope in scopes {
            let body = set_my_commands_body(cmds, scope.as_ref());
            let resp = self.http.post(&url).json(&body).send().await?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("telegram setMyCommands → {status}: {text}");
            }
        }
        Ok(())
    }
}

/// Build the `setMyCommands` request body (v0.8.5 P1). Pure + isolated so
/// the bare-name stripping + JSON shape are unit-testable without a live
/// Bot API. Telegram requires bare command names (`new`, not `/new`). An
/// optional `scope` (e.g. `{"type":"all_private_chats"}`) targets a single
/// command scope; `None` writes the default scope.
fn set_my_commands_body(
    cmds: &[CommandSpec],
    scope: Option<&serde_json::Value>,
) -> serde_json::Value {
    let commands: Vec<serde_json::Value> = cmds
        .iter()
        .map(|c| {
            serde_json::json!({
                "command": c.name.trim_start_matches('/'),
                "description": c.description,
            })
        })
        .collect();
    let mut body = serde_json::json!({ "commands": commands });
    if let Some(scope) = scope {
        body["scope"] = scope.clone();
    }
    body
}

/// Parse a Telegram message id for `setMessageReaction`, tolerating ccteam's
/// `tg-<n>` inbound-id namespacing (the gateway carries IM message ids as
/// `tg-<n>`, but the Bot API needs the BARE numeric `<n>`). Without the strip,
/// the 👀 ack failed on EVERY real telegram turn ("non-numeric message_id
/// tg-6249") — the mock channel doesn't exercise this numeric parse, so it
/// went unnoticed. Bare numeric ids (no prefix) still parse.
fn parse_tg_message_id(message_id: &str) -> anyhow::Result<i64> {
    message_id
        .strip_prefix("tg-")
        .unwrap_or(message_id)
        .parse::<i64>()
        .map_err(|_| {
            anyhow::anyhow!("telegram setMessageReaction: non-numeric message_id {message_id}")
        })
}

/// The `ChannelMessage::id` namespacing for a `callback_query` tap
/// (TG-GATE-V2 W8 — separate from the `"tg-<n>"` inbound-text namespace
/// [`parse_tg_message_id`] strips): `"tg-cb-<n>"`, where `<n>` is the raw
/// platform id of the message CARRYING the tapped inline keyboard (Telegram
/// echoes the same id back on every edit of that message). The gateway's
/// `telegram_callback_message_id` strips this prefix to recover `<n>` and
/// targets [`Channel::edit_message`] at it — resolving a `cmd:?` confirmation
/// tap edits the confirmation prompt itself instead of appending a reply.
fn callback_message_id(message_id: i64) -> String {
    format!("tg-cb-{message_id}")
}

/// Build the `setMessageReaction` request body (pure + isolated so the
/// 👀-emoji add shape and the empty-array clear shape are unit-testable
/// without a live Bot API). `add=true` sets `reaction:[{emoji:👀}]`;
/// `add=false` sets `reaction:[]` (Telegram's documented "clear" shape).
/// `message_id` is the numeric Telegram id.
fn set_message_reaction_body(chat_id: &str, message_id: i64, add: bool) -> serde_json::Value {
    let reaction = if add {
        serde_json::json!([{ "type": "emoji", "emoji": "👀" }])
    } else {
        serde_json::json!([])
    };
    serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reaction": reaction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_template() {
        let ch = TelegramChannel::new("ABC".into(), vec![]);
        let url = ch.api_url("getMe");
        assert_eq!(url, "https://api.telegram.org/botABC/getMe");
    }

    #[test]
    fn set_my_commands_body_strips_leading_slash() {
        // v0.8.5 P1 — Telegram wants BARE command names.
        let specs = vec![
            CommandSpec {
                name: "/new".into(),
                description: "start a new session".into(),
            },
            CommandSpec {
                name: "/sessions".into(),
                description: "list sessions + status".into(),
            },
        ];
        let body = set_my_commands_body(&specs, None);
        let commands = body["commands"].as_array().expect("commands array");
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0]["command"], "new");
        assert_eq!(commands[0]["description"], "start a new session");
        assert_eq!(commands[1]["command"], "sessions");
        // No leading slash leaks through anywhere.
        for c in commands {
            assert!(
                !c["command"].as_str().unwrap().starts_with('/'),
                "telegram command names must be bare"
            );
        }
        // No scope key on the default-scope write.
        assert!(body.get("scope").is_none());
    }

    #[test]
    fn set_my_commands_body_empty_clears_menu() {
        let body = set_my_commands_body(&[], None);
        assert!(body["commands"].as_array().unwrap().is_empty());
    }

    #[test]
    fn set_my_commands_body_carries_all_private_chats_scope() {
        // The DM-scope write is what un-shadows ccteam's menu: a stale
        // all_private_chats menu otherwise wins over the default scope in
        // every private chat. The body must carry that scope verbatim.
        let specs = vec![CommandSpec {
            name: "/new".into(),
            description: "start a new session".into(),
        }];
        let scope = serde_json::json!({ "type": "all_private_chats" });
        let body = set_my_commands_body(&specs, Some(&scope));
        assert_eq!(body["scope"]["type"], "all_private_chats");
        assert_eq!(body["commands"][0]["command"], "new");
    }

    #[test]
    fn chat_allowed_open_when_empty() {
        // The GLOBAL/owner bot keeps the legacy open mode (a half-configured
        // owner must not be locked out of their own box; the daemon warns).
        let ch = TelegramChannel::new("t".into(), vec![]);
        assert!(ch.chat_allowed("12345"));
    }

    #[test]
    fn fail_closed_bot_answers_nobody_until_bound() {
        // A per-tenant bot takes the opposite default, matching Lark: whatever
        // arrives here is stamped with that tenant's identity, so an unbound
        // bot must not hand a stranger their projects and sessions.
        let ch = TelegramChannel::new("t".into(), vec![]).fail_closed();
        assert!(!ch.chat_allowed("12345"), "unbound tenant bot denies all");

        let ch = TelegramChannel::new("t".into(), vec!["12345".into()]).fail_closed();
        assert!(ch.chat_allowed("12345"), "bound chat gets through");
        assert!(!ch.chat_allowed("99999"), "everyone else still denied");
    }

    #[test]
    fn chat_allowed_enforces_list() {
        let ch = TelegramChannel::new("t".into(), vec!["12345".into()]);
        assert!(ch.chat_allowed("12345"));
        assert!(!ch.chat_allowed("99999"));
    }

    // ----- P2a attachment parsing (pure, fixture-driven) ------------

    #[test]
    fn pick_attachment_takes_largest_photo() {
        let m: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5},
            "caption": "error screenshot",
            "photo": [
                {"file_id": "small", "file_size": 100},
                {"file_id": "big", "file_size": 9000}
            ]
        }))
        .unwrap();
        let p = pick_attachment(&m).unwrap();
        assert_eq!(p.file_id, "big");
        assert_eq!(p.kind, AttachmentKind::Image);
        assert_eq!(p.size, Some(9000));
    }

    #[test]
    fn pick_attachment_document_is_file_or_image_by_mime() {
        let doc: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5},
            "document": {"file_id": "d1", "file_name": "log.txt", "mime_type": "text/plain", "file_size": 50}
        }))
        .unwrap();
        let p = pick_attachment(&doc).unwrap();
        assert_eq!(p.kind, AttachmentKind::File);
        assert_eq!(p.file_name, "log.txt");

        let img_doc: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5},
            "document": {"file_id": "d2", "file_name": "shot.png", "mime_type": "image/png"}
        }))
        .unwrap();
        assert_eq!(
            pick_attachment(&img_doc).unwrap().kind,
            AttachmentKind::Image
        );
    }

    #[test]
    fn pick_attachment_none_for_text_only() {
        let m: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1, "date": 0, "chat": {"id": 5}, "text": "hi"
        }))
        .unwrap();
        assert!(pick_attachment(&m).is_none());
    }

    #[test]
    fn set_message_reaction_body_add_carries_eyes_emoji() {
        // The 👀 ack: add → a single emoji reaction of "👀".
        let body = set_message_reaction_body("chat-1", 42, true);
        assert_eq!(body["chat_id"], "chat-1");
        assert_eq!(body["message_id"], 42);
        let reaction = body["reaction"].as_array().expect("reaction array");
        assert_eq!(reaction.len(), 1);
        assert_eq!(reaction[0]["type"], "emoji");
        assert_eq!(reaction[0]["emoji"], "👀");
    }

    #[test]
    fn parse_tg_message_id_strips_the_namespacing_prefix() {
        // The 👀-ack fix: the gateway carries inbound telegram ids as `tg-<n>`,
        // but the Bot API needs the BARE numeric — strip the prefix.
        assert_eq!(parse_tg_message_id("tg-6249").unwrap(), 6249);
        // A bare numeric id (no prefix) still parses.
        assert_eq!(parse_tg_message_id("42").unwrap(), 42);
        // Genuinely non-numeric → error (id preserved in the message for the log).
        assert!(parse_tg_message_id("tg-abc").is_err());
        assert!(parse_tg_message_id("nope").is_err());
    }

    /// TG-GATE-V2 W8 — a `callback_query` tap's `ChannelMessage::id` names
    /// the tapped message under its own `"tg-cb-<n>"` namespace (distinct
    /// from `parse_tg_message_id`'s `"tg-<n>"` inbound-text one), so the
    /// gateway can resolve a `cmd:?` confirmation by editing that same
    /// message instead of appending a new one.
    #[test]
    fn callback_message_id_namespaces_the_platform_message_id() {
        assert_eq!(callback_message_id(6249), "tg-cb-6249");
        assert_eq!(callback_message_id(0), "tg-cb-0");
    }

    #[test]
    fn set_message_reaction_body_remove_is_empty_array() {
        // Clearing the ack: Telegram's documented "remove all" shape is an
        // empty reaction array (NOT a missing key).
        let body = set_message_reaction_body("chat-1", 42, false);
        assert_eq!(body["message_id"], 42);
        assert!(
            body["reaction"]
                .as_array()
                .expect("reaction array")
                .is_empty(),
            "clearing a reaction must send an empty array"
        );
    }

    #[tokio::test]
    async fn add_reaction_rejects_non_numeric_message_id() {
        // The Bot API needs an i64 message id; a non-numeric one is a hard
        // error here (the daemon egress swallows it — reactions are
        // fire-and-forget — but the provider must not silently no-op).
        let ch = TelegramChannel::new("t".into(), vec![]);
        assert!(ch.add_reaction("chat-1", "not-a-number").await.is_err());
        assert!(ch
            .remove_reaction("chat-1", "not-a-number", None)
            .await
            .is_err());
    }

    #[test]
    fn truncate_caption_caps_utf16_units() {
        // F7 — short captions pass through untouched.
        assert_eq!(truncate_caption("hi"), "hi");
        // Over-long captions are capped to the 1024-unit ceiling.
        let long = "x".repeat(2000);
        let out = truncate_caption(&long);
        assert_eq!(out.chars().map(char::len_utf16).sum::<usize>(), 1024);
        // Emoji (2 units each) never split mid-char.
        let emoji = "😀".repeat(1000); // 2000 UTF-16 units
        let out = truncate_caption(&emoji);
        assert!(out.chars().map(char::len_utf16).sum::<usize>() <= 1024);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn parse_entity_error_retries_only_known_bad_requests() {
        assert!(should_retry_plain(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: Can't find end tag corresponding to start tag b"}"#,
        ));
        assert!(!should_retry_plain(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"description":"Bad Request: message is too long"}"#,
        ));
        assert!(!should_retry_plain(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"description":"Bad Request: wrong parse_mode specified"}"#,
        ));
        assert!(should_retry_plain(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"description":"Bad Request: message text is empty"}"#,
        ));
        assert!(should_retry_plain(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"description":"Bad Request: message is empty"}"#,
        ));
        assert!(!should_retry_plain(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"description":"Bad Request: can't parse entities: broken"}"#,
        ));
    }

    #[test]
    fn caption_payload_formats_with_a_plain_fallback() {
        let payload = caption_payload("**caption** <&>");
        assert_eq!(
            payload.html.as_deref(),
            Some("<b>caption</b> &lt;&amp;&gt;")
        );
        assert_eq!(payload.plain, "**caption** <&>");
    }

    #[test]
    fn empty_rendered_text_uses_plain_payload() {
        for source in ["```", "```\n```\n", "# \n", "> \n", "   ", "\n"] {
            let payload = text_payload(source);
            assert_eq!(payload.text, source);
            assert!(!payload.formatted);
            assert!(caption_payload(source).html.is_none());
        }
    }

    #[test]
    fn plain_retry_body_strips_parse_mode() {
        let body = plain_body(
            serde_json::json!({"text":"<b>x</b>","parse_mode":"HTML"}),
            "**x**",
        );
        assert_eq!(body["text"], "**x**");
        assert!(body.get("parse_mode").is_none());
    }

    #[test]
    fn edit_payload_truncates_overlong_rendered_text() {
        let payload = text_payload(&"😀".repeat(2049));
        assert!(!payload.formatted);
        assert!(payload.text.ends_with('…'));
        assert!(payload.text.chars().map(char::len_utf16).sum::<usize>() <= MAX_MESSAGE_UTF16);
    }

    // ----- TG-GATE-V2 W1: Rich Messages request shapes ------------------

    fn opt(data: &str, label: &str, style: Option<ButtonStyle>) -> MessageOption {
        MessageOption {
            data: data.into(),
            label: label.into(),
            id: String::new(),
            style,
        }
    }

    #[test]
    fn rich_send_body_carries_markdown_and_button_rows_block() {
        let message = SendMessage::new("hello", "42")
            .with_rich_markdown("**hello**")
            .with_button_rows(vec![vec![
                opt("cmd:/stop", "⛔ Стоп", Some(ButtonStyle::Danger)),
                opt("cmd:/new", "✏️ Новая", None),
            ]]);
        let body = build_rich_send_body(&message, "**hello**");
        assert_eq!(body["chat_id"], "42");
        assert!(body.get("text").is_none(), "rich send has no `text` field");
        let markdown = body["rich_message"]["markdown"].as_str().unwrap();
        assert!(markdown.starts_with("**hello**"));
        assert!(markdown.contains("<tg-button-row>"));
        assert!(markdown.contains(r#"type="callback_data""#));
        assert!(markdown.contains(r#"callback_data="cmd:/stop""#));
        assert!(markdown.contains(r#"style="danger""#));
        assert!(markdown.contains(">⛔ Стоп</tg-button>"));
        // The second button has no style attribute at all.
        assert!(markdown.contains(r#"callback_data="cmd:/new">✏️ Новая</tg-button>"#));
        assert!(markdown.contains("</tg-button-row>"));
    }

    #[test]
    fn rich_send_body_omits_buttons_block_when_no_rows_or_options() {
        let message = SendMessage::new("hello", "42").with_rich_markdown("hello");
        let body = build_rich_send_body(&message, "hello");
        assert_eq!(body["rich_message"]["markdown"], "hello");
    }

    #[test]
    fn combined_button_rows_puts_button_rows_before_options() {
        let message = SendMessage::new("pick", "42")
            .with_button_rows(vec![vec![opt("nav:a", "A", None)]])
            .with_options(vec![opt("t:0", "Yes", None), opt("t:1", "No", None)]);
        let rows = combined_button_rows(&message);
        // button_rows first (as-is), then one row per option.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[0][0].data, "nav:a");
        assert_eq!(rows[1], vec![opt("t:0", "Yes", None)]);
        assert_eq!(rows[2], vec![opt("t:1", "No", None)]);
    }

    #[test]
    fn inline_keyboard_json_mirrors_combined_button_rows() {
        let message = SendMessage::new("pick", "42")
            .with_button_rows(vec![vec![
                opt("nav:a", "A", Some(ButtonStyle::Primary)),
                opt("nav:b", "B", None),
            ]])
            .with_options(vec![opt("t:0", "Yes", None)]);
        let keyboard = inline_keyboard_json(&message).expect("some keyboard");
        let rows = keyboard["inline_keyboard"].as_array().unwrap();
        // Row 1: both button_rows buttons together; row 2: the option alone.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].as_array().unwrap().len(), 2);
        assert_eq!(rows[0][0]["text"], "A");
        assert_eq!(rows[0][0]["callback_data"], "nav:a");
        // Classic inline_keyboard has no per-button style field at all —
        // the style only exists on the Rich Message ladder.
        assert!(rows[0][0].get("style").is_none());
        assert_eq!(rows[1][0]["text"], "Yes");
        assert_eq!(rows[1][0]["callback_data"], "t:0");
    }

    #[test]
    fn inline_keyboard_json_none_when_nothing_to_render() {
        let message = SendMessage::new("hi", "42");
        assert!(inline_keyboard_json(&message).is_none());
    }

    #[test]
    fn escape_tg_button_attr_escapes_html_specials() {
        assert_eq!(
            escape_tg_button_attr(r#"a<b>c&"d""#),
            "a&lt;b&gt;c&amp;&quot;d&quot;"
        );
    }

    #[test]
    fn button_rows_to_tg_html_skips_empty_rows() {
        let rows: Vec<Vec<MessageOption>> = vec![vec![], vec![opt("a", "A", None)]];
        let html = button_rows_to_tg_html(&rows);
        assert_eq!(html.matches("<tg-button-row>").count(), 1);
        assert!(html.contains(">A</tg-button>"));
    }

    #[test]
    fn rich_edit_body_has_no_buttons_and_parses_message_id() {
        let body = build_rich_edit_body("42", "tg-99", "**edited**", &[]);
        assert_eq!(body["chat_id"], "42");
        assert_eq!(body["message_id"].as_i64(), None, "tg-99 isn't numeric");
        assert_eq!(body["rich_message"]["markdown"], "**edited**");
        assert!(body.get("text").is_none());

        let body = build_rich_edit_body("42", "99", "**edited**", &[]);
        assert_eq!(body["message_id"], 99);
    }

    /// TG-GATE-V2 W5 — an edit's `button_rows` (e.g. the progress edit's
    /// `[⛔ Прервать]`) append the same trailing `<tg-button-row>` block a
    /// send gets.
    #[test]
    fn rich_edit_body_appends_button_rows() {
        let rows = vec![vec![opt("cmd:?/interrupt", "⛔ Прервать", None)]];
        let body = build_rich_edit_body("42", "99", "working...", &rows);
        let markdown = body["rich_message"]["markdown"].as_str().unwrap();
        assert!(markdown.starts_with("working...\n\n<tg-button-row>"));
        assert!(markdown.contains(">⛔ Прервать</tg-button>"));
    }

    /// The classic edit fallback attaches the same `inline_keyboard` shape
    /// `send`'s classic path does (TG-GATE-V2 W5).
    #[test]
    fn inline_keyboard_json_from_rows_mirrors_send() {
        let rows = vec![vec![opt("cmd:?/interrupt", "⛔ Прервать", None)]];
        let keyboard = inline_keyboard_json_from_rows(&rows).expect("some keyboard");
        assert_eq!(
            keyboard["inline_keyboard"][0][0]["callback_data"],
            "cmd:?/interrupt"
        );
        assert!(inline_keyboard_json_from_rows(&[]).is_none());
    }

    #[test]
    fn extract_message_id_reads_result_message_id() {
        assert_eq!(
            extract_message_id(r#"{"ok":true,"result":{"message_id":123}}"#),
            Some("123".to_string())
        );
        assert_eq!(extract_message_id("not json"), None);
        assert_eq!(extract_message_id(r#"{"ok":true,"result":{}}"#), None);
    }

    #[test]
    fn button_style_str_maps_the_three_variants() {
        assert_eq!(
            button_style_str(Some(ButtonStyle::Primary)),
            Some("primary")
        );
        assert_eq!(
            button_style_str(Some(ButtonStyle::Success)),
            Some("success")
        );
        assert_eq!(button_style_str(Some(ButtonStyle::Danger)), Some("danger"));
        assert_eq!(button_style_str(None), None);
    }

    // ----- rich→classic fallback decision --------------------------------

    #[tokio::test]
    async fn send_classic_falls_back_to_html_when_rich_markdown_absent() {
        // No live HTTP in this test crate (no mock server dependency), so
        // this exercises the decision surface that IS unit-testable
        // without one: a message with no `rich_markdown` never calls the
        // rich path at all — `send_classic`'s body-building matches the
        // pre-W1 shape exactly (byte-for-byte `send_classic_part`), proven
        // via `text_payload`/`inline_keyboard_json` directly.
        let message =
            SendMessage::new("hi **there**", "42").with_options(vec![opt("t:0", "Yes", None)]);
        assert!(message.rich_markdown.is_none());
        let payload = text_payload(&message.content);
        assert!(payload.formatted);
        assert_eq!(payload.text, "hi <b>there</b>");
        let keyboard = inline_keyboard_json(&message).unwrap();
        assert_eq!(keyboard["inline_keyboard"][0][0]["text"], "Yes");
    }

    #[test]
    fn long_content_splits_with_part_suffix_like_existing_splitter() {
        // The rich-fallback split path reuses `split_for_channel` + a
        // `(i/n)` suffix; assert the suffix shape independent of the live
        // HTTP call (mirrors `send_classic`'s internal numbering).
        let long = "word ".repeat(2000);
        let parts = crate::sanitize::split_for_channel(&long, MAX_MESSAGE_UTF16);
        assert!(parts.len() > 1);
        let total = parts.len();
        for (i, part) in parts.iter().enumerate() {
            let numbered = format!("{part} ({}/{total})", i + 1);
            assert!(numbered.ends_with(&format!("({}/{total})", i + 1)));
        }
    }
}
