use super::*;
use std::os::unix::fs::symlink;

fn fixture(host: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let run = directory.path().join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(run.join("workspace/delivery")).unwrap();
    let mut state = json!({"status":"complete","finished":true,
            "workspace":{"original":"/private/project-secret"},"task":"PRIVATE_PROMPT",
            "workers":[{"checks":{"command":"SECRET_COMMAND","output":"SECRET_OUTPUT"}}]});
    let runtime = json!({"pid":2147483647,"started_seconds":0,"started_micros":0,"uid":unsafe{libc::getuid()}});
    state["runtime"] = runtime.clone();
    fs::write(
        run.join("watchdog.json"),
        json!({"runtime":runtime}).to_string(),
    )
    .unwrap();
    if host == "claude" {
        state["host"] = json!("claude");
    }
    fs::write(
        run.join(if host == "claude" {
            "claude.json"
        } else {
            "run.json"
        }),
        state.to_string(),
    )
    .unwrap();
    fs::write(run.join("workspace/delivery/result.json"), json!({"delivered":true,
            "cleanup_complete":true,"verification_required":false,"changed_paths":["source-secret.ts"],
            "conflicts":[],"project":"/private/project-secret","recovery":"/secret/recovery"}).to_string()).unwrap();
    fs::write(
        run.join("shutdown-report.json"),
        json!({"ownership_resolved":true,"survivors":[],"errors":[]}).to_string(),
    )
    .unwrap();
    (directory, run)
}
fn write_journal(run: &Path, entries: &[Value]) {
    fs::write(
        run.join("events.jsonl"),
        entries.iter().map(|e| format!("{e}\n")).collect::<String>(),
    )
    .unwrap();
}
fn event(time: u64, kind: &str, data: Value) -> Value {
    json!({"time_ms":time,"kind":kind,"data":data})
}

#[test]
fn report_exports_only_allowlisted_values_for_both_hosts() {
    for host in ["codex", "claude"] {
        let (_fixture, run) = fixture(host);
        write_journal(
            &run,
            &[
                event(
                    100,
                    "preparation_started",
                    json!({"invocation_received_at_ms":95,"task":"PRIVATE_PROMPT"}),
                ),
                event(120, "workspaces_prepared", json!({})),
                event(
                    200,
                    "native",
                    json!({"command":"SECRET_COMMAND","output":"SECRET_OUTPUT"}),
                ),
            ],
        );
        fs::write(run.join("workspace/delivery/previous-0"), "SECRET_SOURCE").unwrap();
        let report = report_at(&run).unwrap();
        let encoded = report.to_string();
        for secret in [
            "PRIVATE_PROMPT",
            "SECRET_COMMAND",
            "SECRET_OUTPUT",
            "SECRET_SOURCE",
            "source-secret.ts",
            "/secret/recovery",
            "/private/project-secret",
        ] {
            assert!(!encoded.contains(secret), "export leaked {secret}");
        }
        assert_eq!(report["host"], host);
        assert_eq!(report["timing"]["phases"]["preparation"], 20);
        assert_eq!(report["delivery"]["changed_file_count"], 1);
        assert_eq!(report["timing"]["closed_worker_overlap_ms"], Value::Null);
    }
}
#[test]
fn timing_separates_overlap_waiting_and_unobserved_phases() {
    let mut timing = Timings::default();
    for value in [
        event(10, "worker_turn_started", json!({"worker":1})),
        event(20, "worker_turn_started", json!({"worker":2})),
        event(
            50,
            "worker_turn_finished",
            json!({"worker":1,"waiting":true}),
        ),
        event(60, "worker_turn_finished", json!({"worker":2})),
        event(70, "worker_turn_started", json!({"worker":1})),
        event(90, "worker_turn_finished", json!({"worker":1})),
    ] {
        timing.record(&value);
    }
    let summary = timing.summary();
    assert_eq!(summary["closed_worker_overlap_ms"], 30);
    assert_eq!(summary["closed_worker_turn_ms"]["1"], 60);
    assert_eq!(summary["closed_waiting_ms"], 20);
    assert_eq!(summary["phases"]["delivery"], Value::Null);
}
#[test]
fn active_missing_and_unresolved_records_block_cleanup() {
    for host in ["claude", "codex"] {
        let (_fixture, run) = fixture(host);
        assert!(inspect(&run).unwrap().cleanup_blockers.is_empty());
        let state_path = run.join(if host == "claude" {
            "claude.json"
        } else {
            "run.json"
        });
        for status in [
            "running",
            "stopped",
            "delivery_conflict",
            "recovery_required",
            "secret-status",
        ] {
            let mut state = read_json(&state_path).unwrap().unwrap();
            state["status"] = json!(status);
            fs::write(&state_path, state.to_string()).unwrap();
            assert!(clean_at(&run, false).is_err());
            assert!(state_path.exists());
            if status == "secret-status" {
                assert_eq!(inspect(&run).unwrap().status, "unknown");
            }
        }
        fs::remove_file(&state_path).unwrap();
        assert!(clean_at(&run, true).is_err());
    }
}
#[test]
fn cleanup_rejects_native_uncertainty_and_remaining_workspaces() {
    let (_fixture, run) = fixture("codex");
    fs::remove_file(run.join("shutdown-report.json")).unwrap();
    assert!(
        inspect(&run)
            .unwrap()
            .cleanup_blockers
            .contains(&"native_shutdown_not_confirmed")
    );
    fs::create_dir(run.join("workspace/capture-2")).unwrap();
    assert!(
        inspect(&run)
            .unwrap()
            .cleanup_blockers
            .contains(&"temporary_workspaces_remain")
    );
    let (_fixture, run) = fixture("claude");
    let mut state = read_json(&run.join("claude.json")).unwrap().unwrap();
    state["finished"] = json!(false);
    fs::write(run.join("claude.json"), state.to_string()).unwrap();
    assert!(
        inspect(&run)
            .unwrap()
            .cleanup_blockers
            .contains(&"native_run_not_finished")
    );
}
#[test]
fn inspection_and_cleanup_never_follow_evidence_links() {
    let (fixture, run) = fixture("codex");
    let outside = fixture.path().join("private");
    fs::write(&outside, "SECRET").unwrap();
    symlink(&outside, run.join("events.jsonl")).unwrap();
    assert!(report_at(&run).is_err());
    assert!(prune_candidates(&run).is_err());
    assert_eq!(fs::read_to_string(outside).unwrap(), "SECRET");
    fs::remove_dir_all(run.join("workspace/delivery")).unwrap();
    let outside_dir = fixture.path().join("other");
    fs::create_dir(&outside_dir).unwrap();
    symlink(&outside_dir, run.join("workspace/delivery")).unwrap();
    assert!(inspect(&run).is_err());
}
#[test]
fn diagnostics_do_not_classify_same_command_on_changed_inputs_as_repeated() {
    let (_fixture, run) = fixture("codex");
    fs::create_dir(run.join("board")).unwrap();
    let db = rusqlite::Connection::open(run.join("board/board.sqlite3")).unwrap();
    db.execute("CREATE TABLE check_receipts (body TEXT)", [])
        .unwrap();
    for (revision, file) in [(1, "a"), (1, "a"), (1, "b"), (2, "a")] {
        let value = json!({"evidence":{"command":"PRIVATE_CHECK"},"request_revision":revision,"files":{"private.ts":file}});
        db.execute("INSERT INTO check_receipts VALUES (?)", [value.to_string()])
            .unwrap();
    }
    let count = check_counts(&run).unwrap();
    assert_eq!(count["receipt_count"], 4);
    assert_eq!(count["repeated_scope_and_command_count"], 1);
    assert!(!count.to_string().contains("PRIVATE_CHECK"));
}
#[test]
fn partial_last_record_is_visible_without_exposing_content() {
    let (_fixture, run) = fixture("codex");
    fs::write(run.join("events.jsonl"), "{\"secret\":").unwrap();
    let report = report_at(&run).unwrap();
    assert_eq!(report["timing"]["partial_journal"], true);
    assert_eq!(report["timing"]["records_skipped"], 1);
}

#[test]
fn new_response_samples_do_not_double_count_legacy_body_events() {
    let mut timing = Timings::default();
    timing.record(&event(
        1,
        "claude_coordination",
        json!({"response":{"result":"legacy"}}),
    ));
    timing.record(&event(
        2,
        "coordination_response",
        json!({"bytes":89,"worker":1}),
    ));
    let summary = timing.summary();
    assert_eq!(summary["coordination_response_samples"], 1);
    assert_eq!(summary["coordination_response_bytes"], 89);
    assert_eq!(
        summary["response_samples_include_local_command_metadata"],
        true
    );
}

#[test]
fn retained_summary_cannot_add_private_export_fields() {
    let summary = retained_timing(&json!({"phases":{"preparation":42,"secret-name":"SECRET"},
        "failed_phase_counts":{"secret-name":3}, "closed_worker_turn_ms":{"PRIVATE_THREAD":900},
        "interpretation":"SECRET", "command":"SECRET", "records_read":"SECRET"}));
    assert_eq!(summary["phases"]["preparation"], 42);
    assert!(!summary.to_string().contains("SECRET"));
    assert!(!summary.to_string().contains("secret-name"));
    assert!(!summary.to_string().contains("PRIVATE_THREAD"));
}

#[test]
fn preparation_failure_is_identified_without_promoting_it_to_completed_delivery() {
    let (_fixture, run) = fixture("codex");
    fs::write(
        run.join("run.json"),
        json!({"host":"codex","status":"preparation_failed",
        "native_started":false,"finished":true,"workspace_cleanup_complete":true,
        "project":"/PRIVATE_PROJECT","reason":"SECRET_REASON"})
        .to_string(),
    )
    .unwrap();
    let report = report_at(&run).unwrap();
    assert_eq!(report["status"], "preparation_failed");
    assert!(
        inspect(&run)
            .unwrap()
            .cleanup_blockers
            .contains(&"run_not_successfully_delivered")
    );
    assert!(!report.to_string().contains("SECRET_REASON"));
    assert!(!report.to_string().contains("PRIVATE_PROJECT"));
}
