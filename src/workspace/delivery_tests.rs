use super::*;
use tempfile::TempDir;

fn fixture() -> (TempDir, PreparedWorkspace) {
    fixture_with_workers(crate::config::DEFAULT_WORKER_COUNT)
}

fn fixture_with_workers(worker_count: usize) -> (TempDir, PreparedWorkspace) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    assert!(
        git::command()
            .arg("init")
            .arg("--quiet")
            .arg(&project)
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        project.join("file.txt"),
        "first\nsecond\nthird\nfourth\nfifth\n",
    )
    .unwrap();
    assert!(
        git::command()
            .arg("-C")
            .arg(&project)
            .args(["add", "file.txt"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        project.join("file.txt"),
        "first\nsecond\nthird\nfourth\nfifth\ninitial dirty\n",
    )
    .unwrap();
    fs::write(project.join("untracked"), "keep untracked\n").unwrap();
    let prepared =
        prepare_with_workers(&project, &temp.path().join("run"), u64::MAX, worker_count).unwrap();
    (temp, prepared)
}

#[test]
#[cfg(target_os = "macos")]
fn four_worker_delivery_uses_selected_result_and_cleans_the_entire_roster() {
    let (_temp, prepared) = fixture_with_workers(4);
    assert_eq!(prepared.workers.len(), 4);
    let index = fs::read(prepared.original.join(".git/index")).unwrap();
    for (worker, path) in prepared.workers.iter().enumerate() {
        fs::write(path.join("result.txt"), format!("worker {}", worker + 1)).unwrap();
    }
    let report = deliver_result(&prepared, 3, &ResultPolicy::default()).unwrap();
    assert!(report.delivered && report.cleanup_complete);
    assert_eq!(
        fs::read_to_string(prepared.original.join("result.txt")).unwrap(),
        "worker 4"
    );
    assert_eq!(
        fs::read(prepared.original.join(".git/index")).unwrap(),
        index
    );
    assert!(prepared.workers.iter().all(|path| !path.exists()));
    assert!(!prepared.baseline.exists());
    assert!(
        deliver_result(&prepared, 3, &ResultPolicy::default())
            .unwrap()
            .delivered
    );
}

#[test]
#[cfg(target_os = "macos")]
fn four_worker_recovery_preserves_every_peer_and_exports_worker_four() {
    let (temp, prepared) = fixture_with_workers(4);
    for (worker, path) in prepared.workers.iter().enumerate() {
        fs::write(path.join("result.txt"), format!("partial {}", worker + 1)).unwrap();
    }
    let report = preserve_partial_and_cleanup(&prepared).unwrap();
    assert!(report.cleanup_complete);
    let inspection = inspect_recovery(&report.recovery).unwrap();
    assert_eq!(inspection.worker_count, 4);
    assert_eq!(
        inspection
            .workers
            .iter()
            .map(|worker| worker.worker)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    let export = export_recovery(&report.recovery, &temp.path().join("export-four"), 4).unwrap();
    assert_eq!(
        fs::read_to_string(export.files.join("result.txt")).unwrap(),
        "partial 4"
    );
    assert!(prepared.workers.iter().all(|path| !path.exists()));
    assert!(!prepared.original.join("result.txt").exists());
    assert!(export_recovery(&report.recovery, &temp.path().join("export-five"), 5).is_err());

    let manifest = report.recovery.join("complete.json");
    let mut saved: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    saved["worker_count"] = serde_json::json!(2);
    fs::write(&manifest, serde_json::to_vec(&saved).unwrap()).unwrap();
    assert!(inspect_recovery(&report.recovery).is_err());
    assert!(export_recovery(&report.recovery, &temp.path().join("forged-four"), 4).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn legacy_two_worker_state_and_recovery_remain_readable() {
    let (temp, prepared) = fixture();
    let legacy: PreparedWorkspace =
        serde_json::from_slice(&serde_json::to_vec(&prepared).unwrap()).unwrap();
    legacy.validate_layout().unwrap();
    assert_eq!(legacy.workers.len(), 2);
    assert!(deliver_result(&legacy, 2, &ResultPolicy::default()).is_err());
    assert!(legacy.workers.iter().all(|path| path.exists()));
    fs::write(legacy.workers[1].join("result.txt"), "legacy second worker").unwrap();
    let report = preserve_partial_and_cleanup(&legacy).unwrap();
    let manifest = report.recovery.join("complete.json");
    let mut saved: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    saved.as_object_mut().unwrap().remove("worker_count");
    fs::write(&manifest, serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(inspect_recovery(&report.recovery).unwrap().worker_count, 2);
    let export = export_recovery(&report.recovery, &temp.path().join("legacy-export"), 2).unwrap();
    assert_eq!(
        fs::read_to_string(export.files.join("result.txt")).unwrap(),
        "legacy second worker"
    );
    assert!(export_recovery(&report.recovery, &temp.path().join("legacy-four"), 4).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn altered_workspace_roster_cannot_deliver_or_remove_owned_trees() {
    let (_temp, mut prepared) = fixture_with_workers(4);
    let all_workers = prepared.workers.clone();
    prepared.workers.truncate(2);
    assert!(deliver_result(&prepared, 0, &ResultPolicy::default()).is_err());
    assert!(preserve_partial_and_cleanup(&prepared).is_err());
    assert!(all_workers.iter().all(|path| path.exists()));
    prepared.workers = all_workers;
    prepared.workers[3] = prepared.workers[0].clone();
    assert!(prepared.validate_layout().is_err());
    assert!(retain_result(&prepared, 3).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn delivery_preserves_index_dirty_inputs_and_applies_assets_links_modes_deletions() {
    let (_temp, prepared) = fixture();
    let index = fs::read(prepared.original.join(".git/index")).unwrap();
    let worker = &prepared.workers[0];
    fs::write(worker.join("file.txt"), "new contents\ninitial dirty\n").unwrap();
    fs::create_dir(worker.join("assets")).unwrap();
    fs::write(worker.join("assets/image.bin"), [0, 255, 42, 0, 18]).unwrap();
    fs::write(worker.join("launch"), "#!/bin/sh\necho ready\n").unwrap();
    fs::set_permissions(worker.join("launch"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("assets/image.bin", worker.join("picture")).unwrap();
    fs::remove_file(worker.join("untracked")).unwrap();
    fs::write(prepared.original.join("concurrent"), "user added this\n").unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(report.delivered && report.cleanup_complete);
    assert_eq!(
        fs::read(prepared.original.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"new contents\ninitial dirty\n"
    );
    assert_eq!(
        fs::read(prepared.original.join("picture")).unwrap(),
        [0, 255, 42, 0, 18]
    );
    assert_eq!(
        fs::metadata(prepared.original.join("launch"))
            .unwrap()
            .mode()
            & 0o777,
        0o755
    );
    assert!(!prepared.original.join("untracked").exists());
    assert_eq!(
        fs::read(prepared.original.join("concurrent")).unwrap(),
        b"user added this\n"
    );
    assert!(prepared.workers.iter().all(|p| !p.exists()));
    assert!(!prepared.baseline.exists());
    assert!(
        deliver_result(&prepared, 0, &ResultPolicy::default())
            .unwrap()
            .delivered
    );
}

#[test]
#[cfg(target_os = "macos")]
fn delivery_merges_nonoverlapping_edits_against_saved_working_tree() {
    let (_temp, prepared) = fixture();
    fs::write(
        prepared.workers[0].join("file.txt"),
        "worker first\nsecond\nthird\nfourth\nfifth\ninitial dirty\n",
    )
    .unwrap();
    fs::write(
        prepared.original.join("file.txt"),
        "first\nsecond\nthird\nfourth\nuser fifth\ninitial dirty\n",
    )
    .unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(report.delivered);
    assert_eq!(report.merged_paths, ["file.txt"]);
    assert!(report.verification_required);
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"worker first\nsecond\nthird\nfourth\nuser fifth\ninitial dirty\n"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn conflicting_edits_save_both_workers_and_leave_original_unchanged() {
    let (_temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("file.txt"), "worker value\n").unwrap();
    fs::write(prepared.workers[1].join("new.txt"), "peer contribution\n").unwrap();
    fs::write(prepared.original.join("file.txt"), "user value\n").unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(!report.delivered);
    assert_eq!(report.conflicts, ["file.txt"]);
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"user value\n"
    );
    let recovery = report.recovery.unwrap();
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(recovery.join("complete.json")).unwrap()).unwrap();
    assert!(metadata["workers"][1]["changes"]["new.txt"].is_array());
    assert_eq!(
        fs::read(recovery.join(format!("{:x}", Sha256::digest(b"peer contribution\n")))).unwrap(),
        b"peer contribution\n"
    );
    assert!(prepared.workers.iter().all(|p| !p.exists()));
    assert!(!prepared.baseline.exists());
}

#[test]
#[cfg(target_os = "macos")]
fn local_dependencies_are_not_transferred_and_existing_environment_survives() {
    let (_temp, prepared) = fixture();
    fs::create_dir(prepared.original.join("node_modules")).unwrap();
    fs::write(
        prepared.original.join("node_modules/local"),
        "original dependency",
    )
    .unwrap();
    fs::create_dir(prepared.workers[0].join("node_modules")).unwrap();
    fs::write(
        prepared.workers[0].join("node_modules/temporary"),
        "temporary dependency",
    )
    .unwrap();
    fs::create_dir_all(prepared.workers[0].join("env/bin")).unwrap();
    fs::write(
        prepared.workers[0].join("env/pyvenv.cfg"),
        "home = /usr/bin\ninclude-system-site-packages = false\nversion = 3.12.0\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "/usr/bin/python3",
        prepared.workers[0].join("env/bin/python"),
    )
    .unwrap();
    fs::write(prepared.workers[0].join("package-lock.json"), "{}\n").unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(report.delivered);
    assert_eq!(report.environment_files_changed, ["package-lock.json"]);
    assert_eq!(
        report.environment_directories_omitted,
        ["env", "node_modules"]
    );
    assert!(report.verification_required);
    assert_eq!(
        fs::read(prepared.original.join("node_modules/local")).unwrap(),
        b"original dependency"
    );
    assert!(!prepared.original.join("node_modules/temporary").exists());
    assert!(!prepared.original.join("env").exists());
    assert!(prepared.original.join("package-lock.json").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn unchanged_manifests_do_not_prove_original_dependencies_are_ready() {
    let (temp, first) = fixture();
    fs::write(
        first.original.join("package.json"),
        "{\"dependencies\":{\"example\":\"1.0.0\"}}\n",
    )
    .unwrap();
    let prepared = prepare(
        &first.original,
        &temp.path().join("with-manifest"),
        u64::MAX,
    )
    .unwrap();
    fs::create_dir(prepared.workers[0].join("node_modules")).unwrap();
    fs::write(
        prepared.workers[0].join("node_modules/example.js"),
        "worker dependency",
    )
    .unwrap();
    fs::write(
        prepared.workers[0].join("file.txt"),
        "import example from 'example';\n",
    )
    .unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(report.delivered && report.verification_required);
    assert!(report.environment_files_changed.is_empty());
    assert_eq!(report.environment_directories_omitted, ["node_modules"]);
    assert!(!prepared.original.join("node_modules").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn durable_report_does_not_claim_cleanup_until_all_workers_are_removed() {
    let (_temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("file.txt"), "result\n").unwrap();
    // UF_IMMUTABLE makes deletion fail after delivery, without preventing read
    // access or depending on the effective user's permission bypasses.
    let blocked = prepared.workers[1].join("file.txt");
    let path = cstr(blocked.as_os_str()).unwrap();
    assert_eq!(
        unsafe { libc::chflags(path.as_ptr(), libc::UF_IMMUTABLE as _) },
        0
    );
    let result = deliver_result(&prepared, 0, &ResultPolicy::default());
    assert_eq!(unsafe { libc::chflags(path.as_ptr(), 0) }, 0);
    assert!(result.is_err());
    let report_path = prepared.run_dir.join("workspace/delivery/result.json");
    let report: DeliveryReport = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert!(report.delivered && !report.cleanup_complete);
    assert!(prepared.baseline.exists());
    let completed = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(completed.cleanup_complete);
    let saved: DeliveryReport = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert!(saved.cleanup_complete);
    assert!(prepared.workers.iter().all(|path| !path.exists()));
    assert!(!prepared.baseline.exists());
}

#[test]
#[cfg(target_os = "macos")]
fn concurrent_save_through_displaced_original_descriptor_is_preserved_and_reported() {
    let (_temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("file.txt"), "worker result\n").unwrap();
    let mut editor = OpenOptions::new()
        .write(true)
        .open(prepared.original.join("file.txt"))
        .unwrap();
    BEFORE_VERIFICATION.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            editor.set_len(0).unwrap();
            editor.write_all(b"concurrent editor save\n").unwrap();
            editor.sync_all().unwrap();
        }));
    });
    let error = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("concurrent edit to displaced original file.txt")
    );
    let recovery = preserve_partial_and_cleanup(&prepared).unwrap();
    assert!(recovery.cleanup_complete);
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"worker result\n"
    );
    assert_eq!(
        fs::read(prepared.run_dir.join("workspace/delivery/previous-0")).unwrap(),
        b"concurrent editor save\n"
    );
    assert!(prepared.workers.iter().all(|path| !path.exists()));
}

#[test]
#[cfg(target_os = "macos")]
fn successful_delivery_keeps_original_inodes_available_for_late_editor_saves() {
    let (_temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("file.txt"), "worker result\n").unwrap();
    let mut editor = OpenOptions::new()
        .write(true)
        .open(prepared.original.join("file.txt"))
        .unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(report.delivered && report.cleanup_complete);
    let recovery = report.recovery.unwrap();
    editor.set_len(0).unwrap();
    editor
        .write_all(b"save after final verification\n")
        .unwrap();
    editor.sync_all().unwrap();
    assert_eq!(
        fs::read(recovery.join("previous-0")).unwrap(),
        b"save after final verification\n"
    );
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"worker result\n"
    );
    assert!(prepared.workers.iter().all(|path| !path.exists()));
    assert!(!prepared.baseline.exists());
}

#[test]
#[cfg(target_os = "macos")]
fn deleting_a_tree_preserves_concurrently_added_children_as_a_conflict() {
    let (temp, _) = fixture();
    let project = temp.path().join("project");
    fs::create_dir(project.join("old")).unwrap();
    fs::write(project.join("old/obsolete"), "before").unwrap();
    let prepared = prepare(&project, &temp.path().join("second-run"), u64::MAX).unwrap();
    fs::remove_dir_all(prepared.workers[0].join("old")).unwrap();
    fs::write(prepared.original.join("old/user-new"), "user work").unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(!report.delivered);
    assert!(report.conflicts.contains(&"old".into()));
    assert_eq!(
        fs::read(prepared.original.join("old/user-new")).unwrap(),
        b"user work"
    );
    assert_eq!(
        fs::read(prepared.original.join("old/obsolete")).unwrap(),
        b"before"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn directory_deletions_and_mode_changes_are_delivered_without_copying_the_tree() {
    let (temp, _) = fixture();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("old/nested")).unwrap();
    fs::write(project.join("old/nested/obsolete"), "remove me").unwrap();
    fs::create_dir(project.join("private-data")).unwrap();
    let prepared = prepare(&project, &temp.path().join("second-run"), u64::MAX).unwrap();
    fs::remove_dir_all(prepared.workers[0].join("old")).unwrap();
    fs::set_permissions(
        prepared.workers[0].join("private-data"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(report.delivered);
    assert!(!prepared.original.join("old").exists());
    assert_eq!(
        fs::metadata(prepared.original.join("private-data"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
#[cfg(target_os = "macos")]
fn interrupted_apply_does_not_rollback_later_user_edits() {
    let (_temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("file.txt"), "worker result").unwrap();
    let root = open_dir(&prepared.original).unwrap();
    let directory = prepared.run_dir.join("workspace/delivery");
    private_directory(&directory).unwrap();
    let after = entry(&open_dir(&prepared.workers[0]).unwrap(), "file.txt").unwrap();
    stage_entry(
        &prepared.workers[0],
        "file.txt",
        after.as_ref().unwrap(),
        &directory.join("next-0"),
    )
    .unwrap();
    let change = Change {
        path: "file.txt".into(),
        before: entry(&root, "file.txt").unwrap(),
        after,
        stage: "next-0".into(),
        displaced: "previous-0".into(),
        applied: false,
    };
    let journal = Journal {
        version: 1,
        project: prepared.original.clone(),
        project_identity: identity(&prepared.original).unwrap(),
        worker: 0,
        changes: vec![change],
        complete: false,
    };
    checkpoint(&directory, &journal).unwrap();
    apply_change(&root, &directory, &journal.changes[0]).unwrap();
    // Simulate the process exiting before the per-file completion checkpoint.
    fs::write(prepared.original.join("file.txt"), "later user edit").unwrap();
    assert!(deliver_result(&prepared, 0, &ResultPolicy::default()).is_err());
    let recovery = preserve_partial_and_cleanup(&prepared).unwrap();
    assert!(recovery.cleanup_complete);
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"later user edit"
    );
    assert_eq!(
        fs::read(directory.join("previous-0")).unwrap(),
        b"first\nsecond\nthird\nfourth\nfifth\ninitial dirty\n"
    );
    assert!(directory.join("journal.json").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn replacing_original_root_blocks_delivery_but_still_preserves_and_cleans_workers() {
    let (temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("file.txt"), "worker changes").unwrap();
    fs::rename(&prepared.original, temp.path().join("moved-project")).unwrap();
    fs::create_dir(&prepared.original).unwrap();
    fs::write(prepared.original.join("file.txt"), "replacement project").unwrap();
    assert!(deliver_result(&prepared, 0, &ResultPolicy::default()).is_err());
    let report = preserve_partial_and_cleanup(&prepared).unwrap();
    assert!(report.cleanup_complete);
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"replacement project"
    );
    assert!(
        report
            .recovery
            .join(format!("{:x}", Sha256::digest(b"worker changes")))
            .exists()
    );
}

#[test]
#[cfg(target_os = "macos")]
fn cancellation_saves_binary_and_link_changes_and_cleanup_is_repeatable() {
    let (_temp, prepared) = fixture();
    fs::write(prepared.workers[1].join("binary"), [0, 255, 1]).unwrap();
    std::os::unix::fs::symlink("missing-target", prepared.workers[1].join("partial-link")).unwrap();
    let report = preserve_partial_and_cleanup(&prepared).unwrap();
    assert!(report.cleanup_complete && report.recovery.join("complete.json").exists());
    assert!(prepared.workers.iter().all(|p| !p.exists()));
    assert_eq!(
        fs::read(prepared.original.join("file.txt")).unwrap(),
        b"first\nsecond\nthird\nfourth\nfifth\ninitial dirty\n"
    );
    assert!(
        preserve_partial_and_cleanup(&prepared)
            .unwrap()
            .cleanup_complete
    );
}

#[test]
#[cfg(target_os = "macos")]
fn concurrent_symlink_parent_cannot_redirect_delivery() {
    let (temp, prepared) = fixture();
    fs::create_dir(prepared.workers[0].join("new-directory")).unwrap();
    fs::write(
        prepared.workers[0].join("new-directory/file"),
        "worker source",
    )
    .unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file"), "do not overwrite").unwrap();
    std::os::unix::fs::symlink(&outside, prepared.original.join("new-directory")).unwrap();
    let report = deliver_result(&prepared, 0, &ResultPolicy::default()).unwrap();
    assert!(!report.delivered);
    assert_eq!(fs::read(outside.join("file")).unwrap(), b"do not overwrite");
    assert!(prepared.workers.iter().all(|p| !p.exists()));
}

#[test]
#[cfg(target_os = "macos")]
fn declared_ignored_artifacts_share_capture_verify_and_delivery_contract() {
    let (_temp, prepared) = fixture();
    let worker = &prepared.workers[0];
    fs::write(worker.join(".gitignore"), "renders/\n*.log\n").unwrap();
    fs::create_dir(worker.join("renders")).unwrap();
    fs::write(worker.join("renders/movie.mp4"), [0, 1, 255, 8]).unwrap();
    fs::write(worker.join("report.log"), "requested log artifact").unwrap();
    let accepted = ResultSelection::new(
        &prepared.baseline_manifest,
        vec!["renders".into(), "report.log".into()],
    )
    .unwrap()
    .capture(worker)
    .unwrap();
    assert_eq!(accepted.artifact_files, ["renders/movie.mp4", "report.log"]);
    fs::write(worker.join("server.log"), "shutdown diagnostic").unwrap();
    accepted.verify(worker).unwrap();
    let report = deliver_accepted_result(&prepared, 0, &accepted).unwrap();
    assert!(report.delivered && report.cleanup_complete);
    assert_eq!(
        fs::read(prepared.original.join("renders/movie.mp4")).unwrap(),
        [0, 1, 255, 8]
    );
    assert_eq!(
        fs::read_to_string(prepared.original.join("report.log")).unwrap(),
        "requested log artifact"
    );
    assert!(!prepared.original.join("server.log").exists());
    assert!(prepared.workers.iter().all(|path| !path.exists()));
}

#[test]
#[cfg(target_os = "macos")]
fn ignored_unclassified_outputs_are_recoverable_for_both_peers_before_cleanup() {
    let (temp, prepared) = fixture();
    let worker = &prepared.workers[0];
    fs::write(worker.join(".gitignore"), "renders/\n").unwrap();
    fs::create_dir(worker.join("renders")).unwrap();
    fs::write(worker.join("renders/movie.mp4"), "movie bytes").unwrap();
    fs::write(
        prepared.workers[1].join("peer-work.txt"),
        "useful peer work",
    )
    .unwrap();
    fs::write(worker.join(".env"), "TOKEN=do not export").unwrap();
    let accepted = ResultSelection::new(&prepared.baseline_manifest, vec![])
        .unwrap()
        .capture(worker)
        .unwrap();
    assert_eq!(accepted.undelivered_outputs, ["renders/movie.mp4"]);
    let report = deliver_accepted_result(&prepared, 0, &accepted).unwrap();
    assert!(!report.delivered && report.cleanup_complete && report.conflicts.is_empty());
    assert_eq!(report.undelivered_outputs, ["renders/movie.mp4"]);
    let bundle = report.recovery.unwrap();
    let inspected = inspect_recovery(&bundle).unwrap();
    assert_eq!(inspected.workers.len(), 2);
    assert!(
        inspected
            .workers
            .iter()
            .flat_map(|w| &w.changes)
            .all(|entry| entry.path != ".env")
    );
    let export = export_recovery(&bundle, &temp.path().join("export-one"), 1).unwrap();
    assert_eq!(
        fs::read_to_string(export.files.join("renders/movie.mp4")).unwrap(),
        "movie bytes"
    );
    assert!(!export.files.join(".env").exists());
    let export = export_recovery(&bundle, &temp.path().join("export-two"), 2).unwrap();
    assert_eq!(
        fs::read_to_string(export.files.join("peer-work.txt")).unwrap(),
        "useful peer work"
    );
    assert!(prepared.workers.iter().all(|path| !path.exists()));
}

#[test]
#[cfg(target_os = "macos")]
fn selected_artifact_change_rejects_delivery_while_ignored_log_changes_do_not() {
    let (_temp, prepared) = fixture();
    let worker = &prepared.workers[0];
    fs::write(worker.join(".gitignore"), "*.log\nmovie.mp4\n.cache/\n").unwrap();
    fs::write(worker.join("movie.mp4"), "first render").unwrap();
    fs::write(worker.join("server.log"), "start").unwrap();
    fs::create_dir(worker.join(".cache")).unwrap();
    fs::write(worker.join(".cache/data"), "cache one").unwrap();
    let accepted = ResultSelection::new(&prepared.baseline_manifest, vec!["movie.mp4".into()])
        .unwrap()
        .capture(worker)
        .unwrap();
    fs::write(worker.join("server.log"), "shutdown").unwrap();
    fs::write(worker.join(".cache/data"), "cache two").unwrap();
    accepted.verify(worker).unwrap();
    fs::write(worker.join("movie.mp4"), "second render").unwrap();
    assert!(deliver_accepted_result(&prepared, 0, &accepted).is_err());
    assert!(worker.exists());
    assert!(!prepared.original.join("movie.mp4").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn artifact_selection_rejects_unsafe_paths_secrets_dependencies_and_escaping_links() {
    let (_temp, prepared) = fixture();
    let worker = &prepared.workers[0];
    for path in ["../outside", "/absolute", ".git/config"] {
        assert!(ResultSelection::new(&prepared.baseline_manifest, vec![path.into()]).is_err());
    }
    fs::write(worker.join(".env"), "TOKEN=secret").unwrap();
    fs::create_dir(worker.join("node_modules")).unwrap();
    fs::write(worker.join("node_modules/dependency"), "dependency").unwrap();
    for path in [".env", "node_modules", "missing"] {
        assert!(
            ResultSelection::new(&prepared.baseline_manifest, vec![path.into()])
                .unwrap()
                .capture(worker)
                .is_err()
        );
    }
    fs::remove_file(worker.join(".env")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", worker.join("source-link")).unwrap();
    assert!(
        ResultSelection::new(&prepared.baseline_manifest, vec![])
            .unwrap()
            .capture(worker)
            .is_err()
    );
}

#[test]
#[cfg(target_os = "macos")]
fn malformed_environment_config_does_not_hide_source_and_credentials_do_not_delete_baseline() {
    let (_temp, prepared) = fixture();
    let worker = &prepared.workers[0];
    fs::create_dir(worker.join("custom")).unwrap();
    fs::write(worker.join("custom/pyvenv.cfg"), "not a Python environment").unwrap();
    fs::write(worker.join("custom/source.py"), "print('deliver me')").unwrap();
    let accepted = ResultSelection::new(&prepared.baseline_manifest, vec![])
        .unwrap()
        .capture(worker)
        .unwrap();
    assert!(accepted.manifest.files.contains_key("custom/source.py"));
    fs::write(
        worker.join("file.txt"),
        "-----BEGIN PRIVATE KEY-----\nprivate\n",
    )
    .unwrap();
    assert!(
        ResultSelection::new(&prepared.baseline_manifest, vec![])
            .unwrap()
            .capture(worker)
            .is_err()
    );
    assert!(prepared.original.join("file.txt").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn recovery_export_preserves_binary_modes_and_deletions_without_activating_symlinks() {
    let (temp, prepared) = fixture();
    let worker = &prepared.workers[0];
    fs::remove_file(worker.join("untracked")).unwrap();
    fs::write(worker.join("launch"), [0, 255, 42]).unwrap();
    fs::set_permissions(worker.join("launch"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", worker.join("external-link")).unwrap();
    let report = preserve_partial_and_cleanup(&prepared).unwrap();
    let exported = export_recovery(&report.recovery, &temp.path().join("restored"), 1).unwrap();
    assert_eq!(
        fs::read(exported.files.join("launch")).unwrap(),
        [0, 255, 42]
    );
    assert_eq!(
        fs::metadata(exported.files.join("launch")).unwrap().mode() & 0o777,
        0o755
    );
    assert!(fs::symlink_metadata(exported.files.join("external-link")).is_err());
    assert!(!exported.files.join("untracked").exists());
    assert_eq!(
        fs::read_to_string(exported.base.join("untracked")).unwrap(),
        "keep untracked\n"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(exported.manifest).unwrap()).unwrap();
    assert!(manifest["changes"]["untracked"][1].is_null());
    assert_eq!(
        manifest["changes"]["external-link"][1]["link_target"],
        "/etc/passwd"
    );
    assert_eq!(
        fs::read_to_string(prepared.original.join("untracked")).unwrap(),
        "keep untracked\n"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn recovery_export_rejects_corruption_and_existing_or_unsafe_destinations() {
    let (temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("new"), "saved bytes").unwrap();
    let report = preserve_partial_and_cleanup(&prepared).unwrap();
    let existing = temp.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("keep"), "untouched").unwrap();
    assert!(export_recovery(&report.recovery, &existing, 1).is_err());
    assert!(export_recovery(&report.recovery, &prepared.original.join("nested"), 1).is_err());
    let hash = format!("{:x}", Sha256::digest(b"saved bytes"));
    fs::write(report.recovery.join(hash), "bad bytes").unwrap();
    assert!(inspect_recovery(&report.recovery).is_err());
    let destination = temp.path().join("must-not-exist");
    assert!(export_recovery(&report.recovery, &destination, 1).is_err());
    assert!(!destination.exists());
    assert_eq!(
        fs::read_to_string(existing.join("keep")).unwrap(),
        "untouched"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn durable_delivery_retry_after_cleanup_never_reapplies_over_later_user_edits() {
    let (_temp, prepared) = fixture();
    fs::write(prepared.workers[0].join("file.txt"), "accepted result").unwrap();
    let accepted = ResultSelection::new(&prepared.baseline_manifest, vec![])
        .unwrap()
        .capture(&prepared.workers[0])
        .unwrap();
    let report = deliver_accepted_result(&prepared, 0, &accepted).unwrap();
    assert!(report.delivered && report.cleanup_complete);
    assert!(!prepared.workers[0].exists());
    fs::write(prepared.original.join("file.txt"), "later user edit").unwrap();
    let retried = deliver_accepted_result(&prepared, 0, &accepted).unwrap();
    assert!(retried.delivered && retried.cleanup_complete);
    assert_eq!(
        fs::read_to_string(prepared.original.join("file.txt")).unwrap(),
        "later user edit"
    );
    assert!(deliver_accepted_result(&prepared, 1, &accepted).is_err());
    let journal_path = prepared.run_dir.join("workspace/delivery/journal.json");
    let mut journal: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
    journal["complete"] = serde_json::json!(false);
    fs::write(journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(deliver_accepted_result(&prepared, 0, &accepted).is_err());
    assert_eq!(
        fs::read_to_string(prepared.original.join("file.txt")).unwrap(),
        "later user edit"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn recovery_protects_declared_cache_path_outputs_and_preserves_nonignored_cache_named_source() {
    let (temp, prepared) = fixture();
    let worker = &prepared.workers[0];
    fs::write(worker.join(".gitignore"), ".cache/\n").unwrap();
    fs::create_dir(worker.join(".cache")).unwrap();
    fs::write(worker.join(".cache/requested.pdf"), "requested PDF").unwrap();
    let recovery =
        preserve_partial_and_cleanup_with_artifacts(&prepared, &[".cache/requested.pdf".into()])
            .unwrap();
    let exported =
        export_recovery(&recovery.recovery, &temp.path().join("cache-output"), 1).unwrap();
    assert_eq!(
        fs::read_to_string(exported.files.join(".cache/requested.pdf")).unwrap(),
        "requested PDF"
    );
    let (_temp, prepared) = fixture();
    fs::create_dir(prepared.workers[0].join(".cache")).unwrap();
    fs::write(
        prepared.workers[0].join(".cache/nonignored-source"),
        "user source",
    )
    .unwrap();
    let recovery = preserve_partial_and_cleanup(&prepared).unwrap();
    let inspected = inspect_recovery(&recovery.recovery).unwrap();
    assert!(
        inspected.workers[0]
            .changes
            .iter()
            .any(|entry| entry.path == ".cache/nonignored-source")
    );
}

#[test]
#[cfg(target_os = "macos")]
fn explicit_source_only_contract_keeps_incidental_build_output_without_blocking_delivery() {
    for explicit in [false, true] {
        let (temp, prepared) = fixture();
        let worker = &prepared.workers[0];
        fs::write(worker.join(".gitignore"), "dist/\n").unwrap();
        fs::create_dir(worker.join("dist")).unwrap();
        fs::write(worker.join("dist/generated.js"), "incidental build output").unwrap();
        fs::write(worker.join("source.js"), "requested source change").unwrap();
        let mut declaration = serde_json::json!({"outcome":"complete","expected_revision":1,"summary":"Source update complete","checks":[]});
        if explicit {
            declaration["artifacts"] = serde_json::json!([]);
        }
        let completion = crate::completion::Completion::capture_with_shared(
            worker,
            &declaration,
            &Default::default(),
            1,
            &ResultPolicy::default(),
            vec![],
            &prepared.baseline_manifest,
        )
        .unwrap();
        let report = completion.deliver(&prepared, 0).unwrap();
        assert_eq!(report.artifacts_declared, explicit);
        assert_eq!(report.delivered, explicit);
        assert_eq!(report.undelivered_outputs, ["dist/generated.js"]);
        assert_eq!(
            fs::read_to_string(prepared.original.join("source.js")).unwrap(),
            "requested source change"
        );
        assert!(!prepared.original.join("dist").exists());
        let export = export_recovery(
            report.recovery.as_ref().unwrap(),
            &temp.path().join("review"),
            1,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(export.files.join("dist/generated.js")).unwrap(),
            "incidental build output"
        );
        assert!(report.cleanup_complete && prepared.workers.iter().all(|path| !path.exists()));
    }
}
