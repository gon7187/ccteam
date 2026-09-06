//! Vendor subscription-quota probe (claude / codex / kimi usage APIs): the
//! process-lifetime, per-vendor 60-second cache shared by the REST route
//! `GET /api/v1/vendors/quota` (ccteam-web) and the MCP `status` panel.
//! Read-only credential files, no OAuth refresh; every failure is an
//! `unavailable` row. Pure parsers live in `ccteam_core::vendor_quota`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ccteam_core::host_registry::AgentProbeSpec;
use ccteam_core::vendor_quota::{
    claude_oauth_from_credentials, codex_chatgpt_from_auth, kimi_managed_token_from_credentials,
    parse_claude_usage, parse_codex_wham_usage, parse_kimi_usages, probed_specs, QuotaProbeKind,
    QuotaVerdict, VendorQuota,
};

/// Bounded reuse for repeated status calls; a snapshot retains its actual
/// observation time so a commander cannot mistake cached usage for live use.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Per-request budget for one vendor's usage API.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The OAuth-subscriber beta flag the claude codebase sends on
/// `/api/oauth/usage` (`OAUTH_BETA_HEADER` in `src/constants/oauth.ts`).
const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
const CODEX_WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const KIMI_USAGES_URL: &str = "https://api.kimi.com/coding/v1/usages";

/// Process-lifetime, per-vendor TTL cache (mirrors `probe_bin_cached`'s
/// shape: a Mutex map keyed by vendor token).
#[derive(Debug)]
struct QuotaCache {
    ttl: Duration,
    map: Mutex<BTreeMap<&'static str, (Instant, VendorQuota)>>,
}

impl QuotaCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            map: Mutex::new(BTreeMap::new()),
        }
    }

    /// The cached row when it is younger than the TTL.
    fn fresh(&self, vendor: &str) -> Option<VendorQuota> {
        let map = self.map.lock().ok()?;
        let (at, quota) = map.get(vendor)?;
        if at.elapsed() < self.ttl {
            Some(quota.clone())
        } else {
            None
        }
    }

    fn store(&self, vendor: &'static str, quota: VendorQuota) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(vendor, (Instant::now(), quota));
        }
    }
}

/// The quota probe service held on `AppState`.
#[derive(Debug)]
pub struct VendorQuotaService {
    client: reqwest::Client,
    cache: QuotaCache,
}

impl Default for VendorQuotaService {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            cache: QuotaCache::new(CACHE_TTL),
        }
    }
}

impl VendorQuotaService {
    /// Every probe-bearing vendor's quota row, registry-ordered. Cached rows
    /// serve from memory; misses probe concurrently and fill the cache.
    pub async fn quotas(&self) -> Vec<VendorQuota> {
        let mut by_vendor: BTreeMap<&'static str, VendorQuota> = BTreeMap::new();
        let mut misses = Vec::new();
        for (spec, kind) in probed_specs() {
            if let Some(hit) = self.cache.fresh(spec.vendor) {
                by_vendor.insert(spec.vendor, hit);
            } else {
                misses.push((spec, kind));
            }
        }
        let probed = futures::future::join_all(misses.iter().map(|(spec, kind)| {
            let client = &self.client;
            async move {
                let mut quota = probe_one(client, spec, *kind).await;
                quota.observed_at = Some(chrono::Utc::now());
                quota.source = Some(
                    match kind {
                        QuotaProbeKind::ClaudeOauthUsage => "claude_oauth_usage",
                        QuotaProbeKind::CodexWhamUsage => "codex_wham_usage",
                        QuotaProbeKind::KimiManagedUsages => "kimi_managed_usages",
                        QuotaProbeKind::GrokBillingUnavailable => "unavailable",
                    }
                    .to_string(),
                );
                (spec.vendor, quota)
            }
        }))
        .await;
        for (vendor, quota) in probed {
            self.cache.store(vendor, quota.clone());
            by_vendor.insert(vendor, quota);
        }
        // Emit in REGISTRY order (the probe axis), not alphabetical.
        probed_specs()
            .filter_map(|(spec, _)| by_vendor.remove(spec.vendor))
            .collect()
    }
}

// ── credential files (read-only) ────────────────────────────────────────────

/// `$CLAUDE_CONFIG_HOME/.credentials.json` (default `~/.claude/…`).
fn claude_credentials_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    Some(dir.join(".credentials.json"))
}

/// `$CODEX_HOME/auth.json` (default `~/.codex/…`) — mirrors
/// `mcp_register::resolve_codex_config_path`'s home resolution.
fn codex_auth_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    Some(dir.join("auth.json"))
}

/// `$KIMI_CODE_HOME/credentials/kimi-code.json` (default `~/.kimi-code/…`).
fn kimi_credentials_path() -> Option<PathBuf> {
    let dir = std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".kimi-code")))?;
    Some(dir.join("credentials").join("kimi-code.json"))
}

/// Failure to read subscription credentials proves nothing about the plan.
fn read_credential<T>(
    path: Option<PathBuf>,
    extract: impl FnOnce(&str) -> Option<T>,
) -> Result<T, String> {
    let path = path.ok_or("credentials_missing")?;
    let body = std::fs::read_to_string(path).map_err(|_| "credentials_unreadable")?;
    extract(&body).ok_or_else(|| "subscription_credentials_unavailable".to_string())
}

// ── HTTP layer (safe diagnostic codes, no response bodies) ─────────────────

fn claude_usage_request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Option<reqwest::Request> {
    client
        .get(url)
        .bearer_auth(token)
        .header("anthropic-beta", CLAUDE_OAUTH_BETA)
        .build()
        .ok()
}

fn codex_usage_request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    account_id: &str,
) -> Option<reqwest::Request> {
    client
        .get(url)
        .bearer_auth(token)
        .header("ChatGPT-Account-Id", account_id)
        .build()
        .ok()
}

fn kimi_usage_request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Option<reqwest::Request> {
    client.get(url).bearer_auth(token).build().ok()
}

/// Execute one probe request and parse the 2xx body. ANY transport or HTTP
/// failure has an explicit safe diagnostic. Unknown is never no subscription.
async fn execute_and_parse(
    client: &reqwest::Client,
    request: Option<reqwest::Request>,
    parse: impl FnOnce(&str) -> QuotaVerdict,
) -> Result<QuotaVerdict, String> {
    let resp = client
        .execute(request.ok_or("invalid_request")?)
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "request_timeout"
            } else {
                "network_error"
            }
        })?;
    if !resp.status().is_success() {
        return Err(format!("http_{}", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(|_| "unreadable_response")?;
    match parse(&body) {
        QuotaVerdict::Unavailable => Err("usage_unreadable".to_string()),
        verdict => Ok(verdict),
    }
}

fn unavailable(vendor: &str, reason: String) -> VendorQuota {
    let mut quota = VendorQuota::unavailable(vendor);
    quota.reason = Some(reason);
    quota
}

/// Probe one vendor. Never fails: every unhappy path is an `unavailable`
/// (or `not_subscription`) row.
async fn probe_one(
    client: &reqwest::Client,
    spec: &'static AgentProbeSpec,
    kind: QuotaProbeKind,
) -> VendorQuota {
    let vendor = spec.vendor;
    match kind {
        // The four-header billing coupling cannot be derived cleanly from
        // `~/.grok/` (refresh-keyed token, internal proxy base) — stubbed
        // unavailable by construction; see QuotaProbeKind's doc comment.
        QuotaProbeKind::GrokBillingUnavailable => {
            unavailable(vendor, "probe_unsupported".to_string())
        }
        QuotaProbeKind::ClaudeOauthUsage => {
            let (token, plan) =
                match read_credential(claude_credentials_path(), claude_oauth_from_credentials) {
                    Ok(auth) => auth,
                    Err(reason) => return unavailable(vendor, reason),
                };
            let request = claude_usage_request(client, CLAUDE_USAGE_URL, &token);
            let mut quota = execute_and_parse(client, request, |body| {
                parse_claude_usage(body, plan.clone())
            })
            .await
            .map(|verdict| verdict.into_quota(vendor))
            .unwrap_or_else(|reason| unavailable(vendor, reason));
            quota.plan = plan;
            quota
        }
        QuotaProbeKind::CodexWhamUsage => {
            let (token, account_id) =
                match read_credential(codex_auth_path(), codex_chatgpt_from_auth) {
                    Ok(auth) => auth,
                    Err(reason) => return unavailable(vendor, reason),
                };
            let request = codex_usage_request(client, CODEX_WHAM_USAGE_URL, &token, &account_id);
            execute_and_parse(client, request, parse_codex_wham_usage)
                .await
                .map(|verdict| verdict.into_quota(vendor))
                .unwrap_or_else(|reason| unavailable(vendor, reason))
        }
        QuotaProbeKind::KimiManagedUsages => {
            let token =
                match read_credential(kimi_credentials_path(), kimi_managed_token_from_credentials)
                {
                    Ok(auth) => auth,
                    Err(reason) => return unavailable(vendor, reason),
                };
            let request = kimi_usage_request(client, KIMI_USAGES_URL, &token);
            execute_and_parse(client, request, parse_kimi_usages)
                .await
                .map(|verdict| verdict.into_quota(vendor))
                .unwrap_or_else(|reason| unavailable(vendor, reason))
        }
    }
}

/// One service per process: both callers must share the cache, otherwise
/// each MCP `status` call would hit three vendor APIs.
pub fn global() -> &'static VendorQuotaService {
    static SERVICE: std::sync::OnceLock<VendorQuotaService> = std::sync::OnceLock::new();
    SERVICE.get_or_init(VendorQuotaService::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::vendor_quota::{QuotaState, QuotaWindowKind};

    // ── request builders (the thin wiring the parsers rely on) ───────────

    #[test]
    fn claude_request_carries_oauth_bearer_and_beta_header() {
        let client = reqwest::Client::new();
        let req = claude_usage_request(&client, "https://api.anthropic.com/api/oauth/usage", "tok")
            .unwrap();
        assert_eq!(req.method(), reqwest::Method::GET);
        assert_eq!(
            req.url().as_str(),
            "https://api.anthropic.com/api/oauth/usage"
        );
        assert_eq!(req.headers().get("authorization").unwrap(), "Bearer tok");
        assert_eq!(
            req.headers().get("anthropic-beta").unwrap(),
            "oauth-2025-04-20"
        );
    }

    #[test]
    fn codex_request_carries_bearer_and_account_id() {
        let client = reqwest::Client::new();
        let req = codex_usage_request(
            &client,
            "https://chatgpt.com/backend-api/wham/usage",
            "tok",
            "acc-1",
        )
        .unwrap();
        assert_eq!(req.headers().get("authorization").unwrap(), "Bearer tok");
        assert_eq!(req.headers().get("chatgpt-account-id").unwrap(), "acc-1");
    }

    // ── TTL cache ────────────────────────────────────────────────────────

    #[test]
    fn cache_serves_within_ttl_and_misses_past_it() {
        let cache = QuotaCache::new(Duration::from_secs(300));
        assert!(cache.fresh("claude").is_none());
        cache.store("claude", VendorQuota::unavailable("claude"));
        assert_eq!(
            cache.fresh("claude").unwrap().state,
            QuotaState::Unavailable
        );
        // A zero TTL never hits — every call re-probes.
        let zero = QuotaCache::new(Duration::ZERO);
        zero.store("codex", VendorQuota::not_subscription("codex"));
        assert!(zero.fresh("codex").is_none());
    }

    // ── execute_and_parse against a local mock (no real network) ─────────

    async fn spawn_mock(
        status: axum::http::StatusCode,
        body: &'static str,
    ) -> std::net::SocketAddr {
        let app = axum::Router::new().route(
            "/usage",
            axum::routing::get(move || async move { (status, body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        addr
    }

    #[tokio::test]
    async fn http_401_maps_to_unavailable() {
        let addr = spawn_mock(axum::http::StatusCode::UNAUTHORIZED, "{\"error\":\"nope\"}").await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{addr}/usage");
        let req = claude_usage_request(&client, &url, "tok");
        let verdict = execute_and_parse(&client, req, |body| parse_claude_usage(body, None)).await;
        assert_eq!(verdict.unwrap_err(), "http_401");
    }

    #[tokio::test]
    async fn timeout_maps_to_unavailable() {
        let app = axum::Router::new().route(
            "/usage",
            axum::routing::get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "never"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let url = format!("http://{addr}/usage");
        let req = kimi_usage_request(&client, &url, "tok");
        let verdict = execute_and_parse(&client, req, parse_kimi_usages).await;
        assert_eq!(verdict.unwrap_err(), "request_timeout");
    }

    #[tokio::test]
    async fn ok_body_parses_through_the_fixture_path() {
        let body = r#"{"five_hour":{"utilization":42.0,"resets_at":"2026-08-17T18:00:00Z"}}"#;
        let addr = spawn_mock(axum::http::StatusCode::OK, body).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{addr}/usage");
        let req = claude_usage_request(&client, &url, "tok");
        let verdict = execute_and_parse(&client, req, |b| parse_claude_usage(b, None))
            .await
            .unwrap();
        let QuotaVerdict::Available { windows, .. } = verdict else {
            panic!("expected available");
        };
        assert_eq!(windows[0].kind, QuotaWindowKind::FiveHour);
        assert_eq!(windows[0].used_percent, 42.0);
    }
}
