use std::collections::BTreeMap;

use ccteam_cost::UnifiedTokenUsage;
use ccteam_harness::execution::experience::{
    experience_jsonl_path, read_all_experience, rebuild_experience, ExperienceRecord,
};
use ccteam_harness::execution::progress_bridge::{
    append_event, append_turn_verdict_if_changed, build_chat_turn_completed_event_with_metadata,
    read_progress_checkpoint, ChatTurnCompletionMetadata, TurnVerdict, Verdict,
};
use ccteam_harness::execution::turns_mirror::{append_turn, TurnRecord};
use serde_json::json;

#[test]
fn rebuild_after_repeated_rotation_uses_only_terminal_rows_and_preserves_projection() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "1024");
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let progress = project.join("progress.jsonl");
    let now = chrono::Utc::now();
    let usage = UnifiedTokenUsage {
        input_tokens: 120,
        output_tokens: 30,
        reported_cost_usd: Some(0.73),
        ..Default::default()
    };
    let base = |turn_id: &str, assistant: &str, outcome: Option<&str>| TurnRecord {
        turn_id: turn_id.into(),
        ts: now,
        vendor: "opencode".into(),
        role: "reviewer".into(),
        user: String::new(),
        assistant: assistant.into(),
        usage: serde_json::to_value(usage).unwrap(),
        tool_calls: vec![],
        attachments: vec![],
        outcome: outcome.map(str::to_string),
        error_kind: None,
        error: None,
    };
    append_turn(project, "s1", &base("user-row", "", None)).unwrap();
    append_turn(project, "s1", &base("temp-1", "draft one", Some("interim"))).unwrap();
    append_turn(project, "s1", &base("temp-2", "draft two", Some("interim"))).unwrap();
    append_turn(
        project,
        "s1",
        &base("turn-1", "final answer", Some("completed")),
    )
    .unwrap();

    append_event(
        &progress,
        &build_chat_turn_completed_event_with_metadata(
            "reviewer",
            "s1",
            "turn-1",
            &usage,
            Some("opencode-pro-2026"),
            Some("opencode"),
            &ChatTurnCompletionMetadata {
                outcome: Some("completed".into()),
                duration_ms: Some(987),
                role_sha: Some("role-sha-at-turn".into()),
                skills_sha: Some(BTreeMap::from([(
                    "audit".into(),
                    "skill-sha-at-turn".into(),
                )])),
                invoked_skills: None,
                signals: None,
            },
        ),
    )
    .unwrap();
    append_turn_verdict_if_changed(
        &progress,
        &TurnVerdict {
            sid: "s1".into(),
            turn_id: "turn-1".into(),
            ts: now,
            verdict: Verdict::Revise,
            feedback: Some("tighten it".into()),
        },
    )
    .unwrap();

    let mut sequence = 0_u64;
    while read_progress_checkpoint(&progress)
        .unwrap()
        .map(|checkpoint| checkpoint.rotation_sequence)
        .unwrap_or_default()
        < 3
    {
        append_event(
            &progress,
            &json!({
                "event": "rebuild_rotation_fixture",
                "seq": sequence,
                "padding": "x".repeat(280),
            }),
        )
        .unwrap();
        sequence += 1;
        assert!(sequence < 100);
    }

    let _ = std::fs::remove_file(experience_jsonl_path(project));
    assert_eq!(
        rebuild_experience(project, Some(&progress)).unwrap(),
        (1, 1)
    );
    let first_bytes = std::fs::read(experience_jsonl_path(project)).unwrap();
    let records = read_all_experience(project).unwrap();
    assert_eq!(records.len(), 2);
    match &records[0] {
        ExperienceRecord::Turn(turn) => {
            assert_eq!(turn.sid, "s1");
            assert_eq!(turn.turn_id, "turn-1");
            assert_eq!(turn.outcome.as_deref(), Some("completed"));
            assert_eq!(turn.cost_usd, Some(0.73));
            assert_eq!(turn.model.as_deref(), Some("opencode-pro-2026"));
            assert_eq!(turn.duration_ms, Some(987));
            assert_eq!(turn.role_sha.as_deref(), Some("role-sha-at-turn"));
            assert_eq!(
                turn.skills_sha
                    .as_ref()
                    .and_then(|skills| skills.get("audit"))
                    .map(String::as_str),
                Some("skill-sha-at-turn")
            );
        }
        other => panic!("expected terminal turn, got {other:?}"),
    }
    assert!(matches!(
        &records[1],
        ExperienceRecord::Verdict(verdict)
            if verdict.turn_id == "turn-1"
                && matches!(verdict.verdict, Verdict::Revise)
                && verdict.feedback.as_deref() == Some("tighten it")
    ));

    assert_eq!(
        rebuild_experience(project, Some(&progress)).unwrap(),
        (1, 1)
    );
    let second_bytes = std::fs::read(experience_jsonl_path(project)).unwrap();
    assert_eq!(second_bytes, first_bytes, "rebuild must be byte-idempotent");
}
