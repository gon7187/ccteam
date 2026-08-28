use ccteam_harness::execution::progress_bridge::{
    append_event, append_turn_verdict_if_changed, latest_turn_verdicts,
    load_or_recover_progress_checkpoint, progress_archive_coverage, progress_archive_path,
    progress_checkpoint_path, progress_verdict_index_path, read_progress_checkpoint, TurnVerdict,
    Verdict,
};
use serde_json::json;
use std::io::Write;

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

#[test]
fn v1_checkpoint_is_backfilled_before_its_covered_archive_is_replaced() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "1024");
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("legacy.jsonl");
    let archive = progress_archive_path(&active);
    let verdict = TurnVerdict {
        sid: "s-legacy".into(),
        turn_id: "turn-before-upgrade".into(),
        ts: chrono::Utc::now(),
        verdict: Verdict::Revise,
        feedback: Some("preserve me".into()),
    };
    let mut event = serde_json::to_value(&verdict).unwrap();
    event["event"] = json!("turn_verdict");
    std::fs::write(
        &archive,
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();
    let coverage = progress_archive_coverage(&active).unwrap().unwrap();
    std::fs::write(
        progress_checkpoint_path(&active),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "rotation_sequence": 1,
            "event_count": 1,
            "corrupt_line_count": 0,
            "cost_total_usd": 0.0,
            "cost_total_by_vendor": {},
            "cost_total_by_sid": {},
            "coverage": coverage,
        }))
        .unwrap(),
    )
    .unwrap();

    // This append rotates immediately. Migration must fold the old `.1`
    // before rotation replaces it, without double-counting its aggregates.
    std::fs::write(&active, b"{\"event\":\"filler\",\"padding\":\"").unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&active)
        .unwrap();
    file.write_all("x".repeat(1200).as_bytes()).unwrap();
    file.write_all(b"\"}\n").unwrap();
    drop(file);
    append_event(&active, &json!({"event": "rotate_now"})).unwrap();

    let checkpoint = read_progress_checkpoint(&active).unwrap().unwrap();
    assert_eq!(checkpoint.schema_version, 2);
    assert_eq!(
        checkpoint.event_count, 3,
        "v1 aggregate must not be folded twice"
    );
    assert_eq!(
        latest_turn_verdicts(&active)
            .unwrap()
            .get(&(verdict.sid.clone(), verdict.turn_id.clone(),)),
        Some(&verdict)
    );
}

#[test]
fn covered_archive_and_cold_active_verdict_lookup_stay_below_read_budget() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("perf.jsonl");
    let archive = progress_archive_path(&active);
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "t1".into(),
        ts: chrono::Utc::now(),
        verdict: Verdict::Accept,
        feedback: None,
    };

    // A covered 12 MiB archive must never be reread by a verdict GET.
    let mut archive_file = std::io::BufWriter::new(std::fs::File::create(&archive).unwrap());
    for seq in 0..42_000_u64 {
        writeln!(
            archive_file,
            "{}",
            json!({"event":"filler","seq":seq,"padding":"x".repeat(260)})
        )
        .unwrap();
    }
    archive_file.flush().unwrap();
    assert!(std::fs::metadata(&archive).unwrap().len() > 10 * 1024 * 1024);
    let coverage = progress_archive_coverage(&active).unwrap().unwrap();
    std::fs::write(
        progress_checkpoint_path(&active),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "rotation_sequence": 1,
            "event_count": 42000,
            "corrupt_line_count": 0,
            "cost_total_usd": 0.0,
            "cost_total_by_vendor": {},
            "cost_total_by_sid": {},
            "turn_verdicts": {"s1": {"t1": verdict}},
            "terminal_turns": {},
            "coverage": coverage,
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(&active, b"").unwrap();

    // Startup migration materializes the tiny projection outside requests.
    load_or_recover_progress_checkpoint(&active).unwrap();
    assert!(progress_verdict_index_path(&active).exists());
    let before = ccteam_harness::execution::journal::metrics();
    let latest = latest_turn_verdicts(&active).unwrap();
    let after = ccteam_harness::execution::journal::metrics();
    assert_eq!(latest.get(&("s1".into(), "t1".into())), Some(&verdict));
    assert!(
        after.bytes_read - before.bytes_read < 10 * 1024 * 1024,
        "cold verdict GET reread {} bytes",
        after.bytes_read - before.bytes_read
    );

    let before_put = ccteam_harness::execution::journal::metrics();
    assert!(!append_turn_verdict_if_changed(&active, &verdict).unwrap());
    let after_put = ccteam_harness::execution::journal::metrics();
    assert!(
        after_put.bytes_read - before_put.bytes_read < 10 * 1024 * 1024,
        "idempotent verdict PUT reread {} bytes",
        after_put.bytes_read - before_put.bytes_read
    );
}

#[test]
fn corrupt_derived_verdict_index_rebuilds_from_progress_authority() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("corrupt-index.jsonl");
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "t1".into(),
        ts: chrono::Utc::now(),
        verdict: Verdict::Revise,
        feedback: Some("keep the journal fact".into()),
    };
    assert!(append_turn_verdict_if_changed(&active, &verdict).unwrap());

    std::fs::write(progress_verdict_index_path(&active), b"{truncated").unwrap();

    assert_eq!(
        latest_turn_verdicts(&active)
            .unwrap()
            .get(&(verdict.sid.clone(), verdict.turn_id.clone())),
        Some(&verdict),
        "a direct GET must rebuild the derived index from progress.jsonl"
    );

    std::fs::write(progress_verdict_index_path(&active), b"{truncated-again").unwrap();
    load_or_recover_progress_checkpoint(&active).unwrap();
    assert_eq!(
        latest_turn_verdicts(&active)
            .unwrap()
            .get(&(verdict.sid.clone(), verdict.turn_id.clone())),
        Some(&verdict),
        "startup hydration must rebuild the same derived index"
    );

    std::fs::write(
        progress_verdict_index_path(&active),
        serde_json::to_vec(&json!({"schema_version": 999, "verdicts": {}})).unwrap(),
    )
    .unwrap();
    assert!(
        latest_turn_verdicts(&active).is_err(),
        "an older binary must not overwrite a valid future index schema"
    );
}

#[test]
fn generic_append_routes_canonical_verdicts_through_the_index() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("generic-verdict.jsonl");
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "t1".into(),
        ts: chrono::Utc::now(),
        verdict: Verdict::Accept,
        feedback: None,
    };
    let mut event = serde_json::to_value(&verdict).unwrap();
    event["event"] = json!("turn_verdict");

    append_event(&active, &event).unwrap();

    assert_eq!(
        latest_turn_verdicts(&active)
            .unwrap()
            .get(&(verdict.sid.clone(), verdict.turn_id.clone())),
        Some(&verdict),
        "the public generic writer must not bypass the canonical verdict projection"
    );
    assert!(
        append_event(&active, &json!({"event": "turn_verdict", "sid": "s1"})).is_err(),
        "a malformed canonical verdict must fail closed"
    );
}
