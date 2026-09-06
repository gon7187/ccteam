//! VENDOR-QUOTA-1 — the normalized vendor subscription-quota model plus the
//! PURE parsers (fixture-testable, zero I/O). The thin HTTP layer, per-vendor
//! credential file reads, and the 60-second cache live in
//! `ccteam_im::vendor_quota_probe`; the `GET /api/v1/vendors/quota` REST
//! handler in `ccteam_web::routes::vendor_quota` calls it.
//!
//! Same registry philosophy as [`crate::host_registry::AGENT_PROBE_SPECS`]:
//! a vendor's probe is one [`AgentProbeSpec::quota_probe`] entry, and the UI
//! renders purely off the normalized [`VendorQuota`] rows — a new probeable
//! vendor is one registry entry + one parser, zero UI change.
//!
//! Probe surfaces (verified against the vendor source references):
//! - claude: `GET https://api.anthropic.com/api/oauth/usage`, Bearer = OAuth
//!   `accessToken` from `~/.claude/.credentials.json` (`claudeAiOauth`,
//!   `user:profile` scope required) + `anthropic-beta: oauth-2025-04-20`.
//!   Unreadable/empty usage is unknown, never proof of no subscription.
//! - codex: `GET https://chatgpt.com/backend-api/wham/usage`, Bearer =
//!   `tokens.access_token` from `~/.codex/auth.json` + `ChatGPT-Account-Id:
//!   tokens.account_id`. Missing ChatGPT credentials leave usage unknown.
//! - kimi: `GET https://api.kimi.com/coding/v1/usages`, Bearer = managed
//!   OAuth token from `$KIMI_CODE_HOME/credentials/kimi-code.json`.
//!   Without managed credentials the probe cannot measure usage.
//! - grok: UNPROBED — see [`QuotaProbeKind::GrokBillingUnavailable`].
//! - opencode / pi / dsh: no surface (`quota_probe: None`; dsh's
//!   balance-only `/user/balance` is deliberately not a quota window).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::host_registry::AgentProbeSpec;

/// Which quota probe a vendor has. Implementations + credentials live in
/// `ccteam_im::vendor_quota_probe`; this enum is the registry token the
/// dispatch matches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaProbeKind {
    /// claude — `GET https://api.anthropic.com/api/oauth/usage`.
    ClaudeOauthUsage,
    /// codex — `GET https://chatgpt.com/backend-api/wham/usage`.
    CodexWhamUsage,
    /// kimi — `GET https://api.kimi.com/coding/v1/usages`.
    KimiManagedUsages,
    /// grok — the credits surface (`{cli-chat-proxy}/billing?format=credits`)
    /// needs a four-header coupling (`Authorization`, `X-XAI-Token-Auth`,
    /// `x-userid`, `x-grok-client-version`) whose values cannot be derived
    /// cleanly from `~/.grok/`: `auth.json` holds a *refresh* credential
    /// keyed by issuer, the proxy base is internal, and the x-userid /
    /// client-version pair is session-minted. Weekly-only anyway — the
    /// probe reports `unavailable` by construction rather than guessing.
    /// Do NOT fabricate a request here.
    GrokBillingUnavailable,
}

/// A window's duration, never its position in the vendor response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindowKind {
    FiveHour,
    Weekly,
    Monthly,
    Unknown,
}

/// One normalized quota window: how much of the window is consumed and when
/// it resets (unknown reset = `None`, never fabricated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub kind: QuotaWindowKind,
    /// 0–100, clamped on parse.
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    /// A model-specific pool. None means the subscription-wide pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Wire state of one vendor's quota probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaState {
    Available,
    /// The vendor has no subscription meter for this credential (API-key
    /// account). Missing or unreadable credentials are Unavailable.
    NotSubscription,
    /// The probe could not find out (network / 401 / timeout / shape
    /// drift) — readers must keep the remaining capacity unknown.
    Unavailable,
}

/// One vendor's normalized quota row — the `GET /api/v1/vendors/quota`
/// element shape. Flat + explicit (no serde tag/flatten tricks) so the SPA
/// type is trivial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorQuota {
    pub vendor: String,
    pub state: QuotaState,
    /// Plan label when the surface reports one (codex `plan_type`, claude
    /// `subscriptionType`); a small badge in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<QuotaWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Safe diagnostic code, never a raw HTTP body or credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl VendorQuota {
    pub fn available(vendor: &str, plan: Option<String>, windows: Vec<QuotaWindow>) -> Self {
        Self {
            vendor: vendor.to_string(),
            state: QuotaState::Available,
            plan,
            windows,
            observed_at: None,
            source: None,
            reason: None,
        }
    }

    pub fn not_subscription(vendor: &str) -> Self {
        Self {
            vendor: vendor.to_string(),
            state: QuotaState::NotSubscription,
            plan: None,
            windows: Vec::new(),
            observed_at: None,
            source: None,
            reason: None,
        }
    }

    pub fn unavailable(vendor: &str) -> Self {
        Self {
            vendor: vendor.to_string(),
            state: QuotaState::Unavailable,
            plan: None,
            windows: Vec::new(),
            observed_at: None,
            source: None,
            reason: None,
        }
    }
}

/// A successful HTTP response may still have an unreadable usage payload.
#[derive(Debug, Clone, PartialEq)]
pub enum QuotaVerdict {
    Available {
        plan: Option<String>,
        windows: Vec<QuotaWindow>,
    },
    NotSubscription,
    Unavailable,
}

impl QuotaVerdict {
    pub fn into_quota(self, vendor: &str) -> VendorQuota {
        match self {
            Self::Available { plan, windows } => VendorQuota::available(vendor, plan, windows),
            Self::NotSubscription => VendorQuota::not_subscription(vendor),
            Self::Unavailable => VendorQuota::unavailable(vendor),
        }
    }
}

// ── credential extractors (pure — the HTTP layer reads the files) ───────────

/// claude: the OAuth bearer + plan from `~/.claude/.credentials.json`.
/// Requires the `user:profile` scope (the usage endpoint's minimum, per the
/// claude codebase's own gate); an API-key-only file has no `claudeAiOauth`
/// block at all → `None`; this probe cannot measure its subscription.
pub fn claude_oauth_from_credentials(body: &str) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let token = oauth.get("accessToken")?.as_str()?;
    if token.is_empty() {
        return None;
    }
    let has_profile_scope = oauth
        .get("scopes")
        .and_then(|s| s.as_array())
        .is_some_and(|scopes| scopes.iter().any(|s| s.as_str() == Some("user:profile")));
    if !has_profile_scope {
        return None;
    }
    let plan = oauth
        .get("subscriptionType")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some((token.to_string(), plan))
}

/// codex: the ChatGPT bearer + account id from `~/.codex/auth.json`. ApiKey
/// auth has no `tokens` block → `None`; the subscription remains unknown.
pub fn codex_chatgpt_from_auth(body: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let tokens = v.get("tokens")?;
    let access = tokens.get("access_token")?.as_str()?;
    let account = tokens.get("account_id")?.as_str()?;
    if access.is_empty() || account.is_empty() {
        return None;
    }
    Some((access.to_string(), account.to_string()))
}

/// kimi: the managed OAuth bearer from `$KIMI_CODE_HOME/credentials/
/// kimi-code.json`. A machine using a plain API-key provider has no such
/// file → `None`; the subscription remains unknown.
pub fn kimi_managed_token_from_credentials(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let token = v.get("access_token")?.as_str()?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

// ── response parsers (pure — fixtures in tests) ─────────────────────────────

fn clamp_percent(raw: f64) -> f64 {
    raw.clamp(0.0, 100.0)
}

fn parse_iso(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_unix(raw: Option<i64>) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(raw?, 0)
}

fn window(
    kind: QuotaWindowKind,
    used_percent: f64,
    resets_at: Option<DateTime<Utc>>,
) -> QuotaWindow {
    QuotaWindow {
        kind,
        used_percent: clamp_percent(used_percent),
        resets_at,
        duration_seconds: match kind {
            QuotaWindowKind::FiveHour => Some(18_000),
            QuotaWindowKind::Weekly => Some(604_800),
            _ => None,
        },
        scope: None,
    }
}

/// claude `GET /api/oauth/usage`: `{five_hour?: {utilization, resets_at},
/// seven_day?: same, limits?: typed windows, …}`. Empty usage is unknown.
pub fn parse_claude_usage(body: &str, plan: Option<String>) -> QuotaVerdict {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return QuotaVerdict::Unavailable;
    };
    let mut windows = Vec::new();
    for (key, kind) in [
        ("five_hour", QuotaWindowKind::FiveHour),
        ("seven_day", QuotaWindowKind::Weekly),
    ] {
        let Some(rate) = v.get(key) else { continue };
        let Some(utilization) = rate
            .get("utilization")
            .and_then(|u| u.as_f64())
            .filter(|v| *v >= 0.0)
        else {
            continue;
        };
        let resets_at = parse_iso(rate.get("resets_at").and_then(|r| r.as_str()));
        windows.push(window(kind, utilization, resets_at));
    }
    // The typed list reports scoped pools (including Fable) that have no
    // stable top-level field. Common pools above remain single entries.
    for limit in v
        .get("limits")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let kind = match limit.get("group").and_then(|v| v.as_str()) {
            Some("session") => QuotaWindowKind::FiveHour,
            Some("weekly") => QuotaWindowKind::Weekly,
            _ => QuotaWindowKind::Unknown,
        };
        let Some(used) = limit
            .get("percent")
            .and_then(|v| v.as_f64())
            .filter(|v| *v >= 0.0)
        else {
            continue;
        };
        let scope = limit
            .pointer("/scope/model/display_name")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                limit
                    .pointer("/scope/model/id")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            })
            .map(str::to_string);
        // Unknown scoped limits must never masquerade as the general pool.
        let scoped = limit.get("kind").and_then(|v| v.as_str()) == Some("weekly_scoped");
        let scope = scope.or_else(|| scoped.then(|| "unknown_scope".to_string()));
        if windows.iter().any(|w| w.kind == kind && w.scope == scope) {
            continue;
        }
        let mut w = window(
            kind,
            used,
            parse_iso(limit.get("resets_at").and_then(|v| v.as_str())),
        );
        w.scope = scope;
        windows.push(w);
    }
    // Older payloads can expose additional model windows by name.
    if let Some(fields) = v.as_object() {
        for (key, rate) in fields {
            let Some(scope) = key.strip_prefix("seven_day_") else {
                continue;
            };
            let Some(used) = rate
                .get("utilization")
                .and_then(|v| v.as_f64())
                .filter(|v| *v >= 0.0)
            else {
                continue;
            };
            if windows.iter().any(|w| {
                w.scope
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(scope))
            }) {
                continue;
            }
            let mut w = window(
                QuotaWindowKind::Weekly,
                used,
                parse_iso(rate.get("resets_at").and_then(|v| v.as_str())),
            );
            w.scope = Some(scope.to_string());
            windows.push(w);
        }
    }
    if windows.is_empty() {
        return QuotaVerdict::Unavailable;
    }
    QuotaVerdict::Available { plan, windows }
}

/// codex `GET /backend-api/wham/usage`: `{plan_type, rate_limit:
/// {primary_window: {used_percent, reset_at(unix secs)}, secondary_window:
/// same, limit_window_seconds}}`. Window duration is supplied by the API;
/// the general primary window can be weekly with no secondary. Tolerates the
/// `{"rate_limits": {…}}` wrapper spelling too — the two shapes disagree
/// across codex releases and both parse.
pub fn parse_codex_wham_usage(body: &str) -> QuotaVerdict {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return QuotaVerdict::Unavailable;
    };
    let rate_limit = v
        .get("rate_limit")
        .or_else(|| v.pointer("/rate_limits/rate_limit"));
    let plan = v
        .get("plan_type")
        .or_else(|| v.pointer("/rate_limits/plan_type"))
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string());
    let mut windows = Vec::new();
    if let Some(rate_limit) = rate_limit {
        append_codex_windows(&mut windows, rate_limit, None);
    }
    for limit in v
        .get("additional_rate_limits")
        .or_else(|| v.pointer("/rate_limits/additional_rate_limits"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let scope = limit
            .get("limit_name")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                limit
                    .get("metered_feature")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or("unknown_scope")
            .to_string();
        if let Some(rate) = limit.get("rate_limit") {
            append_codex_windows(&mut windows, rate, Some(scope));
        }
    }
    if windows.is_empty() && plan.is_none() {
        return QuotaVerdict::Unavailable;
    }
    QuotaVerdict::Available { plan, windows }
}

fn append_codex_windows(
    windows: &mut Vec<QuotaWindow>,
    rate: &serde_json::Value,
    scope: Option<String>,
) {
    for key in ["primary_window", "secondary_window"] {
        let Some(raw) = rate.get(key) else { continue };
        let Some(used) = raw
            .get("used_percent")
            .and_then(|v| v.as_f64())
            .filter(|v| *v >= 0.0)
        else {
            continue;
        };
        let duration = raw
            .get("limit_window_seconds")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0);
        let kind = match duration {
            Some(18_000) => QuotaWindowKind::FiveHour,
            Some(604_800) => QuotaWindowKind::Weekly,
            _ => QuotaWindowKind::Unknown,
        };
        let mut w = window(
            kind,
            used,
            parse_unix(raw.get("reset_at").and_then(|v| v.as_i64())),
        );
        w.duration_seconds = duration;
        w.scope = scope.clone();
        windows.push(w);
    }
}

/// kimi `GET /coding/v1/usages`: `{usage: {used, limit, resetTime}, limits:
/// [{window: {duration: 300, timeUnit: "TIME_UNIT_MINUTE"}, detail: {used,
/// limit, resetTime}}]}`. Numbers may arrive as decimal strings. Top-level
/// `usage` = weekly; the 300-minute limit entry = 5h.
pub fn parse_kimi_usages(body: &str) -> QuotaVerdict {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return QuotaVerdict::Unavailable;
    };
    let mut windows = Vec::new();
    if let Some(usage) = v.get("usage") {
        if let Some(w) = kimi_ratio_window(QuotaWindowKind::Weekly, usage) {
            windows.push(w);
        }
    }
    if let Some(limits) = v.get("limits").and_then(|l| l.as_array()) {
        for entry in limits {
            let is_five_hour = entry
                .get("window")
                .map(|w| {
                    w.get("duration").and_then(|d| d.as_i64()) == Some(300)
                        && w.get("timeUnit").and_then(|t| t.as_str()) == Some("TIME_UNIT_MINUTE")
                })
                .unwrap_or(false);
            if !is_five_hour {
                continue;
            }
            if let Some(w) = entry
                .get("detail")
                .and_then(|d| kimi_ratio_window(QuotaWindowKind::FiveHour, d))
            {
                windows.push(w);
            }
        }
    }
    if windows.is_empty() {
        return QuotaVerdict::Unavailable;
    }
    QuotaVerdict::Available {
        plan: None,
        windows,
    }
}

/// kimi's `{used, limit, resetTime}` row → a percent window. `used`/`limit`
/// arrive as decimal strings (proto JSON); numbers are accepted too. A row
/// with missing fields, negative used, non-finite values, or non-positive
/// limit is unusable → `None` (the window drops; an all-dropped payload
/// stays unknown upstream, never misread as a full window).
fn kimi_ratio_window(kind: QuotaWindowKind, row: &serde_json::Value) -> Option<QuotaWindow> {
    let used = kimi_number(row.get("used"))?;
    let limit = kimi_number(row.get("limit"))?;
    if !used.is_finite() || !limit.is_finite() || used < 0.0 || limit <= 0.0 {
        return None;
    }
    let resets_at = parse_iso(row.get("resetTime").and_then(|r| r.as_str()));
    Some(window(kind, used / limit * 100.0, resets_at))
}

fn kimi_number(raw: Option<&serde_json::Value>) -> Option<f64> {
    match raw? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Every probe kind a vendor declares — the endpoint iterates this (via the
/// registry) so the response axis and the probe axis can never drift apart.
pub fn probed_specs() -> impl Iterator<Item = (&'static AgentProbeSpec, QuotaProbeKind)> {
    crate::host_registry::AGENT_PROBE_SPECS
        .iter()
        .filter_map(|spec| spec.quota_probe.map(|kind| (spec, kind)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── credential extractors ────────────────────────────────────────────

    #[test]
    fn claude_credentials_require_oauth_token_and_profile_scope() {
        let ok = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"r",
            "expiresAt":1893456000000,"scopes":["user:profile","user:inference"],
            "subscriptionType":"max"}}"#;
        let (token, plan) = claude_oauth_from_credentials(ok).unwrap();
        assert_eq!(token, "sk-ant-oat01-x");
        assert_eq!(plan.as_deref(), Some("max"));

        // API-key-only account: no claudeAiOauth block at all.
        assert!(claude_oauth_from_credentials(r#"{"installMethod":"native"}"#).is_none());
        // OAuth login without the usage endpoint's minimum scope.
        let no_scope = r#"{"claudeAiOauth":{"accessToken":"t","scopes":["user:inference"]}}"#;
        assert!(claude_oauth_from_credentials(no_scope).is_none());
        // Garbage / empty token.
        assert!(claude_oauth_from_credentials("not json").is_none());
        assert!(claude_oauth_from_credentials(r#"{"claudeAiOauth":{"accessToken":""}}"#).is_none());
    }

    #[test]
    fn codex_auth_requires_chatgpt_tokens() {
        let ok = r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"id","access_token":"atk",
            "refresh_token":"rtk","account_id":"acc-123"}}"#;
        assert_eq!(
            codex_chatgpt_from_auth(ok),
            Some(("atk".to_string(), "acc-123".to_string()))
        );
        // ApiKey auth: no tokens block → NotSubscription upstream.
        assert!(codex_chatgpt_from_auth(r#"{"OPENAI_API_KEY":"sk-x"}"#).is_none());
        // tokens without an account id cannot build the request.
        assert!(codex_chatgpt_from_auth(r#"{"tokens":{"access_token":"atk"}}"#).is_none());
        assert!(codex_chatgpt_from_auth("").is_none());
    }

    #[test]
    fn kimi_credentials_extract_the_managed_token() {
        let ok = r#"{"access_token":"kat","refresh_token":"krt","expires_at":1893456000,
            "scope":"openid","token_type":"Bearer","expires_in":3600}"#;
        assert_eq!(
            kimi_managed_token_from_credentials(ok).as_deref(),
            Some("kat")
        );
        assert!(kimi_managed_token_from_credentials(r#"{"refresh_token":"krt"}"#).is_none());
        assert!(kimi_managed_token_from_credentials("not json").is_none());
    }

    // ── claude usage parse ───────────────────────────────────────────────

    #[test]
    fn claude_usage_parses_both_windows() {
        let body = r#"{"five_hour":{"utilization":42.0,"resets_at":"2026-08-17T18:00:00Z"},
            "seven_day":{"utilization":15.5,"resets_at":"2026-08-20T00:00:00Z"},
            "seven_day_opus":{"utilization":3.0,"resets_at":"2026-08-20T00:00:00Z"}}"#;
        let QuotaVerdict::Available { plan, windows } =
            parse_claude_usage(body, Some("max".into()))
        else {
            panic!("expected available");
        };
        assert_eq!(plan.as_deref(), Some("max"));
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].kind, QuotaWindowKind::FiveHour);
        assert_eq!(windows[0].used_percent, 42.0);
        assert_eq!(
            windows[0].resets_at.unwrap().to_rfc3339(),
            "2026-08-17T18:00:00+00:00"
        );
        assert_eq!(windows[1].kind, QuotaWindowKind::Weekly);
        assert_eq!(windows[1].used_percent, 15.5);
    }

    #[test]
    fn claude_usage_without_meters_is_unknown() {
        assert_eq!(parse_claude_usage("{}", None), QuotaVerdict::Unavailable);
        // Null windows (subscriber with no active limits) also normalize away.
        assert_eq!(
            parse_claude_usage(r#"{"five_hour":null,"seven_day":null}"#, None),
            QuotaVerdict::Unavailable
        );
        assert_eq!(
            parse_claude_usage("not json", None),
            QuotaVerdict::Unavailable
        );
    }

    #[test]
    fn claude_usage_clamps_and_tolerates_missing_reset() {
        let body = r#"{"five_hour":{"utilization":137.0,"resets_at":null}}"#;
        let QuotaVerdict::Available { windows, .. } = parse_claude_usage(body, None) else {
            panic!("expected available");
        };
        assert_eq!(windows[0].used_percent, 100.0);
        assert_eq!(windows[0].resets_at, None);
    }

    // ── codex wham parse ─────────────────────────────────────────────────

    #[test]
    fn subscription_windows_keep_the_reported_duration_and_model_scope() {
        let codex = parse_codex_wham_usage(r#"{
            "plan_type":"pro","rate_limit":{
                "primary_window":{"used_percent":81,"limit_window_seconds":604800,"reset_at":1789106484},
                "secondary_window":null},
            "additional_rate_limits":[{"limit_name":"GPT-5.3-Codex-Spark","metered_feature":"codex_bengalfox",
                "rate_limit":{"primary_window":{"used_percent":0,"limit_window_seconds":18000},"secondary_window":null}}]
        }"#).into_quota("codex");
        assert_eq!(codex.windows[0].kind, QuotaWindowKind::Weekly);
        assert_eq!(codex.windows[0].used_percent, 81.0);
        assert_eq!(codex.windows.len(), 2);
        let json = serde_json::to_value(&codex).unwrap();
        assert_eq!(json["windows"][1]["scope"], "GPT-5.3-Codex-Spark");
        assert_eq!(codex.windows[1].kind, QuotaWindowKind::FiveHour);

        let claude = parse_claude_usage(r#"{
            "five_hour":{"utilization":6,"resets_at":"2026-09-06T13:40:00Z"},
            "seven_day":{"utilization":33,"resets_at":"2026-09-11T18:00:00Z"},
            "limits":[
                {"kind":"session","group":"session","percent":6},
                {"kind":"weekly_all","group":"weekly","percent":33},
                {"kind":"weekly_scoped","group":"weekly","percent":58,
                 "scope":{"model":{"id":null,"display_name":"Fable"}},"resets_at":"2026-09-11T18:00:00Z"}]
        }"#, Some("max".into())).into_quota("claude");
        assert_eq!(
            claude.windows.len(),
            3,
            "keep Fable without duplicating common windows"
        );
        let json = serde_json::to_value(&claude).unwrap();
        assert_eq!(json["windows"][2]["scope"], "Fable");
        assert_eq!(claude.windows[2].used_percent, 58.0);
    }

    #[test]
    fn unreadable_usage_is_unknown_not_a_non_subscriber() {
        for quota in [
            parse_codex_wham_usage("not json").into_quota("codex"),
            parse_claude_usage("not json", Some("max".into())).into_quota("claude"),
            parse_codex_wham_usage(r#"{"rate_limit":{"primary_window":{"used_percent":-1}}}"#)
                .into_quota("codex"),
            parse_claude_usage(r#"{"five_hour":{"utilization":-1}}"#, None).into_quota("claude"),
            parse_claude_usage(r#"{"limits":[{"group":"weekly","percent":-1}]}"#, None)
                .into_quota("claude"),
            parse_claude_usage(r#"{"seven_day_fable":{"utilization":-1}}"#, None)
                .into_quota("claude"),
        ] {
            assert_eq!(quota.state, QuotaState::Unavailable);
            assert!(quota.windows.is_empty());
        }
    }

    #[test]
    fn codex_wham_parses_primary_and_secondary_windows() {
        let body = r#"{"plan_type":"pro","rate_limit":{
            "primary_window":{"used_percent":42,"limit_window_seconds":18000,
                "reset_after_seconds":3600,"reset_at":1789000000},
            "secondary_window":{"used_percent":15,"limit_window_seconds":604800,
                "reset_after_seconds":300000,"reset_at":1789500000}}}"#;
        let QuotaVerdict::Available { plan, windows } = parse_codex_wham_usage(body) else {
            panic!("expected available");
        };
        assert_eq!(plan.as_deref(), Some("pro"));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].kind, QuotaWindowKind::FiveHour);
        assert_eq!(windows[0].used_percent, 42.0);
        assert_eq!(
            windows[0].resets_at.unwrap().timestamp(),
            1789000000,
            "reset_at is unix seconds"
        );
        assert_eq!(windows[1].kind, QuotaWindowKind::Weekly);
    }

    #[test]
    fn codex_wham_tolerates_the_rate_limits_wrapper_spelling() {
        let body = r#"{"rate_limits":{"plan_type":"team","rate_limit":{
            "primary_window":{"used_percent":7,"reset_at":1789000000}}}}"#;
        let QuotaVerdict::Available { plan, windows } = parse_codex_wham_usage(body) else {
            panic!("expected available");
        };
        assert_eq!(plan.as_deref(), Some("team"));
        assert_eq!(windows.len(), 1);
        assert_eq!(
            windows[0].kind,
            QuotaWindowKind::Unknown,
            "missing duration is not five hours"
        );
    }

    #[test]
    fn codex_wham_empty_payload_is_unknown() {
        assert_eq!(parse_codex_wham_usage("{}"), QuotaVerdict::Unavailable);
        assert_eq!(
            parse_codex_wham_usage("not json"),
            QuotaVerdict::Unavailable
        );
    }

    #[test]
    fn codex_wham_plan_only_still_renders_a_badge() {
        // A subscriber whose rate_limit block is null keeps the plan.
        let body = r#"{"plan_type":"go","rate_limit":null}"#;
        let QuotaVerdict::Available { plan, windows } = parse_codex_wham_usage(body) else {
            panic!("expected available");
        };
        assert_eq!(plan.as_deref(), Some("go"));
        assert!(windows.is_empty());
    }

    // ── kimi usages parse ────────────────────────────────────────────────

    #[test]
    fn kimi_usages_parse_weekly_summary_and_300_minute_window() {
        // Numbers as decimal strings (proto JSON), as the platform sends.
        let body = r#"{"usage":{"used":"40","limit":"1000","resetTime":"2026-08-20T05:20:51Z"},
            "limits":[
                {"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                 "detail":{"used":"1","limit":"100","resetTime":"2026-08-17T20:00:00Z"}},
                {"window":{"duration":1,"timeUnit":"TIME_UNIT_DAY"},
                 "detail":{"used":"5","limit":"50","resetTime":"2026-08-18T00:00:00Z"}}
            ]}"#;
        let QuotaVerdict::Available { plan, windows } = parse_kimi_usages(body) else {
            panic!("expected available");
        };
        assert_eq!(plan, None);
        assert_eq!(windows.len(), 2);
        // Top-level usage = weekly.
        assert_eq!(windows[0].kind, QuotaWindowKind::Weekly);
        assert_eq!(windows[0].used_percent, 4.0);
        // The 300-minute entry = 5h; the daily entry is dropped.
        assert_eq!(windows[1].kind, QuotaWindowKind::FiveHour);
        assert_eq!(windows[1].used_percent, 1.0);
    }

    #[test]
    fn kimi_usages_accept_numeric_too_and_reject_empty() {
        let body = r#"{"usage":{"used":1,"limit":4,"resetTime":"2026-08-20T00:00:00Z"}}"#;
        let QuotaVerdict::Available { windows, .. } = parse_kimi_usages(body) else {
            panic!("expected available");
        };
        assert_eq!(windows[0].used_percent, 25.0);
    }

    #[test]
    fn kimi_unusable_usage_is_unknown_never_no_subscription() {
        // Malformed JSON / empty usage: unknown, like claude/codex.
        assert_eq!(parse_kimi_usages("not json"), QuotaVerdict::Unavailable);
        assert_eq!(parse_kimi_usages("{}"), QuotaVerdict::Unavailable);
        assert_eq!(
            parse_kimi_usages(r#"{"usage":null,"limits":null}"#),
            QuotaVerdict::Unavailable
        );
        // Missing fields, negative used, non-finite values, non-positive
        // limit: every row drops, leaving unknown — never no subscription
        // or full capacity.
        for body in [
            r#"{"usage":{}}"#,
            r#"{"usage":{"used":"1","limit":"0"}}"#,
            r#"{"usage":{"used":"1","limit":"-5"}}"#,
            r#"{"usage":{"used":"-1","limit":"100"}}"#,
            r#"{"usage":{"used":"NaN","limit":"100"}}"#,
            r#"{"usage":{"used":"inf","limit":"100"}}"#,
            r#"{"usage":{"used":1,"limit":"NaN"}}"#,
        ] {
            assert_eq!(
                parse_kimi_usages(body),
                QuotaVerdict::Unavailable,
                "body: {body}"
            );
        }
        // A bad row drops without poisoning its valid siblings.
        let mixed = parse_kimi_usages(
            r#"{"usage":{"used":"40","limit":"1000"},
                "limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                "detail":{"used":"-1","limit":"100"}}]}"#,
        );
        let QuotaVerdict::Available { windows, .. } = mixed else {
            panic!("expected available");
        };
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].kind, QuotaWindowKind::Weekly);
    }

    // ── registry wiring ──────────────────────────────────────────────────

    #[test]
    fn quota_probe_registry_matches_the_shipped_stance() {
        use QuotaProbeKind::*;
        let probe = |vendor: &str| AgentProbeSpec::by_vendor(vendor).unwrap().quota_probe;
        assert_eq!(probe("claude"), Some(ClaudeOauthUsage));
        assert_eq!(probe("codex"), Some(CodexWhamUsage));
        assert_eq!(probe("kimi"), Some(KimiManagedUsages));
        assert_eq!(probe("grok"), Some(GrokBillingUnavailable));
        // No surface at all: opencode / pi / dsh (dsh balance is not a quota).
        assert_eq!(probe("opencode"), None);
        assert_eq!(probe("pi"), None);
        assert_eq!(probe("dsh"), None);
        // probed_specs covers exactly the Some entries, in registry order.
        let vendors: Vec<&str> = probed_specs().map(|(s, _)| s.vendor).collect();
        assert_eq!(vendors, vec!["claude", "codex", "grok", "kimi"]);
    }

    #[test]
    fn vendor_quota_wire_shape_is_flat_and_self_describing() {
        let q = VendorQuota::available(
            "claude",
            Some("max".into()),
            vec![window(QuotaWindowKind::FiveHour, 42.0, None)],
        );
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(v["vendor"], "claude");
        assert_eq!(v["state"], "available");
        assert_eq!(v["plan"], "max");
        assert_eq!(v["windows"][0]["kind"], "five_hour");
        // Roundtrip.
        let back: VendorQuota = serde_json::from_value(v).unwrap();
        assert_eq!(back, q);
        // Sparse states omit the optional fields entirely.
        let v = serde_json::to_value(VendorQuota::unavailable("grok")).unwrap();
        assert_eq!(v["state"], "unavailable");
        assert!(v.get("plan").is_none());
        assert!(v.get("windows").is_none());
    }
}
