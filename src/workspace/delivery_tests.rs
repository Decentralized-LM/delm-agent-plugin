use super::*;
use tempfile::TempDir;

fn fixture() -> (TempDir, PreparedWorkspace) {
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
    let prepared = prepare(&project, &temp.path().join("run"), u64::MAX).unwrap();
    (temp, prepared)
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
        "home = /usr/bin\n",
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
