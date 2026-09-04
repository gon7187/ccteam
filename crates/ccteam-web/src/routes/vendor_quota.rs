//! VENDOR-QUOTA-1 — `GET /api/v1/vendors/quota`: the local machine's vendor
//! subscription-quota snapshot for the Ops & Hosts page. Admin-only: it reads
//! the daemon user's vendor credential files. The probe + cache live in
//! `ccteam_im::vendor_quota_probe` (shared with the MCP `status` panel).

use axum::{
    response::{IntoResponse, Response},
    Extension, Json,
};

use crate::auth::{deny_non_admin, Identity};

/// `GET /api/v1/vendors/quota` — the local machine's per-vendor quota rows.
/// Vendors with no probe surface (opencode/pi/dsh) are absent from the list.
#[utoipa::path(
    get,
    path = "/api/v1/vendors/quota",
    tag = "hosts",
    responses(
        (status = 200, description = "Per-vendor quota rows `{quotas: [{vendor, state, plan?, windows?}]}`", body = serde_json::Value),
        (status = 403, description = "Not the admin/owner"),
    ),
)]
pub(crate) async fn handle_vendor_quota(Extension(identity): Extension<Identity>) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let quotas = ccteam_im::vendor_quota_probe::global().quotas().await;
    Json(serde_json::json!({ "quotas": quotas })).into_response()
}
