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
