//! Integration coverage for the read-only Evolution analytics projection.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
use ccteam_harness::execution::experience::{
    append_experience, ExperienceRecord, TurnExperience, TurnSignals, VerdictExperience,
};
use ccteam_harness::execution::progress_bridge::{
    progress_archive_path, TurnVerdict, Verdict, TURN_VERDICT,
};
use ccteam_web::{router_with_state, AppState, AuthState};
use serde_json::Value;
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn seed_project(paths: &CcteamPaths, slug: &str) {
    let state_path = paths.project_state(slug);
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut state = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
    state.owner = Some("user:web-api".into());
    state.save(&state_path).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn turn(
    sid: &str,
    turn_id: &str,
    ts: chrono::DateTime<chrono::Utc>,
    role: &str,
    role_sha: Option<&str>,
    skills_sha: &[(&str, &str)],
    cost_usd: Option<f64>,
    outcome: Option<&str>,
    duration_ms: Option<u64>,
) -> ExperienceRecord {
    ExperienceRecord::Turn(TurnExperience {
        sid: sid.into(),
        turn_id: turn_id.into(),
        ts,
        vendor: "claude".into(),
        model: None,
        role: role.into(),
        usage: None,
        cost_usd,
        outcome: outcome.map(str::to_owned),
        duration_ms,
        role_sha: role_sha.map(str::to_owned),
        skills_sha: (!skills_sha.is_empty()).then(|| {
            skills_sha
                .iter()
                .map(|(id, sha)| ((*id).to_owned(), (*sha).to_owned()))
                .collect::<BTreeMap<_, _>>()
        }),
        signals: TurnSignals {
            tool_calls: 0,
            steered: false,
            error_recovered: None,
        },
    })
}

async fn fetch_evolution(addr: SocketAddr, slug: &str) -> reqwest::Response {
    client()
        .get(format!("http://{addr}/api/v1/projects/{slug}/evolution"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
}

fn verdict_event(verdict: &TurnVerdict) -> Value {
    let mut value = serde_json::to_value(verdict).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("event".into(), Value::String(TURN_VERDICT.into()));
    value
}

fn write_jsonl(path: &std::path::Path, rows: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{body}\n")).unwrap();
}

#[tokio::test]
async fn evolution_reports_7day_turn_trend() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "alpha");
    let dir = paths.project_dir("alpha");

    append_experience(
        &dir,
        &turn(
            "s1",
            "recent",
            chrono::Utc::now(),
            "cto",
            Some("abc123abc123"),
            &[],
            None,
            None,
            None,
        ),
    )
    .unwrap();
    append_experience(
        &dir,
        &turn(
            "s1",
            "old",
            chrono::Utc::now() - chrono::Duration::days(30),
            "cto",
            Some("abc123abc123"),
            &[],
            None,
            None,
            None,
        ),
    )
    .unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let body: Value = fetch_evolution(addr, "alpha")
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turn_records"], 2);
    assert_eq!(
        body["turn_records_7d"], 1,
        "only the recent turn counts: {body}"
    );
    assert_eq!(body["unrated_turns"], 2);
    assert_eq!(body["outcome_unknown_turns"], 2);
    assert_eq!(body["unpriced_turns"], 2);
    assert!(body["avg_duration_ms"].is_null());
    assert_eq!(body["empty"], false);
}

#[tokio::test]
async fn evolution_joins_latest_canonical_verdicts_and_keeps_unknowns_honest() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "alpha");
    let dir = paths.project_dir("alpha");
    let now = chrono::Utc::now();

    for record in [
        turn(
            "s1",
            "accepted",
            now,
            "cto",
            Some("role-a"),
            &[("research", "skill-a")],
            Some(2.0),
            Some("completed"),
            Some(100),
        ),
        turn(
            "s1",
            "revised",
            now,
            "cto",
            Some("role-a"),
            &[("research", "skill-a"), ("ux", "skill-u")],
            None,
            Some("failed"),
            Some(300),
        ),
        turn(
            "s2",
            "unrated-priced-zero",
            now,
            "worker",
            Some("role-b"),
            &[("research", "skill-b")],
            Some(0.0),
            Some("cancelled"),
            None,
        ),
        turn(
            "s3",
            "roleless-unknown",
            now,
            "",
            None,
            &[],
            None,
            None,
            Some(500),
        ),
        // The projection can contain a stale verdict row; canonical progress
        // must win and derived rows must not inflate verdict metrics.
        ExperienceRecord::Verdict(VerdictExperience {
            sid: "s1".into(),
            turn_id: "revised".into(),
            ts: now,
            verdict: Verdict::Accept,
            feedback: Some("stale projection".into()),
        }),
    ] {
        append_experience(&dir, &record).unwrap();
    }

    let accepted = TurnVerdict {
        sid: "s1".into(),
        turn_id: "accepted".into(),
        ts: now,
        verdict: Verdict::Accept,
        feedback: None,
    };
    let stale_revised = TurnVerdict {
        sid: "s1".into(),
        turn_id: "revised".into(),
        ts: now,
        verdict: Verdict::Accept,
        feedback: None,
    };
    let revised = TurnVerdict {
        sid: "s1".into(),
        turn_id: "revised".into(),
        ts: now + chrono::Duration::seconds(1),
        verdict: Verdict::Revise,
        feedback: Some("fix the edge case".into()),
    };
    let progress = paths.progress_jsonl("alpha");
    write_jsonl(
        &progress_archive_path(&progress),
        &[verdict_event(&stale_revised)],
    );
    write_jsonl(
        &progress,
        &[verdict_event(&accepted), verdict_event(&revised)],
    );

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let body: Value = fetch_evolution(addr, "alpha")
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["turn_records"], 4, "derived verdict row is not a turn");
    assert_eq!(body["verdict_records"], 2, "latest canonical facts only");
    assert_eq!(body["accepted_turns"], 1);
    assert_eq!(body["revised_turns"], 1);
    assert_eq!(body["unrated_turns"], 2);
    assert_eq!(body["completed_turns"], 1);
    assert_eq!(body["failed_turns"], 1);
    assert_eq!(body["outcome_unknown_turns"], 2);
    assert_eq!(body["priced_turns"], 2, "known zero is still priced");
    assert_eq!(body["unpriced_turns"], 2);
    assert_eq!(body["avg_duration_ms"], 300.0);
    assert_eq!(body["skill_attribution"], "available_at_spawn");

    let roles = body["roles"].as_array().unwrap();
    let cto = roles.iter().find(|row| row["id"] == "cto").unwrap();
    assert_eq!(cto["turn_count"], 2);
    assert_eq!(cto["accepted_turns"], 1);
    assert_eq!(cto["revised_turns"], 1);
    assert_eq!(cto["unrated_turns"], 0);
    assert_eq!(cto["completed_turns"], 1);
    assert_eq!(cto["failed_turns"], 1);
    assert_eq!(cto["outcome_unknown_turns"], 0);
    assert_eq!(cto["priced_turns"], 1);
    assert_eq!(cto["unpriced_turns"], 1);
    assert_eq!(cto["avg_duration_ms"], 200.0);
    assert_eq!(cto["avg_cost_usd"], 2.0);
    assert_eq!(cto["total_cost_usd"], 2.0);

    let worker = roles.iter().find(|row| row["id"] == "worker").unwrap();
    assert_eq!(worker["priced_turns"], 1);
    assert_eq!(worker["unpriced_turns"], 0);
    assert_eq!(worker["avg_cost_usd"], 0.0);
    assert_eq!(worker["total_cost_usd"], 0.0);
    assert!(worker["avg_duration_ms"].is_null());

    let skills = body["skills"].as_array().unwrap();
    let research_a = skills
        .iter()
        .find(|row| row["id"] == "research" && row["sha"] == "skill-a")
        .unwrap();
    assert_eq!(research_a["turn_count"], 2);
    assert_eq!(research_a["accepted_turns"], 1);
    assert_eq!(research_a["revised_turns"], 1);
    assert_eq!(research_a["avg_duration_ms"], 200.0);
}

#[tokio::test]
async fn evolution_returns_500_for_experience_or_progress_read_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "experience-broken");
    seed_project(&paths, "progress-broken");

    let experience_path = paths
        .project_dir("experience-broken")
        .join(".ccteam/experience.jsonl");
    std::fs::create_dir_all(&experience_path).unwrap();

    let progress_dir = paths.project_dir("progress-broken");
    append_experience(
        &progress_dir,
        &turn(
            "s1",
            "t1",
            chrono::Utc::now(),
            "cto",
            Some("role-a"),
            &[],
            None,
            None,
            None,
        ),
    )
    .unwrap();
    std::fs::create_dir_all(paths.progress_jsonl("progress-broken")).unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    for slug in ["experience-broken", "progress-broken"] {
        let response = fetch_evolution(addr, slug).await;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "{slug} storage failure must not masquerade as empty analytics"
        );
        let body: Value = response.json().await.unwrap();
        assert!(body["error"].is_string());
    }
}
