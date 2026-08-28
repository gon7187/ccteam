use ccteam_harness::execution::progress_bridge::{
    append_chat_turn_completed_if_absent, append_event, append_turn_verdict_if_changed,
    latest_turn_verdicts, latest_turn_verdicts_for_turns_detailed,
    load_or_recover_progress_checkpoint, progress_archive_coverage, progress_archive_path,
    progress_checkpoint_path, progress_corrupt_line_count, progress_terminal_projection_path,
    progress_verdict_index_path, progress_verdict_projection_path, read_progress_checkpoint,
    terminal_turns_for_rebuild, TurnVerdict, Verdict,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::process::{Command, Stdio};

const LARGE_PROJECTION_TURNS: usize = 10_000;

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
fn corrupt_row_moves_from_active_cursor_to_checkpoint_exactly_once_on_rotation() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "1024");
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("corrupt-rotation.jsonl");
    std::fs::write(&active, b"corrupt-active-row\n").unwrap();

    append_event(
        &active,
        &json!({
            "event": "force_rotation",
            "padding": "x".repeat(1200),
        }),
    )
    .unwrap();

    assert_eq!(
        read_progress_checkpoint(&active)
            .unwrap()
            .unwrap()
            .corrupt_line_count,
        1
    );
    assert_eq!(progress_corrupt_line_count(&active).unwrap(), 1);
    assert_eq!(progress_corrupt_line_count(&active).unwrap(), 1);
}

#[test]
fn same_bytes_in_a_new_archive_generation_are_folded_again() {
    std::env::set_var("CCTEAM_PROGRESS_ROTATE_BYTES", "1024");
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("same-bytes.jsonl");
    append_event(
        &active,
        &json!({"event": "generation", "padding": "x".repeat(1200)}),
    )
    .unwrap();
    let first = read_progress_checkpoint(&active).unwrap().unwrap();
    let archive = progress_archive_path(&active);
    let replacement = temp.path().join("replacement.jsonl");
    std::fs::write(&replacement, std::fs::read(&archive).unwrap()).unwrap();
    std::fs::rename(&replacement, &archive).unwrap();

    let recovered = load_or_recover_progress_checkpoint(&active)
        .unwrap()
        .unwrap();

    assert_eq!(recovered.rotation_sequence, first.rotation_sequence + 1);
    assert_eq!(recovered.event_count, first.event_count * 2);
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
    assert_eq!(checkpoint.schema_version, 4);
    assert!(checkpoint.turn_verdicts.is_empty());
    assert!(checkpoint.terminal_turns.is_empty());
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
fn v2_legacy_archive_marker_upgrades_without_double_folding() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("legacy-v2.jsonl");
    let archive = progress_archive_path(&active);
    let line = format!(
        "{}\n",
        json!({
            "event": "agent_done",
            "sid": "s1",
            "vendor": "claude",
            "cost_usd": 7.0,
        })
    );
    std::fs::write(&archive, line.as_bytes()).unwrap();
    let first_line_sha256 = format!("{:x}", Sha256::digest(line.as_bytes()));
    std::fs::write(
        progress_checkpoint_path(&active),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "rotation_sequence": 1,
            "event_count": 1,
            "corrupt_line_count": 0,
            "cost_total_usd": 7.0,
            "cost_total_by_vendor": {"claude": 7.0},
            "cost_total_by_sid": {"s1": 7.0},
            "turn_verdicts": {},
            "terminal_turns": {},
            "coverage": {
                "byte_size": line.len(),
                "first_line_sha256": first_line_sha256,
            },
        }))
        .unwrap(),
    )
    .unwrap();

    let upgraded = load_or_recover_progress_checkpoint(&active)
        .unwrap()
        .unwrap();

    assert_eq!(upgraded.schema_version, 4);
    assert!(upgraded.turn_verdicts.is_empty());
    assert!(upgraded.terminal_turns.is_empty());
    assert_eq!(upgraded.rotation_sequence, 1);
    assert_eq!(upgraded.event_count, 1);
    assert_eq!(upgraded.cost_total_usd, 7.0);
    assert!(upgraded
        .coverage
        .as_ref()
        .and_then(|coverage| coverage.full_file_sha256.as_deref())
        .is_some());
    let raw_checkpoint: serde_json::Value =
        serde_json::from_slice(&std::fs::read(progress_checkpoint_path(&active)).unwrap()).unwrap();
    assert!(raw_checkpoint.get("turn_verdicts").is_none());
    assert!(raw_checkpoint.get("terminal_turns").is_none());
}

#[test]
fn v3_checkpoint_and_index_upgrade_preserves_projections_without_double_folding() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("legacy-v3.jsonl");
    let archive = progress_archive_path(&active);
    let terminal = json!({
        "event": "chat_turn_completed",
        "sid": "s1",
        "turn_id": "turn-1",
        "ts": "2026-08-28T00:00:00Z",
        "outcome": "failed",
    });
    let stale_terminal = json!({
        "event": "chat_turn_completed",
        "sid": "s1",
        "turn_id": "turn-1",
        "ts": "2026-08-28T01:00:00Z",
        "outcome": "completed",
    });
    let accepted = TurnVerdict {
        sid: "s1".into(),
        turn_id: "turn-1".into(),
        ts: "2026-08-28T00:00:01Z".parse().unwrap(),
        verdict: Verdict::Accept,
        feedback: None,
    };
    let revised = TurnVerdict {
        ts: "2026-08-28T01:00:01Z".parse().unwrap(),
        verdict: Verdict::Revise,
        feedback: Some("new evidence".into()),
        ..accepted.clone()
    };
    let mut accepted_event = serde_json::to_value(&accepted).unwrap();
    accepted_event["event"] = json!("turn_verdict");
    let mut revised_event = serde_json::to_value(&revised).unwrap();
    revised_event["event"] = json!("turn_verdict");
    let archive_rows = [
        json!({
            "event": "agent_done",
            "sid": "s1",
            "vendor": "claude",
            "cost_usd": 7.0,
        }),
        terminal.clone(),
        accepted_event,
    ];
    std::fs::write(
        &archive,
        archive_rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    std::fs::write(&active, format!("{stale_terminal}\n{revised_event}\n")).unwrap();
    let coverage = progress_archive_coverage(&active).unwrap().unwrap();
    std::fs::write(
        progress_checkpoint_path(&active),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 3,
            "rotation_sequence": 1,
            "event_count": archive_rows.len(),
            "corrupt_line_count": 0,
            "cost_total_usd": 7.0,
            "cost_total_by_vendor": {"claude": 7.0},
            "cost_total_by_sid": {"s1": 7.0},
            "turn_verdicts": {"s1": {"turn-1": accepted}},
            "terminal_turns": {"s1": {"turn-1": terminal}},
            "coverage": coverage,
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        progress_verdict_index_path(&active),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 3,
            "verdicts": {"s1": {"turn-1": accepted}},
            "terminal_turns": {"s1": {"turn-1": terminal}},
        }))
        .unwrap(),
    )
    .unwrap();

    let upgraded = load_or_recover_progress_checkpoint(&active)
        .unwrap()
        .unwrap();

    assert_eq!(upgraded.schema_version, 4);
    assert_eq!(upgraded.rotation_sequence, 1);
    assert_eq!(upgraded.event_count, archive_rows.len() as u64);
    assert_eq!(upgraded.cost_total_usd, 7.0);
    assert!(upgraded.turn_verdicts.is_empty());
    assert!(upgraded.terminal_turns.is_empty());
    assert_eq!(
        latest_turn_verdicts(&active)
            .unwrap()
            .get(&("s1".into(), "turn-1".into())),
        Some(&revised)
    );
    let replay = append_chat_turn_completed_if_absent(&active, &stale_terminal).unwrap();
    assert!(!replay.appended);
    assert_eq!(replay.event["outcome"], "failed");
    for state_path in [
        progress_checkpoint_path(&active),
        progress_verdict_index_path(&active),
    ] {
        let raw = std::fs::read(&state_path).unwrap();
        assert!(raw.len() < 64 * 1024);
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert!(value.get("turn_verdicts").is_none());
        assert!(value.get("terminal_turns").is_none());
        assert!(value.get("verdicts").is_none());
    }
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
fn corrupt_current_verdict_index_with_a_projection_fails_closed() {
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
    let progress_before = std::fs::read(&active).unwrap();
    let projection_path = progress_verdict_projection_path(&active);
    let projection_before = std::fs::read(&projection_path).unwrap();
    std::fs::write(
        progress_checkpoint_path(&active),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 4,
            "rotation_sequence": 0,
            "event_count": 0,
            "corrupt_line_count": 0,
            "cost_total_usd": 0.0,
            "cost_total_by_vendor": {},
            "cost_total_by_sid": {},
            "coverage": null,
        }))
        .unwrap(),
    )
    .unwrap();

    std::fs::write(progress_verdict_index_path(&active), b"{truncated").unwrap();

    let error = latest_turn_verdicts(&active).unwrap_err().to_string();
    assert!(error.contains("verdict projection coverage mismatch"));
    assert_eq!(std::fs::read(&active).unwrap(), progress_before);
    assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
    assert!(load_or_recover_progress_checkpoint(&active).is_err());
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

#[test]
fn special_projection_keeps_hot_state_bounded_at_ten_thousand_unique_turns() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("bounded-specials.jsonl");
    let mut progress = std::io::BufWriter::new(std::fs::File::create(&active).unwrap());
    for turn in 0..LARGE_PROJECTION_TURNS {
        writeln!(
            progress,
            "{}",
            json!({
                "event": "chat_turn_completed",
                "sid": "s1",
                "turn_id": format!("turn-{turn}"),
                "ts": "2026-08-28T00:00:00Z",
                "vendor": "claude",
                "outcome": "completed",
                "payload": "x".repeat(256),
            })
        )
        .unwrap();
        writeln!(
            progress,
            "{}",
            json!({
                "event": "turn_verdict",
                "sid": "s1",
                "turn_id": format!("turn-{turn}"),
                "ts": "2026-08-28T00:00:01Z",
                "verdict": "accept",
            })
        )
        .unwrap();
    }
    progress.flush().unwrap();

    load_or_recover_progress_checkpoint(&active).unwrap();

    assert!(
        std::fs::metadata(progress_checkpoint_path(&active))
            .map(|meta| meta.len())
            .unwrap_or(0)
            < 64 * 1024,
        "lifetime checkpoint must not retain terminal/verdict payloads"
    );
    assert!(
        std::fs::metadata(progress_verdict_index_path(&active))
            .map(|meta| meta.len())
            .unwrap_or(0)
            < 64 * 1024,
        "the crash fence/cursor must stay bounded"
    );
    assert!(progress_terminal_projection_path(&active).exists());
    assert!(progress_verdict_projection_path(&active).exists());

    let requested = (0..100).map(|turn| format!("turn-{turn}")).collect();
    let before = ccteam_harness::execution::journal::metrics();
    let first = latest_turn_verdicts_for_turns_detailed(&active, "s1", &requested).unwrap();
    for _ in 0..20 {
        assert_eq!(
            latest_turn_verdicts_for_turns_detailed(&active, "s1", &requested).unwrap(),
            first
        );
    }
    let bytes_read = ccteam_harness::execution::journal::metrics()
        .bytes_read
        .saturating_sub(before.bytes_read);
    assert_eq!(first.verdicts.len(), requested.len());
    assert!(
        bytes_read < 10 * 1024 * 1024,
        "repeated page-scoped verdict reads consumed {bytes_read} bytes"
    );

    let child_metrics = temp.path().join("cold-child-bytes");
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("cold_process_receipt_lookup_is_bounded")
        .arg("--nocapture")
        .env("CCTEAM_RECEIPT_CHILD_PROGRESS", &active)
        .env("CCTEAM_RECEIPT_CHILD_METRICS", &child_metrics)
        .env("CCTEAM_PROGRESS_ROTATE_BYTES", "67108864")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "cold receipt child failed:\n{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let child_bytes: u64 = std::fs::read_to_string(child_metrics)
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        child_bytes < 64 * 1024,
        "a cold one-shot writer read {child_bytes} bytes for two keys"
    );
}

#[test]
fn cold_page_lookup_skips_a_large_uncovered_ordinary_delta() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("cold-page.jsonl");
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "turn-1".into(),
        ts: "2026-08-28T00:00:00Z".parse().unwrap(),
        verdict: Verdict::Accept,
        feedback: None,
    };
    assert!(append_turn_verdict_if_changed(&active, &verdict).unwrap());
    let mut ordinary = std::fs::OpenOptions::new()
        .append(true)
        .open(&active)
        .unwrap();
    writeln!(
        ordinary,
        "{}",
        json!({"event": "ordinary_large_delta", "payload": "x".repeat(12 * 1024 * 1024)})
    )
    .unwrap();
    drop(ordinary);
    let requested = ["turn-1".to_string()].into_iter().collect();

    let before = ccteam_harness::execution::journal::metrics();
    let read = latest_turn_verdicts_for_turns_detailed(&active, "s1", &requested).unwrap();
    let bytes_read = ccteam_harness::execution::journal::metrics()
        .bytes_read
        .saturating_sub(before.bytes_read);

    assert_eq!(
        read.verdicts.get(&("s1".into(), "turn-1".into())),
        Some(&verdict)
    );
    assert!(
        bytes_read < 64 * 1024,
        "bounded cold page read consumed {bytes_read} bytes"
    );
}

#[test]
fn oversized_legacy_sidecars_are_rejected_before_cold_page_read() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("oversized-v3.jsonl");
    std::fs::write(&active, b"{\"event\":\"ordinary\"}\n").unwrap();
    let huge = "x".repeat(2 * 1024 * 1024);
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "turn-1".into(),
        ts: "2026-08-28T00:00:00Z".parse().unwrap(),
        verdict: Verdict::Accept,
        feedback: Some(huge.clone()),
    };
    std::fs::write(
        progress_verdict_index_path(&active),
        serde_json::to_vec(&json!({
            "schema_version": 3,
            "verdicts": {"s1": {"turn-1": verdict}},
            "terminal_turns": {},
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        progress_checkpoint_path(&active),
        serde_json::to_vec(&json!({
            "schema_version": 3,
            "rotation_sequence": 1,
            "event_count": 1,
            "corrupt_line_count": 0,
            "cost_total_usd": 0.0,
            "cost_total_by_vendor": {},
            "cost_total_by_sid": {},
            "turn_verdicts": {},
            "terminal_turns": {"s1": {"turn-1": {"payload": huge}}},
            "coverage": null,
        }))
        .unwrap(),
    )
    .unwrap();
    let requested = ["turn-1".to_string()].into_iter().collect();

    let before = ccteam_harness::execution::journal::metrics();
    let error = latest_turn_verdicts_for_turns_detailed(&active, "s1", &requested)
        .unwrap_err()
        .to_string();
    let bytes_read = ccteam_harness::execution::journal::metrics()
        .bytes_read
        .saturating_sub(before.bytes_read);

    assert!(error.contains("progress verdict index exceeds bounded lookup limit"));
    assert!(
        bytes_read < 64 * 1024,
        "oversized legacy sidecars consumed {bytes_read} bytes"
    );
}

#[test]
fn global_verdict_scan_rejects_an_uncovered_oversized_projection_record() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("oversized-verdict-projection.jsonl");
    std::fs::write(
        progress_verdict_projection_path(&active),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "source_id": "corrupt",
            "verdict": {
                "sid": "s1",
                "turn_id": "turn-1",
                "ts": "2026-08-28T00:00:00Z",
                "verdict": "accept",
                "feedback": "x".repeat(128 * 1024),
            },
        }))
        .unwrap(),
    )
    .unwrap();

    let error = latest_turn_verdicts(&active).unwrap_err().to_string();

    assert!(error
        .contains("verdict bootstrap projection contains bytes not proven by retained progress"));
}

#[test]
fn terminal_rebuild_rejects_an_uncovered_oversized_projection_record() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("oversized-terminal-projection.jsonl");
    std::fs::write(
        progress_terminal_projection_path(&active),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "source_id": "corrupt",
            "event": {
                "event": "chat_turn_completed",
                "sid": "s1",
                "turn_id": "turn-1",
                "ts": "2026-08-28T00:00:00Z",
                "payload": "x".repeat(128 * 1024),
            },
        }))
        .unwrap(),
    )
    .unwrap();

    let error = terminal_turns_for_rebuild(&active).unwrap_err().to_string();

    assert!(error
        .contains("terminal bootstrap projection contains bytes not proven by retained progress"));
}

#[test]
fn legacy_verdict_receipt_size_failure_does_not_grow_projection_on_retry() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("oversized-legacy-verdict-receipt.jsonl");
    let turn_id = "t".repeat(40 * 1024);
    std::fs::write(
        &active,
        format!(
            "{}\n",
            json!({
                "event": "turn_verdict",
                "sid": "s1",
                "turn_id": turn_id,
                "ts": "2026-08-28T00:00:00Z",
                "verdict": "accept",
            })
        ),
    )
    .unwrap();
    let projection = progress_verdict_projection_path(&active);

    let first_error = load_or_recover_progress_checkpoint(&active)
        .unwrap_err()
        .to_string();
    let first_len = std::fs::metadata(&projection).unwrap().len();
    let second_error = load_or_recover_progress_checkpoint(&active)
        .unwrap_err()
        .to_string();
    let second_len = std::fs::metadata(&projection).unwrap().len();

    assert!(first_error.contains("verdict receipt exceeds storage limit"));
    assert!(second_error.contains("verdict receipt exceeds storage limit"));
    assert_eq!(
        first_len, 0,
        "an invalid receipt must block projection append"
    );
    assert_eq!(second_len, first_len, "retry must not append an orphan row");
}

#[test]
fn projection_coverage_rejects_a_valid_truncated_verdict_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("truncated-verdict-prefix.jsonl");
    for turn_id in ["turn-1", "turn-2"] {
        assert!(append_turn_verdict_if_changed(
            &active,
            &TurnVerdict {
                sid: "s1".into(),
                turn_id: turn_id.into(),
                ts: "2026-08-28T00:00:00Z".parse().unwrap(),
                verdict: Verdict::Accept,
                feedback: None,
            },
        )
        .unwrap());
    }
    let projection = progress_verdict_projection_path(&active);
    let complete = std::fs::read(&projection).unwrap();
    let first_end = complete.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&projection)
        .unwrap()
        .set_len(first_end as u64)
        .unwrap();
    let truncated = std::fs::read(&projection).unwrap();

    let error = latest_turn_verdicts(&active).unwrap_err().to_string();

    assert!(error.contains("verdict projection coverage mismatch"));
    assert_eq!(std::fs::read(&projection).unwrap(), truncated);
}

#[test]
fn projection_coverage_rejects_a_valid_truncated_terminal_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("truncated-terminal-prefix.jsonl");
    for turn_id in ["turn-1", "turn-2"] {
        assert!(
            append_chat_turn_completed_if_absent(
                &active,
                &json!({
                    "event": "chat_turn_completed",
                    "sid": "s1",
                    "turn_id": turn_id,
                    "ts": "2026-08-28T00:00:00Z",
                    "outcome": "completed",
                }),
            )
            .unwrap()
            .appended
        );
    }
    let projection = progress_terminal_projection_path(&active);
    let complete = std::fs::read(&projection).unwrap();
    let first_end = complete.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&projection)
        .unwrap()
        .set_len(first_end as u64)
        .unwrap();
    let truncated = std::fs::read(&projection).unwrap();

    let error = terminal_turns_for_rebuild(&active).unwrap_err().to_string();

    assert!(error.contains("terminal projection coverage mismatch"));
    assert_eq!(std::fs::read(&projection).unwrap(), truncated);
}

#[cfg(unix)]
#[test]
fn projection_coverage_rejects_a_same_length_verdict_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("replaced-verdict-projection.jsonl");
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "turn-1".into(),
        ts: "2026-08-28T00:00:00Z".parse().unwrap(),
        verdict: Verdict::Accept,
        feedback: None,
    };
    assert!(append_turn_verdict_if_changed(&active, &verdict).unwrap());
    let projection = progress_verdict_projection_path(&active);
    let replacement = temp.path().join("replacement.jsonl");
    std::fs::write(&replacement, std::fs::read(&projection).unwrap()).unwrap();
    std::fs::rename(&replacement, &projection).unwrap();

    let error = latest_turn_verdicts(&active).unwrap_err().to_string();

    assert!(error.contains("verdict projection coverage mismatch"));
}

#[test]
fn projection_coverage_rejects_a_valid_extra_verdict_suffix() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("extra-verdict-projection.jsonl");
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "turn-1".into(),
        ts: "2026-08-28T00:00:00Z".parse().unwrap(),
        verdict: Verdict::Accept,
        feedback: None,
    };
    assert!(append_turn_verdict_if_changed(&active, &verdict).unwrap());
    let projection = progress_verdict_projection_path(&active);
    let line = std::fs::read(&projection).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&projection)
        .unwrap()
        .write_all(&line)
        .unwrap();

    let error = latest_turn_verdicts(&active).unwrap_err().to_string();

    assert!(error.contains("verdict projection coverage mismatch"));
}

#[cfg(unix)]
#[test]
fn terminal_first_wins_across_concurrent_processes() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("multiprocess-terminal.jsonl");
    let barrier = temp.path().join("go");
    let mut children = Vec::new();
    for worker in 0..8 {
        let result = temp.path().join(format!("result-{worker}.json"));
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("cross_process_terminal_race_child")
            .arg("--nocapture")
            .env("CCTEAM_TERMINAL_RACE_PROGRESS", &active)
            .env("CCTEAM_TERMINAL_RACE_BARRIER", &barrier)
            .env("CCTEAM_TERMINAL_RACE_RESULT", &result)
            .env("CCTEAM_TERMINAL_RACE_WORKER", worker.to_string())
            .env("CCTEAM_PROGRESS_ROTATE_BYTES", "67108864")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        children.push((child, result));
    }
    std::fs::write(&barrier, b"go").unwrap();

    let mut results = Vec::new();
    for (child, result) in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "terminal race child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        results.push(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(result).unwrap()).unwrap(),
        );
    }

    assert_eq!(
        results
            .iter()
            .filter(|result| result["appended"] == true)
            .count(),
        1
    );
    let canonical_outcomes = results
        .iter()
        .map(|result| result["outcome"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(canonical_outcomes.len(), 1);
    assert_eq!(std::fs::read_to_string(&active).unwrap().lines().count(), 1);
    assert_eq!(
        std::fs::read(progress_terminal_projection_path(&active))
            .unwrap()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
        1
    );
    let replay = append_chat_turn_completed_if_absent(
        &active,
        &json!({
            "event": "chat_turn_completed",
            "sid": "s1",
            "turn_id": "turn-race",
            "ts": "2026-08-28T02:00:00Z",
            "outcome": "parent-replay",
        }),
    )
    .unwrap();
    assert!(!replay.appended);
    assert_eq!(
        replay.event["outcome"].as_str().unwrap(),
        *canonical_outcomes.first().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn cross_process_terminal_race_child() {
    let Ok(progress) = std::env::var("CCTEAM_TERMINAL_RACE_PROGRESS") else {
        return;
    };
    let barrier = std::path::PathBuf::from(std::env::var("CCTEAM_TERMINAL_RACE_BARRIER").unwrap());
    let result = std::path::PathBuf::from(std::env::var("CCTEAM_TERMINAL_RACE_RESULT").unwrap());
    let worker = std::env::var("CCTEAM_TERMINAL_RACE_WORKER").unwrap();
    let started = std::time::Instant::now();
    while !barrier.exists() {
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let admitted = append_chat_turn_completed_if_absent(
        std::path::Path::new(&progress),
        &json!({
            "event": "chat_turn_completed",
            "sid": "s1",
            "turn_id": "turn-race",
            "ts": "2026-08-28T01:00:00Z",
            "outcome": format!("worker-{worker}"),
        }),
    )
    .unwrap();
    std::fs::write(
        result,
        serde_json::to_vec(&json!({
            "appended": admitted.appended,
            "outcome": admitted.event["outcome"],
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn cold_process_receipt_lookup_is_bounded() {
    let Ok(progress) = std::env::var("CCTEAM_RECEIPT_CHILD_PROGRESS") else {
        return;
    };
    let metrics = std::env::var("CCTEAM_RECEIPT_CHILD_METRICS").unwrap();
    let progress = std::path::PathBuf::from(progress);
    let before = ccteam_harness::execution::journal::metrics();
    let replay = json!({
        "event": "chat_turn_completed",
        "sid": "s1",
        "turn_id": "turn-1",
        "ts": "2026-08-28T09:00:00Z",
        "vendor": "claude",
        "outcome": "failed",
    });
    let admitted = append_chat_turn_completed_if_absent(&progress, &replay).unwrap();
    assert!(!admitted.appended);
    assert_eq!(admitted.event["outcome"], "completed");
    let fresh = json!({
        "event": "chat_turn_completed",
        "sid": "s1",
        "turn_id": "turn-cold-new",
        "ts": "2026-08-28T09:00:00Z",
        "vendor": "claude",
        "outcome": "completed",
    });
    assert!(
        append_chat_turn_completed_if_absent(&progress, &fresh)
            .unwrap()
            .appended
    );
    let verdict = TurnVerdict {
        sid: "s1".into(),
        turn_id: "turn-1".into(),
        ts: "2026-08-28T09:00:01Z".parse().unwrap(),
        verdict: Verdict::Accept,
        feedback: None,
    };
    assert!(!append_turn_verdict_if_changed(&progress, &verdict).unwrap());
    let bytes = ccteam_harness::execution::journal::metrics()
        .bytes_read
        .saturating_sub(before.bytes_read);
    std::fs::write(metrics, bytes.to_string()).unwrap();
}
