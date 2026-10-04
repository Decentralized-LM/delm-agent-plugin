//! Support commands use isolated storage and never launch a model.
#![cfg(target_os = "macos")]
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture {
    _directory: tempfile::TempDir,
    home: PathBuf,
    run: PathBuf,
    id: String,
}
impl Fixture {
    fn new(host: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let id = uuid::Uuid::new_v4().to_string();
        let run = home.join("Library/Application Support/DeLM/runs").join(&id);
        let project = home.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(run.join("workspace/delivery")).unwrap();
        fs::set_permissions(
            home.join("Library/Application Support/DeLM"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let project = project.canonicalize().unwrap();
        fs::write(project.join("source.js"), "original").unwrap();
        let mut child = Command::new("/bin/sleep").arg("10").spawn().unwrap();
        let runtime = delm::supervisor::ProcessIdentity::capture(child.id()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        fs::write(
            run.join(if host == "claude" {
                "claude.json"
            } else {
                "run.json"
            }),
            json!({
            "status":"complete","finished":true,"runtime":runtime,"project":project,
            "workspace":{"original":project}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            run.join("watchdog.json"),
            json!({"runtime":runtime}).to_string(),
        )
        .unwrap();
        fs::write(
            run.join("shutdown-report.json"),
            json!({"ownership_resolved":true,"survivors":[],"errors":[]}).to_string(),
        )
        .unwrap();
        fs::write(run.join("workspace/delivery/result.json"),json!({"delivered":true,
            "cleanup_complete":true,"verification_required":false,"recovery":run.join("workspace/delivery")}).to_string()).unwrap();
        fs::write(
            run.join("workspace/delivery/previous-0"),
            "recoverable original",
        )
        .unwrap();
        fs::write(
            run.join("events.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"time_ms":100,"kind":"preparation_started","data":{}}),
                json!({"time_ms":135,"kind":"workspaces_prepared","data":{}})
            ),
        )
        .unwrap();
        fs::write(run.join("completion.json"), "{}").unwrap();
        Self {
            _directory: directory,
            home,
            run,
            id,
        }
    }
    fn command(&self, command: &str) -> Command {
        let mut cli = Command::new(env!("CARGO_BIN_EXE_delm"));
        cli.env("HOME", &self.home).arg(command);
        if command != "runs" {
            cli.args(["--run-id", &self.id]);
        }
        cli
    }
    fn json(output: Output) -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn deliberate_cleanup_preserves_delivery_recovery_and_timing_for_both_hosts() {
    for host in ["codex", "claude"] {
        let fixture = Fixture::new(host);
        let preview = Fixture::json(fixture.command("clean").output().unwrap());
        assert_eq!(preview["removed"], false);
        assert!(fixture.run.join("events.jsonl").exists());
        let cleaned = Fixture::json(fixture.command("clean").arg("--confirm").output().unwrap());
        assert_eq!(cleaned["removed"], true);
        assert!(!fixture.run.join("events.jsonl").exists());
        assert!(fixture.run.join("completion.json").exists());
        assert!(fixture.run.join("workspace/delivery/result.json").exists());
        assert_eq!(
            fs::read_to_string(fixture.run.join("workspace/delivery/previous-0")).unwrap(),
            "recoverable original"
        );
        assert_eq!(
            fs::read_to_string(fixture.home.join("project/source.js")).unwrap(),
            "original"
        );
        let report = Fixture::json(fixture.command("report").output().unwrap());
        assert_eq!(report["timing"]["phases"]["preparation"], 35);
        assert_eq!(report["timing"]["retained_after_diagnostic_cleanup"], true);
        Fixture::json(fixture.command("clean").arg("--confirm").output().unwrap());
        let report = Fixture::json(fixture.command("report").output().unwrap());
        assert_eq!(report["timing"]["phases"]["preparation"], 35);
    }
}
#[test]
fn running_runtime_and_unknown_ownership_block_even_terminal_record_cleanup() {
    let fixture = Fixture::new("claude");
    let path = fixture.run.join("claude.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["runtime"] = serde_json::to_value(
        delm::supervisor::ProcessIdentity::capture(std::process::id()).unwrap(),
    )
    .unwrap();
    fs::write(&path, state.to_string()).unwrap();
    let output = fixture.command("clean").arg("--confirm").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("runtime_still_running"));
    assert!(fixture.run.join("events.jsonl").exists());
    state["runtime"] = Value::Null;
    fs::write(&path, state.to_string()).unwrap();
    assert!(
        !fixture
            .command("clean")
            .arg("--confirm")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(fixture.run.join("events.jsonl").exists());
}
#[test]
fn new_installation_listing_does_not_create_storage_and_export_never_overwrites() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_delm"))
        .env("HOME", directory.path())
        .args(["runs", "--json"])
        .output()
        .unwrap();
    assert_eq!(Fixture::json(output), json!([]));
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    let fixture = Fixture::new("codex");
    let output = fixture.home.join("diagnostic.json");
    assert!(
        fixture
            .command("report")
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap()
            .status
            .success()
    );
    let bytes = fs::read(&output).unwrap();
    assert!(
        !fixture
            .command("report")
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&output).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
