use ccteam_harness::execution::progress_bridge::{
    append_event, append_turn_verdict_if_changed, latest_turn_verdicts, read_progress_checkpoint,
    TurnVerdict, Verdict,
};
use serde_json::json;

#[test]
fn latest_verdict_survives_three_archive_replacements_and_stays_idempotent() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "1024");
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("demo.jsonl");
    let accepted = TurnVerdict {
        sid: "s1".into(),
        turn_id: "sj-1".into(),
        ts: chrono::Utc::now(),
        verdict: Verdict::Accept,
        feedback: None,
    };
    assert!(append_turn_verdict_if_changed(&active, &accepted).unwrap());

    let mut sequence = 0_u64;
    while read_progress_checkpoint(&active)
        .unwrap()
        .map(|checkpoint| checkpoint.rotation_sequence)
        .unwrap_or_default()
        < 3
    {
        append_event(
            &active,
            &json!({
                "event": "verdict_rotation_fixture",
                "seq": sequence,
                "padding": "x".repeat(280),
            }),
        )
        .unwrap();
        sequence += 1;
        assert!(sequence < 100, "tiny threshold must force three rotations");
    }

    let latest = latest_turn_verdicts(&active).unwrap();
    assert_eq!(
        latest.get(&("s1".into(), "sj-1".into())),
        Some(&accepted),
        "the checkpoint retains verdict state after `.1` was replaced twice"
    );

    let duplicate = TurnVerdict {
        ts: chrono::Utc::now() + chrono::Duration::seconds(30),
        ..accepted
    };
    assert!(
        !append_turn_verdict_if_changed(&active, &duplicate).unwrap(),
        "an identical PUT remains idempotent after repeated rotation"
    );
}
