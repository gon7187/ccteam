//! `GET /api/v1/projects/{slug}/evolution` — read-only experience aggregate.
//!
//! Terminal-turn facts come from the rebuildable `experience.jsonl`
//! projection. Human verdicts are joined from canonical `progress.jsonl`
//! (`.1` archive first, active journal last), so a stale derived verdict can
//! never override the latest human decision.

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_harness::execution::experience::{
    read_all_experience, ExperienceRecord, TurnExperience,
};
use ccteam_harness::execution::progress_bridge::{latest_turn_verdicts, Verdict};
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

use super::sessions_api::project_not_visible;

const SKILL_ATTRIBUTION: &str = "available_at_spawn";

/// Per-role or per-skill fingerprint bucket.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvolutionBucket {
    /// `role` or `skill`.
    pub kind: String,
    /// Role name or skill id.
    pub id: String,
    /// Content digest (12-hex for roles; full skill map digest when present).
    pub sha: String,
    /// Number of turn records attributed to this fingerprint.
    pub turn_count: u64,
    pub accepted_turns: u64,
    pub revised_turns: u64,
    pub unrated_turns: u64,
    pub completed_turns: u64,
    pub failed_turns: u64,
    pub outcome_unknown_turns: u64,
    pub priced_turns: u64,
    pub unpriced_turns: u64,
    /// Mean duration across turns that reported duration (None if unknown).
    pub avg_duration_ms: Option<f64>,
    /// Mean cost USD across turns that reported cost (None if none priced).
    pub avg_cost_usd: Option<f64>,
    /// Sum of known costs.
    pub total_cost_usd: Option<f64>,
}

/// Project evolution summary (read-only).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvolutionSummary {
    pub slug: String,
    /// Total turn records in experience.jsonl.
    pub turn_records: u64,
    /// Latest canonical verdicts that match projected turns.
    pub verdict_records: u64,
    /// v0.8.24 — turn records written in the last 7 days (trend stat).
    pub turn_records_7d: u64,
    pub accepted_turns: u64,
    pub revised_turns: u64,
    pub unrated_turns: u64,
    pub completed_turns: u64,
    pub failed_turns: u64,
    pub outcome_unknown_turns: u64,
    pub priced_turns: u64,
    pub unpriced_turns: u64,
    /// Mean duration across turns that reported duration (None if unknown).
    pub avg_duration_ms: Option<f64>,
    pub roles: Vec<EvolutionBucket>,
    pub skills: Vec<EvolutionBucket>,
    /// Skills are availability fingerprints captured at session spawn; they
    /// do not claim that a particular skill was invoked during the turn.
    pub skill_attribution: String,
    /// True when the experience file is missing or empty.
    pub empty: bool,
}

/// `GET /api/v1/projects/{slug}/evolution`
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/evolution",
    tag = "projects",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Evolution summary (may be empty)", body = EvolutionSummary),
        (status = 403, description = "Project not visible"),
        (status = 404, description = "Unknown project"),
        (status = 500, description = "Evolution journal read failed"),
    ),
)]
pub(crate) async fn handle_evolution(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(slug): Path<String>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let project_dir = app.paths.project_dir(&slug);
    let records = match read_all_experience(&project_dir) {
        Ok(records) => records,
        Err(error) => {
            tracing::error!(%slug, %error, "evolution: read experience failed");
            return evolution_read_failed("experience");
        }
    };
    let verdicts = match latest_turn_verdicts(&app.paths.progress_jsonl(&slug)) {
        Ok(verdicts) => verdicts,
        Err(error) => {
            tracing::error!(%slug, %error, "evolution: read canonical verdicts failed");
            return evolution_read_failed("verdict journal");
        }
    };

    let mut turn_records = 0u64;
    let mut turn_records_7d = 0u64;
    let week_ago = chrono::Utc::now() - chrono::Duration::days(7);
    let mut turn_keys = BTreeSet::new();
    let mut summary_acc = Acc::default();
    // key = (kind, id, sha)
    let mut role_acc: BTreeMap<(String, String), Acc> = BTreeMap::new();
    let mut skill_acc: BTreeMap<(String, String), Acc> = BTreeMap::new();

    for rec in &records {
        match rec {
            ExperienceRecord::Turn(t) => {
                turn_records += 1;
                turn_keys.insert((t.sid.clone(), t.turn_id.clone()));
                if t.ts >= week_ago {
                    turn_records_7d += 1;
                }
                let verdict = verdicts
                    .get(&(t.sid.clone(), t.turn_id.clone()))
                    .map(|entry| entry.verdict);
                summary_acc.observe(t, verdict);
                accumulate_turn(t, verdict, &mut role_acc, &mut skill_acc);
            }
            // `experience.jsonl` is a derived index. Its verdict rows are
            // deliberately ignored; canonical progress facts above win.
            ExperienceRecord::Verdict(_) => {}
        }
    }

    let verdict_records = u64::try_from(
        turn_keys
            .iter()
            .filter(|key| verdicts.contains_key(*key))
            .count(),
    )
    .unwrap_or(u64::MAX);
    let roles = finish_buckets("role", role_acc);
    let skills = finish_buckets("skill", skill_acc);
    let empty = turn_records == 0 && verdict_records == 0;

    let summary = EvolutionSummary {
        slug,
        turn_records,
        verdict_records,
        turn_records_7d,
        accepted_turns: summary_acc.accepted,
        revised_turns: summary_acc.revised,
        unrated_turns: summary_acc.unrated(),
        completed_turns: summary_acc.completed,
        failed_turns: summary_acc.failed,
        outcome_unknown_turns: summary_acc.outcome_unknown(),
        priced_turns: summary_acc.priced,
        unpriced_turns: summary_acc.unpriced(),
        avg_duration_ms: summary_acc.avg_duration_ms(),
        roles,
        skills,
        skill_attribution: SKILL_ATTRIBUTION.to_string(),
        empty,
    };
    (StatusCode::OK, Json(summary)).into_response()
}

fn evolution_read_failed(source: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": format!("failed to read evolution {source}"),
        })),
    )
        .into_response()
}

#[derive(Default)]
struct Acc {
    turns: u64,
    accepted: u64,
    revised: u64,
    completed: u64,
    failed: u64,
    priced: u64,
    cost_sum: f64,
    cost_n: u64,
    duration_sum_ms: u128,
    duration_n: u64,
}

impl Acc {
    fn observe(&mut self, turn: &TurnExperience, verdict: Option<Verdict>) {
        self.turns += 1;
        match verdict {
            Some(Verdict::Accept) => self.accepted += 1,
            Some(Verdict::Revise) => self.revised += 1,
            None => {}
        }
        match turn.outcome.as_deref() {
            Some("completed") => self.completed += 1,
            Some("failed") => self.failed += 1,
            _ => {}
        }
        if let Some(cost) = turn.cost_usd {
            self.priced += 1;
            self.cost_sum += cost;
            self.cost_n += 1;
        }
        if let Some(duration_ms) = turn.duration_ms {
            self.duration_sum_ms += u128::from(duration_ms);
            self.duration_n += 1;
        }
    }

    fn unrated(&self) -> u64 {
        self.turns
            .saturating_sub(self.accepted.saturating_add(self.revised))
    }

    fn outcome_unknown(&self) -> u64 {
        self.turns
            .saturating_sub(self.completed.saturating_add(self.failed))
    }

    fn unpriced(&self) -> u64 {
        self.turns.saturating_sub(self.priced)
    }

    fn avg_duration_ms(&self) -> Option<f64> {
        (self.duration_n > 0).then(|| self.duration_sum_ms as f64 / self.duration_n as f64)
    }
}

fn accumulate_turn(
    t: &TurnExperience,
    verdict: Option<Verdict>,
    roles: &mut BTreeMap<(String, String), Acc>,
    skills: &mut BTreeMap<(String, String), Acc>,
) {
    if !t.role.is_empty() {
        let sha = t.role_sha.clone().unwrap_or_else(|| "unknown".into());
        let e = roles.entry((t.role.clone(), sha)).or_default();
        e.observe(t, verdict);
    }
    if let Some(map) = &t.skills_sha {
        for (id, sha) in map {
            let e = skills.entry((id.clone(), sha.clone())).or_default();
            e.observe(t, verdict);
        }
    }
}

fn finish_buckets(kind: &str, acc: BTreeMap<(String, String), Acc>) -> Vec<EvolutionBucket> {
    let mut out: Vec<EvolutionBucket> = acc
        .into_iter()
        .map(|((id, sha), a)| EvolutionBucket {
            kind: kind.to_string(),
            id,
            sha,
            turn_count: a.turns,
            accepted_turns: a.accepted,
            revised_turns: a.revised,
            unrated_turns: a.unrated(),
            completed_turns: a.completed,
            failed_turns: a.failed,
            outcome_unknown_turns: a.outcome_unknown(),
            priced_turns: a.priced,
            unpriced_turns: a.unpriced(),
            avg_duration_ms: a.avg_duration_ms(),
            avg_cost_usd: if a.cost_n > 0 {
                Some(a.cost_sum / a.cost_n as f64)
            } else {
                None
            },
            total_cost_usd: if a.cost_n > 0 { Some(a.cost_sum) } else { None },
        })
        .collect();
    out.sort_by(|a, b| b.turn_count.cmp(&a.turn_count).then(a.id.cmp(&b.id)));
    out
}
