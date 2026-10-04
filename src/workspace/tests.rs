use super::*;
use std::os::unix::fs::symlink;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project with spaces");
    fs::create_dir(&root).unwrap();
    let status = git::command()
        .args(["init", "--quiet", "--template="])
        .arg(&root)
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(root.join("hello.txt"), "committed\n").unwrap();
    fixture_git(&root, &["add", "hello.txt"]);
    fixture_git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "Fixture",
        ],
    );
    (temp, root)
}
fn fixture_git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = git::command()
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn scan(root: &Path) -> Inventory {
    inventory(&open_dir(root).unwrap(), u64::MAX, deadline(), true).unwrap()
}

#[test]
#[cfg(target_os = "macos")]
fn exact_metric_counts_every_path_sparse_hard_links_and_xattrs() {
    let (_temp, root) = fixture();
    fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir(root.join("ignored")).unwrap();
    let sparse = File::create(root.join("ignored/sparse")).unwrap();
    sparse.set_len(1024 * 1024).unwrap();
    fs::hard_link(root.join("ignored/sparse"), root.join(".hidden-hardlink")).unwrap();
    let xattr = CString::new("user.delm-fixture").unwrap();
    let value = b"attribute-value";
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                sparse.as_raw_fd(),
                xattr.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        },
        0
    );
    fn logical(path: &Path) -> u64 {
        let file = if fs::symlink_metadata(path).unwrap().is_dir() {
            open_dir(path).unwrap()
        } else {
            File::open(path).unwrap()
        };
        let m = file.metadata().unwrap();
        let mut total = m.len()
            + attributes(&file)
                .unwrap()
                .values()
                .map(|v| v.len() as u64)
                .sum::<u64>();
        if m.is_dir() {
            for e in fs::read_dir(path).unwrap() {
                total += logical(&e.unwrap().path());
            }
        }
        total
    }
    let measured = measure_repository(&root).unwrap();
    assert_eq!(measured, logical(&root));
    assert!(measured > 2 * 1024 * 1024);
    let root_fd = open_dir(&root).unwrap();
    assert!(inventory(&root_fd, measured + 1, deadline(), true).is_ok());
    assert!(inventory(&root_fd, measured, deadline(), true).is_err());
    assert!(inventory(&root_fd, measured - 1, deadline(), true).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn native_clones_are_independent_and_never_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::write(&source, "saved source").unwrap();
    clone_file(&source, &first).unwrap();
    clone_file(&source, &second).unwrap();
    assert_ne!(
        fs::metadata(&source).unwrap().ino(),
        fs::metadata(&first).unwrap().ino()
    );
    fs::write(&first, "worker changed").unwrap();
    assert_eq!(fs::read(&source).unwrap(), b"saved source");
    assert_eq!(fs::read(&second).unwrap(), b"saved source");
    assert!(clone_file(&source, &first).is_err());
    assert_eq!(fs::read(&first).unwrap(), b"worker changed");
    let link = temp.path().join("link");
    symlink(&source, &link).unwrap();
    assert!(clone_file(&link, &temp.path().join("bad")).is_err());
    assert!(!temp.path().join("bad").exists());
}

#[test]
fn board_paths_reject_traversal_git_and_symlink_components() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("normal")).unwrap();
    symlink("normal", temp.path().join("link")).unwrap();
    for path in [
        "../out",
        "/tmp/out",
        ".git/config",
        "normal/.GiT/config",
        "normal/./x",
        "normal//x",
        "link/file",
        "link",
        "",
    ] {
        assert!(safe_path(temp.path(), path).is_err(), "accepted {path}");
    }
    assert_eq!(
        safe_path(temp.path(), "normal/new.txt").unwrap(),
        temp.path().join("normal/new.txt")
    );
}

#[test]
#[cfg(target_os = "macos")]
fn preserves_dirty_index_history_tags_and_disables_source_customization() {
    let (temp, root) = fixture();
    fixture_git(&root, &["tag", "fixture-version"]);
    fs::write(root.join("hello.txt"), "staged\n").unwrap();
    fixture_git(&root, &["add", "hello.txt"]);
    fs::write(root.join("hello.txt"), "unstaged\n").unwrap();
    fs::write(root.join("untracked.txt"), "new file\n").unwrap();
    fs::write(root.join(".npmrc"), "save-exact=true\n").unwrap();
    fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir(root.join("ignored")).unwrap();
    fs::write(root.join("ignored/dependency"), "rebuild privately").unwrap();
    fs::write(root.join(".env"), "PRIVATE_TOKEN=fixture-only").unwrap();
    let marker = temp.path().join("sentinel-executed");
    let hook = root.join(".git/hooks/pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    fixture_git(
        &root,
        &[
            "config",
            "core.fsmonitor",
            &format!("touch '{}'", marker.display()),
        ],
    );
    fixture_git(
        &root,
        &[
            "config",
            "remote.origin.url",
            "https://fixture-token@example.invalid/private",
        ],
    );
    let original = scan(&root);
    let index = fs::read(root.join(".git/index")).unwrap();
    let prepared = prepare(&root, &temp.path().join("run"), 1_000_000).unwrap();
    assert_eq!(scan(&root), original);
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), index);
    assert!(!marker.exists());
    for private in [
        &prepared.baseline,
        &prepared.workers[0],
        &prepared.workers[1],
    ] {
        assert_eq!(fs::read(private.join("hello.txt")).unwrap(), b"unstaged\n");
        assert_eq!(fixture_git(private, &["show", ":hello.txt"]), b"staged\n");
        assert_eq!(
            fixture_git(private, &["rev-parse", "HEAD"]),
            fixture_git(&root, &["rev-parse", "HEAD"])
        );
        assert_eq!(
            fixture_git(private, &["describe", "--tags", "--always"]),
            b"fixture-version\n"
        );
        assert!(private.join("untracked.txt").exists());
        assert_eq!(
            fs::read(private.join(".npmrc")).unwrap(),
            b"save-exact=true\n"
        );
        assert!(!private.join("ignored").exists());
        assert!(!private.join(".env").exists());
        let config = fs::read_to_string(private.join(".git/config")).unwrap();
        assert!(!config.contains("fixture-token") && !config.contains("sentinel"));
        assert!(!private.join(".git/hooks").exists());
    }
    assert!(prepared.baseline_manifest.exclusions.contains_key(".env"));
    assert_eq!(
        manifest(&prepared.workers[0]).unwrap(),
        manifest(&prepared.workers[1]).unwrap()
    );
    fs::write(prepared.workers[0].join("hello.txt"), "one worker").unwrap();
    assert_eq!(
        fs::read(prepared.workers[1].join("hello.txt")).unwrap(),
        b"unstaged\n"
    );
    assert_eq!(scan(&root), original);
}

#[test]
#[cfg(target_os = "macos")]
fn retains_same_result_path_and_self_contained_changed_preimages() {
    let (temp, root) = fixture();
    let original = scan(&root);
    let prepared = prepare(&root, &temp.path().join("run"), 1_000_000).unwrap();
    fs::write(prepared.workers[0].join("hello.txt"), b"changed\0binary").unwrap();
    let expected = prepared.workers[0].clone();
    assert_eq!(retain_result(&prepared, 0).unwrap(), expected);
    assert_eq!(retain_result(&prepared, 0).unwrap(), expected);
    assert!(expected.exists());
    assert!(!prepared.baseline.exists());
    assert!(!prepared.workers[1].exists());
    let review = prepared.run_dir.join("workspace/review");
    assert_eq!(
        fs::read(review.join("before/hello.txt")).unwrap(),
        b"committed\n"
    );
    assert_eq!(
        fs::read(review.join("after/hello.txt")).unwrap(),
        b"changed\0binary"
    );
    assert_eq!(scan(&root), original);
    assert!(fixture_git(&expected, &["status", "--porcelain"]).starts_with(b" M hello.txt"));
}

#[test]
#[cfg(target_os = "macos")]
fn retains_real_python_venv_and_npm_with_all_outputs_recorded() {
    let (temp, root) = fixture();
    let original = scan(&root);
    let prepared = prepare(&root, &temp.path().join("run"), 1_000_000).unwrap();
    let worker = &prepared.workers[0];
    let python = Command::new("python3")
        .args(["-m", "venv", "--without-pip"])
        .arg(worker.join(".venv"))
        .output()
        .unwrap();
    assert!(
        python.status.success(),
        "{}",
        String::from_utf8_lossy(&python.stderr)
    );
    let interpreter = worker.join(".venv/bin/python3").canonicalize().unwrap();
    let policy = ResultPolicy {
        readonly_runtime_roots: vec![interpreter.parent().unwrap().to_path_buf()],
        denied_roots: vec![root.clone(), prepared.workers[1].clone()],
    };
    // Install a real local package without contacting a registry or user cache.
    let package = worker.join("fixture-package");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("package.json"), r#"{"name":"delm-retention-fixture","version":"1.0.0","main":"index.js","bin":{"delm-retention-fixture":"cli.js"}}"#).unwrap();
    fs::write(
        package.join("index.js"),
        "module.exports = 'retained dependency';\n",
    )
    .unwrap();
    fs::write(
        package.join("cli.js"),
        "#!/usr/bin/env node\nconsole.log(require('./index'));\n",
    )
    .unwrap();
    fs::set_permissions(package.join("cli.js"), fs::Permissions::from_mode(0o755)).unwrap();
    let npm_home = temp.path().join("npm-home");
    fs::create_dir(&npm_home).unwrap();
    fs::write(npm_home.join("npmrc"), "").unwrap();
    for args in [
        vec!["pack", "./fixture-package", "--ignore-scripts"],
        vec![
            "install",
            "./delm-retention-fixture-1.0.0.tgz",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--package-lock=false",
            "--offline",
        ],
    ] {
        let output = Command::new("npm")
            .args(args)
            .current_dir(worker)
            .env("HOME", &npm_home)
            .env("NPM_CONFIG_CACHE", npm_home.join("cache"))
            .env("NPM_CONFIG_USERCONFIG", npm_home.join("npmrc"))
            .output()
            .expect("retention fixture requires the installed npm toolchain");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(
        fs::symlink_metadata(worker.join("node_modules/.bin/delm-retention-fixture"))
            .unwrap()
            .is_symlink()
    );
    fs::write(worker.join(".gitignore"), ".venv/\nnode_modules/\ndist/\n").unwrap();
    fs::create_dir(worker.join("dist")).unwrap();
    fs::write(
        worker.join("dist/requested-image.png"),
        b"requested generated output",
    )
    .unwrap();
    fs::write(
        worker.join(".venv/user-notes.txt"),
        b"unknown user-authored file",
    )
    .unwrap();
    assert!(
        manifest(worker).is_err(),
        "source manifests must remain strict"
    );
    let before = manifest_for_result(worker, &policy).unwrap();
    assert!(!before.runtime_links.is_empty());
    assert!(before.files["dist/requested-image.png"].sha256.is_some());
    assert!(before.files[".venv/user-notes.txt"].sha256.is_some());
    assert!(
        before.files["node_modules/delm-retention-fixture/index.js"]
            .sha256
            .is_some()
    );
    assert_eq!(
        retain_result_with_policy(&prepared, 0, &policy).unwrap(),
        *worker
    );
    assert_eq!(
        retain_result_with_policy(&prepared, 0, &policy).unwrap(),
        *worker
    );
    assert_eq!(manifest_for_result(worker, &policy).unwrap(), before);
    assert_eq!(scan(&root), original);
    let output = Command::new(worker.join(".venv/bin/python"))
        .args([
            "-c",
            "import sys; assert sys.prefix != sys.base_prefix; print('retained venv')",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("node")
        .args(["-e", "console.log(require('delm-retention-fixture'))"])
        .current_dir(worker)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "retained dependency"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn result_interpreter_allowance_does_not_admit_source_or_fake_environment_links() {
    let (temp, root) = fixture();
    let runtime = temp.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    let interpreter = runtime.join("python3");
    fs::write(&interpreter, "fixture interpreter").unwrap();
    fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o755)).unwrap();
    let policy = ResultPolicy {
        readonly_runtime_roots: vec![runtime.clone()],
        denied_roots: vec![],
    };
    fs::create_dir_all(root.join(".venv/bin")).unwrap();
    symlink(&interpreter, root.join(".venv/bin/python3")).unwrap();
    let config = format!(
        "home = {}\ninclude-system-site-packages = false\nversion = 3.9.6\n",
        runtime.display()
    );
    fs::write(root.join(".venv/pyvenv.cfg"), &config).unwrap();
    let captured = manifest_for_result(&root, &policy).unwrap();
    fs::write(&interpreter, "changed interpreter").unwrap();
    assert_ne!(manifest_for_result(&root, &policy).unwrap(), captured);
    let denied = ResultPolicy {
        readonly_runtime_roots: vec![runtime.clone()],
        denied_roots: vec![runtime.clone()],
    };
    assert!(manifest_for_result(&root, &denied).is_err());
    let original_private = temp.path().join("original-private");
    fs::create_dir(&original_private).unwrap();
    symlink(&interpreter, original_private.join("python3")).unwrap();
    fs::remove_file(root.join(".venv/bin/python3")).unwrap();
    symlink(
        original_private.join("python3"),
        root.join(".venv/bin/python3"),
    )
    .unwrap();
    let denied_route = ResultPolicy {
        readonly_runtime_roots: vec![runtime.clone()],
        denied_roots: vec![original_private],
    };
    assert!(manifest_for_result(&root, &denied_route).is_err());
    fs::remove_file(root.join(".venv/bin/python3")).unwrap();
    symlink(&interpreter, root.join(".venv/bin/python3")).unwrap();
    symlink(&interpreter, root.join("source-link")).unwrap();
    assert!(manifest_for_result(&root, &policy).is_err());
    fs::remove_file(root.join("source-link")).unwrap();
    symlink(".venv/bin/python3", root.join("source-link")).unwrap();
    assert!(manifest_for_result(&root, &policy).is_err());
    fs::remove_file(root.join("source-link")).unwrap();
    fs::remove_file(root.join(".venv/pyvenv.cfg")).unwrap();
    fs::write(root.join("venv-configuration.cfg"), config).unwrap();
    symlink("../venv-configuration.cfg", root.join(".venv/pyvenv.cfg")).unwrap();
    assert!(manifest_for_result(&root, &policy).is_err());
    assert!(manifest(&root).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn exact_root_selection_unsupported_state_and_size_failure_create_no_workers() {
    let (temp, root) = fixture();
    let before = scan(&root);
    assert!(prepare(&root, &root.join("forbidden-run"), 1_000_000).is_err());
    assert!(!root.join("forbidden-run").exists());
    assert_eq!(scan(&root), before);
    let size = measure_repository(&root).unwrap();
    assert!(prepare(&root, &temp.path().join("run-sized"), size).is_err());
    assert!(!temp.path().join("run-sized").exists());
    fs::write(root.join(".git/index.lock"), "fixture lock").unwrap();
    assert!(prepare(&root, &temp.path().join("run-locked"), 1_000_000).is_err());
    assert!(!temp.path().join("run-locked/workspace/worker-1").exists());
    assert!(root.join(".git/index.lock").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn internal_links_survive_external_links_and_special_files_fail_closed() {
    let (temp, root) = fixture();
    symlink("hello.txt", root.join("relative-link")).unwrap();
    let prepared = prepare(&root, &temp.path().join("run"), 1_000_000).unwrap();
    assert_eq!(
        fs::read_link(prepared.workers[0].join("relative-link")).unwrap(),
        PathBuf::from("hello.txt")
    );
    symlink("../../external", root.join("escape")).unwrap();
    assert!(prepare(&root, &temp.path().join("bad-run"), 1_000_000).is_err());
    fs::remove_file(root.join("escape")).unwrap();
    let fifo = CString::new(root.join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(measure_repository(&root).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn ignored_environment_links_are_counted_without_admitting_their_targets() {
    let (temp, root) = fixture();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("interpreter"), "outside must remain untouched").unwrap();
    fs::write(root.join(".gitignore"), ".venv/\n").unwrap();
    fs::create_dir_all(root.join(".venv/bin")).unwrap();
    symlink(outside.join("interpreter"), root.join(".venv/bin/python")).unwrap();
    symlink("../../../../absent", root.join(".venv/bin/absent")).unwrap();
    let before = scan(&root);
    let outside_before = fs::read(outside.join("interpreter")).unwrap();
    let measured = measure_repository(&root).unwrap();
    assert!(
        measured
            > fs::symlink_metadata(root.join(".venv/bin/python"))
                .unwrap()
                .len()
    );
    assert!(prepare(&root, &temp.path().join("at-limit"), measured).is_err());
    let prepared = prepare(&root, &temp.path().join("run"), measured + 1).unwrap();
    for worker in &prepared.workers {
        assert!(!worker.join(".venv").exists());
    }
    assert!(
        prepared
            .baseline_manifest
            .exclusions
            .contains_key(".venv/bin/python")
    );
    assert_eq!(scan(&root), before);
    assert_eq!(
        fs::read(outside.join("interpreter")).unwrap(),
        outside_before
    );

    // The same target is still rejected when it is an actual source input.
    symlink(outside.join("interpreter"), root.join("source-link")).unwrap();
    assert!(prepare(&root, &temp.path().join("admitted-link"), u64::MAX).is_err());
    assert!(manifest(&root).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn stale_capture_inventory_cannot_publish_changed_source() {
    let (temp, root) = fixture();
    let source = open_dir(&root).unwrap();
    let initial = scan(&root);
    fs::write(root.join("hello.txt"), "changed after admission").unwrap();
    let baseline = temp.path().join("incomplete");
    fs::create_dir(&baseline).unwrap();
    assert!(prepare::capture(&source, &root, &baseline, &initial, 1_000_000, deadline()).is_err());
    assert!(!temp.path().join("worker-1").exists());
    assert_eq!(
        fs::read(root.join("hello.txt")).unwrap(),
        b"changed after admission"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn ownership_replacement_prevents_cleanup_of_unowned_data() {
    let (temp, root) = fixture();
    let prepared = prepare(&root, &temp.path().join("run"), 1_000_000).unwrap();
    let saved = temp.path().join("saved-loser");
    fs::rename(&prepared.workers[1], &saved).unwrap();
    fs::create_dir(&prepared.workers[1]).unwrap();
    fs::write(prepared.workers[1].join("unique"), "must survive").unwrap();
    assert!(retain_result(&prepared, 0).is_err());
    assert!(prepared.workers[0].exists() && prepared.baseline.exists());
    assert_eq!(
        fs::read(prepared.workers[1].join("unique")).unwrap(),
        b"must survive"
    );
    assert!(saved.exists());
}

#[test]
#[cfg(target_os = "macos")]
fn retained_git_cannot_depend_on_scratch_or_original_administration() {
    let (temp, root) = fixture();
    let prepared = prepare(&root, &temp.path().join("run"), 1_000_000).unwrap();
    let original_git = prepared.workers[0].join(".git");
    fs::rename(&original_git, prepared.workers[0].join("saved-git")).unwrap();
    symlink(prepared.baseline.join(".git"), &original_git).unwrap();
    assert!(retain_result(&prepared, 0).is_err());
    assert!(prepared.baseline.exists() && prepared.workers[1].exists());
    assert!(
        !prepared
            .run_dir
            .join("workspace/retained-result.json")
            .exists()
    );
}

#[test]
#[cfg(target_os = "macos")]
fn unborn_absent_index_detached_head_and_packed_history_survive() {
    let temp = tempfile::tempdir().unwrap();
    let unborn = temp.path().join("unborn");
    assert!(
        git::command()
            .args(["init", "--quiet", "--template="])
            .arg(&unborn)
            .status()
            .unwrap()
            .success()
    );
    fs::write(unborn.join("new.txt"), "new").unwrap();
    let measured = measure_repository(&unborn).unwrap();
    assert!(prepare(&unborn, &temp.path().join("at-limit"), measured).is_err());
    assert!(prepare(&unborn, &temp.path().join("over-limit"), measured - 1).is_err());
    let prepared = prepare(&unborn, &temp.path().join("run-unborn"), measured + 1).unwrap();
    assert!(!prepared.workers[0].join(".git/index").exists());
    assert_eq!(
        fs::read(prepared.workers[0].join(".git/HEAD")).unwrap(),
        fs::read(unborn.join(".git/HEAD")).unwrap()
    );
    let (temp, root) = fixture();
    fixture_git(&root, &["tag", "packed-tag"]);
    fixture_git(&root, &["pack-refs", "--all"]);
    fixture_git(&root, &["repack", "-ad"]);
    fixture_git(&root, &["checkout", "--quiet", "--detach"]);
    let original = scan(&root);
    let prepared = prepare(&root, &temp.path().join("run-packed"), 1_000_000).unwrap();
    assert_eq!(
        fixture_git(&prepared.workers[0], &["describe", "--tags", "--always"]),
        b"packed-tag\n"
    );
    assert_eq!(
        fs::read(prepared.workers[0].join(".git/HEAD")).unwrap(),
        fs::read(root.join(".git/HEAD")).unwrap()
    );
    assert_eq!(scan(&root), original);
}

#[test]
#[cfg(target_os = "macos")]
fn replacement_race_is_detected_without_following_external_link() {
    let (temp, root) = fixture();
    let external = temp.path().join("external-file");
    fs::write(&external, "outside original").unwrap();
    let outside_before = fs::read(&external).unwrap();
    let initial = scan(&root);
    fs::remove_file(root.join("hello.txt")).unwrap();
    symlink(&external, root.join("hello.txt")).unwrap();
    let baseline = temp.path().join("incomplete");
    fs::create_dir(&baseline).unwrap();
    assert!(
        prepare::capture(
            &open_dir(&root).unwrap(),
            &root,
            &baseline,
            &initial,
            1_000_000,
            deadline()
        )
        .is_err()
    );
    assert_eq!(fs::read(&external).unwrap(), outside_before);
    assert!(!baseline.join("hello.txt").exists());
}

#[test]
#[cfg(target_os = "macos")]
fn unsupported_git_administration_is_never_repaired() {
    for state in [
        "objects/info/alternates",
        "shallow",
        "MERGE_HEAD",
        "refs/replace/fake",
        "rebase-merge/state",
        "sharedindex.fake",
    ] {
        let (temp, root) = fixture();
        let unsupported = root.join(".git").join(state);
        fs::create_dir_all(unsupported.parent().unwrap()).unwrap();
        fs::write(&unsupported, "fixture").unwrap();
        let before = scan(&root);
        assert!(
            prepare(&root, &temp.path().join("run"), 1_000_000).is_err(),
            "admitted {state}"
        );
        assert_eq!(scan(&root), before);
        assert!(!temp.path().join("run/workspace/worker-1").exists());
    }
    let (temp, root) = fixture();
    fs::write(root.join(".gitattributes"), "*.txt filter=lfs\n").unwrap();
    assert!(prepare(&root, &temp.path().join("run"), 1_000_000).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn result_retains_cpython_314_pi_alias_without_widening_source_admission() {
    let (temp, root) = fixture();
    let runtime = temp.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    let interpreter = runtime.join("python3.14");
    fs::write(&interpreter, "qualified interpreter").unwrap();
    fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(root.join(".venv/bin")).unwrap();
    fs::write(
        root.join(".venv/pyvenv.cfg"),
        format!(
            "home = {}\ninclude-system-site-packages = false\nversion = 3.14.0\n",
            runtime.display()
        ),
    )
    .unwrap();
    symlink(&interpreter, root.join(".venv/bin/python3.14")).unwrap();
    symlink("python3.14", root.join(".venv/bin/python")).unwrap();
    // CPython 3.14's UTF-8 POSIX venv adds this exact same-bin alias.
    symlink("python3.14", root.join(".venv/bin/𝜋thon")).unwrap();
    let policy = ResultPolicy {
        readonly_runtime_roots: vec![runtime],
        denied_roots: vec![],
    };
    assert!(manifest(&root).is_err());
    let captured = manifest_for_result(&root, &policy).unwrap();
    assert_eq!(captured.files[".venv/bin/𝜋thon"].kind, FileKind::Symlink);
    assert_eq!(
        captured.runtime_links[".venv/bin/𝜋thon"],
        captured.runtime_links[".venv/bin/python3.14"]
    );
    assert_eq!(
        captured.runtime_links[".venv/bin/𝜋thon"].target,
        interpreter.canonicalize().unwrap()
    );

    // The new spelling cannot grant access through a denied source/peer route.
    let alias = root.join(".venv/bin/𝜋thon");
    for name in ["original", "peer"] {
        let denied = temp.path().join(name);
        fs::create_dir(&denied).unwrap();
        symlink(&interpreter, denied.join("python3.14")).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(denied.join("python3.14"), &alias).unwrap();
        let restricted = ResultPolicy {
            readonly_runtime_roots: policy.readonly_runtime_roots.clone(),
            denied_roots: vec![denied],
        };
        assert!(manifest_for_result(&root, &restricted).is_err());
    }
    fs::remove_file(&alias).unwrap();
    symlink("python3.14", &alias).unwrap();
    let denied = ResultPolicy {
        readonly_runtime_roots: policy.readonly_runtime_roots.clone(),
        denied_roots: policy.readonly_runtime_roots.clone(),
    };
    assert!(manifest_for_result(&root, &denied).is_err());
    assert!(manifest_for_result(&root, &ResultPolicy::default()).is_err());

    // Similar spellings and arbitrary tools remain ordinary external links.
    for name in ["πthon", "𝜋thon3", "pip", "source-link"] {
        let unknown = root.join(".venv/bin").join(name);
        symlink("python3.14", &unknown).unwrap();
        assert!(manifest_for_result(&root, &policy).is_err(), "{name}");
        fs::remove_file(unknown).unwrap();
    }
    symlink(".venv/bin/𝜋thon", root.join("source-link")).unwrap();
    assert!(manifest_for_result(&root, &policy).is_err());
    fs::remove_file(root.join("source-link")).unwrap();

    let unrelated = policy.readonly_runtime_roots[0].join("unrelated-tool");
    fs::write(&unrelated, "not a Python interpreter").unwrap();
    fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&alias).unwrap();
    symlink(&unrelated, &alias).unwrap();
    assert!(manifest_for_result(&root, &policy).is_err());
    fs::remove_file(&alias).unwrap();
    symlink("python3.14", &alias).unwrap();

    fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(manifest_for_result(&root, &policy).is_err());
    fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(root.join(".venv/pyvenv.cfg")).unwrap();
    assert!(manifest_for_result(&root, &policy).is_err());
}
