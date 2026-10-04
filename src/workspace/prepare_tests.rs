use super::*;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project with spaces");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    git_command(&root, &["init", "--quiet", "--template="]);
    fs::write(root.join("tracked.txt"), "saved contents\n").unwrap();
    git_command(&root, &["add", "tracked.txt"]);
    (temp, root)
}

#[test]
#[cfg(target_os = "macos")]
fn absent_git_is_initialized_at_selected_root_without_discovering_parent() {
    let (temp, ancestor) = fixture();
    let before_index = fs::read(ancestor.join(".git/index")).unwrap();
    let project = ancestor.join("new project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("notes.txt"), "existing untracked input\n").unwrap();
    let prepared = prepare(&project, &temp.path().join("new-run"), 1_000_000).unwrap();
    assert_eq!(fs::read(ancestor.join(".git/index")).unwrap(), before_index);
    assert_eq!(
        fs::read(project.join(".git/HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert!(!project.join(".git/index").exists());
    assert!(!project.join(".git/refs/heads/main").exists());
    for worker in &prepared.workers {
        assert_eq!(
            fs::read(worker.join("notes.txt")).unwrap(),
            b"existing untracked input\n"
        );
        assert!(!worker.join("tracked.txt").exists());
    }
}

#[test]
#[cfg(target_os = "macos")]
fn existing_broken_git_entry_is_not_replaced() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    for kind in ["file", "symlink", "directory"] {
        let project = temp.path().join(kind);
        fs::create_dir(&project).unwrap();
        match kind {
            "file" => fs::write(project.join(".git"), "gitdir: /unavailable\n").unwrap(),
            "symlink" => symlink("missing", project.join(".git")).unwrap(),
            _ => fs::create_dir(project.join(".git")).unwrap(),
        }
        let entry = fs::symlink_metadata(project.join(".git")).unwrap();
        assert!(
            prepare(
                &project,
                &temp.path().join(format!("run-{kind}")),
                1_000_000
            )
            .is_err()
        );
        let after = fs::symlink_metadata(project.join(".git")).unwrap();
        assert_eq!(
            (entry.dev(), entry.ino(), entry.mode()),
            (after.dev(), after.ino(), after.mode())
        );
        match kind {
            "file" => assert_eq!(
                fs::read(project.join(".git")).unwrap(),
                b"gitdir: /unavailable\n"
            ),
            "symlink" => assert_eq!(
                fs::read_link(project.join(".git")).unwrap(),
                PathBuf::from("missing")
            ),
            _ => assert_eq!(fs::read_dir(project.join(".git")).unwrap().count(), 0),
        }
    }
}

#[test]
#[cfg(target_os = "macos")]
fn failed_capture_removes_owned_directories_and_preserves_source() {
    let (temp, project) = fixture();
    fs::write(project.join(".git/index.lock"), "external lock\n").unwrap();
    let before = fs::read(project.join(".git/index")).unwrap();
    let run = temp.path().join("failed-run");
    assert!(prepare(&project, &run, 1_000_000).is_err());
    assert!(!run.join("workspace/capture-0").exists());
    assert!(!run.join("workspace/baseline").exists());
    assert!(!run.join("workspace/worker-1").exists());
    assert!(!run.join("workspace/worker-2").exists());
    let journal = fs::read_to_string(run.join("workspace/preparation-ownership.jsonl")).unwrap();
    assert!(journal.contains("cleaned"));
    assert_eq!(fs::read(project.join(".git/index")).unwrap(), before);
    assert_eq!(
        fs::read(project.join(".git/index.lock")).unwrap(),
        b"external lock\n"
    );
}

#[test]
fn preparation_guard_preserves_replacement_and_unowned_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut guard = PreparationGuard::new(&workspace).unwrap();
    let owned = guard.create("capture-0").unwrap();
    fs::write(owned.join("partial"), "capture\n").unwrap();
    fs::create_dir(workspace.join("unrelated")).unwrap();
    fs::rename(&owned, temp.path().join("moved-capture")).unwrap();
    fs::create_dir(&owned).unwrap();
    fs::write(owned.join("user.txt"), "preserve\n").unwrap();
    assert!(guard.cleanup().is_err());
    assert_eq!(fs::read(owned.join("user.txt")).unwrap(), b"preserve\n");
    assert!(workspace.join("unrelated").exists());
}

fn git_command(root: &Path, arguments: &[&str]) {
    let output = git::command()
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn batched_directory_classification_matches_individual_git_queries() {
    let (_temp, root) = fixture();
    fs::write(
        root.join(".gitignore"),
        "ignored*/\n!ignored-keep/\nline*/\n",
    )
    .unwrap();
    let directories: BTreeSet<_> = [
        "empty",
        "ignored space",
        "ignored space/line\nchild",
        "ignored-keep",
        "--no-index",
        "tab\tname",
        "line\nname",
        "unicode-é",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for directory in &directories {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    let mut individual = BTreeSet::new();
    for directory in &directories {
        let result = git::command()
            .arg("-C")
            .arg(&root)
            .args(["check-ignore", "--quiet", "--", directory])
            .status()
            .unwrap();
        match result.code() {
            Some(0) => {
                individual.insert(directory.clone());
            }
            Some(1) => {}
            status => panic!("unexpected Git exit: {status:?}"),
        }
    }
    assert_eq!(
        git::ignored_directories(&root, &root, &directories).unwrap(),
        individual
    );
    assert!(individual.contains("line\nname"));
    assert!(!individual.contains("ignored-keep"));
    assert!(
        git::ignored_directories(&root, &root, &BTreeSet::from(["empty".into()]))
            .unwrap()
            .is_empty()
    );
    assert!(
        git::ignored_directories(&root, &root, &BTreeSet::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
#[cfg(target_os = "macos")]
fn cached_baseline_inventory_rejects_file_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(source.join("file"), "captured contents").unwrap();
    let root = open_dir(&source).unwrap();
    let frozen = inventory(&root, u64::MAX, deadline(), true).unwrap();
    fs::write(temp.path().join("replacement"), "captured contents").unwrap();
    fs::rename(temp.path().join("replacement"), source.join("file")).unwrap();

    let error = clone_selected(
        &root,
        &destination,
        &frozen,
        &BTreeSet::from(["file".into()]),
        deadline(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("source changed before clone"));
    assert!(!destination.join("file").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn capture_preserves_empty_and_untracked_directories_with_literal_names() {
    let (temp, root) = fixture();
    fs::write(root.join(".gitignore"), "ignored/\nmixed/*\n!mixed/keep/\n").unwrap();
    let included = [
        "empty",
        "nested/empty",
        "space name",
        "--literal-option",
        "tab\tname",
        "line\nname",
        "unicode-é",
        "mixed/keep",
    ];
    for name in included {
        fs::create_dir_all(root.join(name)).unwrap();
    }
    for name in [
        "ignored/empty",
        "ignored/line\nname",
        "mixed/excluded",
        "nested/excluded",
    ] {
        fs::create_dir_all(root.join(name)).unwrap();
    }
    fs::write(root.join("nested/.gitignore"), "excluded/\n").unwrap();
    fs::write(root.join("mixed/keep/untracked.txt"), "include me\n").unwrap();
    fs::write(root.join("ignored/untracked.txt"), "omit me\n").unwrap();
    fs::write(root.join("ignored/tracked.txt"), "tracked despite ignore\n").unwrap();
    git_command(&root, &["add", "--force", "ignored/tracked.txt"]);
    let original = manifest(&root).unwrap();

    let prepared = prepare(&root, &temp.path().join("run"), u64::MAX).unwrap();
    for worker in &prepared.workers {
        for name in included {
            assert!(worker.join(name).is_dir(), "missing directory {name:?}");
        }
        assert_eq!(
            fs::read(worker.join("mixed/keep/untracked.txt")).unwrap(),
            b"include me\n"
        );
        assert_eq!(
            fs::read(worker.join("ignored/tracked.txt")).unwrap(),
            b"tracked despite ignore\n"
        );
        for name in [
            "ignored/empty",
            "ignored/line\nname",
            "ignored/untracked.txt",
            "mixed/excluded",
            "nested/excluded",
        ] {
            assert!(
                !worker.join(name).exists(),
                "included ignored path {name:?}"
            );
            assert!(prepared.baseline_manifest.exclusions.contains_key(name));
        }
        assert_eq!(
            manifest(worker).unwrap().files,
            prepared.baseline_manifest.files
        );
    }
    assert_eq!(manifest(&root).unwrap(), original);
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "local preparation timing; no model calls"]
fn preparation_timing_with_many_ignored_directories() {
    let (temp, root) = fixture();
    fs::write(root.join(".gitignore"), "cache/\n").unwrap();
    fs::write(root.join("tracked.txt"), vec![b'x'; 4 * 1024 * 1024]).unwrap();
    for index in 0..400 {
        let directory = root.join(format!("cache/package-{index}"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("ignored.txt"), "ignored package data\n").unwrap();
    }
    for index in 0..40 {
        fs::create_dir(root.join(format!("empty-{index}"))).unwrap();
    }
    let mut elapsed = Vec::new();
    for index in 0..3 {
        let start = Instant::now();
        let result = prepare(&root, &temp.path().join(format!("run-{index}")), u64::MAX).unwrap();
        elapsed.push(start.elapsed());
        assert!(
            result
                .workers
                .iter()
                .all(|worker| !worker.join("cache").exists())
        );
        assert!(
            result
                .workers
                .iter()
                .all(|worker| worker.join("empty-39").is_dir())
        );
    }
    elapsed.sort();
    eprintln!(
        "preparation timing (400 ignored directories, 40 empty directories, 4 MiB saved file): {elapsed:?}; median {:?}",
        elapsed[1]
    );
}
