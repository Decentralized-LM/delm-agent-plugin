//! Exercise the public skill CLI with a fake stock Codex host, never a model.
#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

const SHORT_LIMIT: Duration = Duration::from_secs(15);

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    fn walk(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let data = if metadata.is_file() {
            fs::read(path).unwrap()
        } else if metadata.file_type().is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        result.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            (metadata.permissions().mode(), data),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                walk(root, &entry.unwrap().path(), result);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
    host: PathBuf,
    before: BTreeMap<PathBuf, (u32, Vec<u8>)>,
}

impl Fixture {
    fn new(mode: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("original");
        let bin = root.join("bin");
        for path in [&home.join(".codex"), &project, &bin] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(project.join("source.txt"), "unchanged source\n").unwrap();
        let init = Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["init", "-q", "--template="])
            .arg(&project)
            .output()
            .unwrap();
        assert!(init.status.success(), "{init:?}");
        let host = bin.join("codex");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/worker_host.py"),
            &host,
        )
        .unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            host.with_extension("json"),
            json!({"mode":mode}).to_string(),
        )
        .unwrap();
        fs::write(
            root.join("task.txt"),
            "Create result.txt in the private project.",
        )
        .unwrap();
        fs::write(
            root.join("context.txt"),
            "Preserve the original project. Context marker: CLI-CONTEXT.",
        )
        .unwrap();
        let before = snapshot(&project);
        Self {
            _temporary: temporary,
            root,
            home,
            project,
            host,
            before,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_delm"));
        let path = std::env::join_paths([
            self.root.join("bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
            PathBuf::from("/usr/sbin"),
            PathBuf::from("/sbin"),
        ])
        .unwrap();
        command
            .env("HOME", &self.home)
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("PATH", path)
            .env_remove("CODEX_THREAD_ID")
            .current_dir(&self.root);
        command
    }

    fn start(&self) -> Session {
        self.start_with_seconds(180)
    }

    fn start_with_seconds(&self, seconds: u64) -> Session {
        let stderr = self.root.join("runtime.stderr");
        let mut child = self
            .command()
            .arg("run")
            .arg("--project")
            .arg(&self.project)
            .arg("--task-file")
            .arg(self.root.join("task.txt"))
            .arg("--context-file")
            .arg(self.root.join("context.txt"))
            .args(["--seconds", &seconds.to_string()])
            .args(if self.root.join("inputs.json").exists() {
                vec![
                    "--inputs-file".to_owned(),
                    self.root.join("inputs.json").to_string_lossy().into_owned(),
                ]
            } else {
                Vec::new()
            })
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        Session {
            child,
            stdout,
            pending: Vec::new(),
            events: Vec::new(),
            stderr,
        }
    }

    fn cli(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn control(&self, arguments: &[&str]) -> Value {
        let output = self.cli(arguments);
        assert!(output.status.success(), "CLI command failed: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn run_dir(&self, id: &str) -> PathBuf {
        self.home
            .join("Library/Application Support/DeLM/runs")
            .join(id)
    }

    fn wire(&self) -> Vec<Value> {
        fs::read_to_string(self.host.with_extension("jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn requests(&self, method: &str) -> Vec<Value> {
        self.wire()
            .into_iter()
            .filter(|entry| entry["direction"] == "in" && entry["message"]["method"] == method)
            // Qualification creates an ephemeral metadata-only thread. Count
            // the two task workers separately from that no-model probe.
            .filter(|entry| entry["message"]["params"]["ephemeral"] != true)
            .map(|entry| entry["message"].clone())
            .collect()
    }

    fn assert_preserved_and_stopped(&self) {
        assert_eq!(
            snapshot(&self.project),
            self.before,
            "original repository changed"
        );
        let pids = self
            .wire()
            .iter()
            .filter_map(|entry| entry["pid"].as_i64())
            .collect::<BTreeSet<_>>();
        assert!(!pids.is_empty());
        for pid in pids {
            assert_eq!(
                unsafe { libc::kill(pid as libc::pid_t, 0) },
                -1,
                "fixture host {pid} survived"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
}

struct Session {
    child: Child,
    stdout: ChildStdout,
    pending: Vec<u8>,
    events: Vec<Value>,
    stderr: PathBuf,
}

impl Session {
    fn pump(&mut self) {
        let mut bytes = [0; 8192];
        loop {
            match self.stdout.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => self.pending.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("read CLI output: {error}"),
            }
        }
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            self.events.push(serde_json::from_slice(&line).unwrap());
        }
    }

    fn until(&mut self, predicate: impl Fn(&Value) -> bool, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if let Some(event) = self.events.iter().find(|event| predicate(event)) {
                return event.clone();
            }
            if self.child.try_wait().unwrap().is_some() {
                // The process may have written its final event between the first
                // nonblocking read and observing exit. Drain once more first.
                self.pump();
                if let Some(event) = self.events.iter().find(|event| predicate(event)) {
                    return event.clone();
                }
                panic!(
                    "CLI exited before expected event: {:?}; {}",
                    self.events,
                    fs::read_to_string(&self.stderr).unwrap_or_default()
                );
            }
            assert!(
                Instant::now() < deadline,
                "CLI event timed out: {:?}; {}",
                self.events,
                fs::read_to_string(&self.stderr).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn run_id(&mut self) -> String {
        self.until(|event| event["run_id"].is_string(), SHORT_LIMIT)["run_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if let Some(status) = self.child.try_wait().unwrap() {
                self.pump();
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "CLI did not exit after completion: {:?}; {}",
                self.events,
                fs::read_to_string(&self.stderr).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.child.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn raw_control(id: &str, request: Value) -> Value {
    let socket = Path::new("/tmp")
        .canonicalize()
        .unwrap()
        .join(format!("delm-{}", unsafe { libc::getuid() }))
        .join(format!("{id}.sock"));
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn missing_capability_or_failed_isolation_stops_before_task_workers() {
    for mode in ["missing_compatibility_method", "failed_compatibility_probe"] {
        let fixture = Fixture::new(mode);
        let mut session = fixture.start();
        assert!(!session.wait(SHORT_LIMIT).success());
        let error = fs::read_to_string(&session.stderr).unwrap();
        assert!(
            error.contains("did not pass DeLM compatibility checks"),
            "{error}"
        );
        assert!(fixture.requests("thread/start").is_empty());
        assert!(fixture.requests("turn/start").is_empty());
        fixture.assert_preserved_and_stopped();
    }
}

#[test]
fn control_access_must_be_confirmed_before_task_workers_start() {
    let fixture = Fixture::new("complete");
    let mut session = fixture.start();
    let id = session.run_id();
    session.until(|event| event["type"] == "awaiting_control", SHORT_LIMIT);
    assert!(fixture.requests("thread/start").is_empty());
    assert!(fixture.requests("turn/start").is_empty());
    assert_eq!(
        fixture.control(&["status", "--run-id", &id])["status"],
        "awaiting_control"
    );
    assert!(
        raw_control(
            &id,
            json!({"token":"wrong","type":"status","keep_alive":true})
        )["error"]
            .is_string()
    );
    assert!(fixture.requests("turn/start").is_empty());
    let saved: Value =
        serde_json::from_slice(&fs::read(fixture.run_dir(&id).join("run.json")).unwrap()).unwrap();
    assert_eq!(
        saved["expires"], 0,
        "execution deadline started before admission"
    );

    let update = fixture.root.join("before-start.txt");
    fs::write(&update, "PRESTART-UPDATE: keep the source untouched.").unwrap();
    fixture.control(&[
        "update",
        "--run-id",
        &id,
        "--message-file",
        update.to_str().unwrap(),
    ]);
    assert!(fixture.requests("turn/start").is_empty());
    fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    session.until(|event| event["type"] == "result", SHORT_LIMIT);
    assert!(session.wait(SHORT_LIMIT).success());
    assert_eq!(fixture.requests("thread/start").len(), 2);
    assert!(
        fixture
            .requests("turn/start")
            .iter()
            .all(|request| request.to_string().contains("PRESTART-UPDATE"))
    );
    fixture.assert_preserved_and_stopped();
}

#[test]
fn stop_before_control_confirmation_never_starts_task_workers() {
    let fixture = Fixture::new("complete");
    let mut session = fixture.start();
    let id = session.run_id();
    session.until(|event| event["type"] == "awaiting_control", SHORT_LIMIT);
    fixture.control(&["stop", "--run-id", &id]);
    session.until(|event| event["type"] == "stopped", SHORT_LIMIT);
    assert!(session.wait(SHORT_LIMIT).success());
    assert!(fixture.requests("thread/start").is_empty());
    assert!(fixture.requests("turn/start").is_empty());
    fixture.assert_preserved_and_stopped();
}

#[test]
fn public_run_automatically_accepts_and_exits_with_a_retained_result() {
    let fixture = Fixture::new("complete");
    let mut session = fixture.start();
    let id = session.run_id();
    let mut status = fixture.control(&[
        "status",
        "--run-id",
        &id,
        "--keep-alive",
        "--wait-seconds",
        "15",
    ]);
    let deadline = Instant::now() + SHORT_LIMIT;
    while status["status"] != "complete" && Instant::now() < deadline {
        let sequence = status["update_sequence"].as_u64().unwrap();
        status = fixture.control(&[
            "status",
            "--run-id",
            &id,
            "--after",
            &sequence.to_string(),
            "--wait-seconds",
            "15",
        ]);
        assert!(status["update_sequence"].as_u64().unwrap() >= sequence);
    }
    assert_eq!(
        status["status"], "complete",
        "pending status did not receive the terminal outcome"
    );
    let result = session.until(|event| event["type"] == "result", SHORT_LIMIT);
    assert!(session.wait(SHORT_LIMIT).success());
    let path = PathBuf::from(result["path"].as_str().unwrap());
    assert!(path.join("result.txt").is_file());
    assert_ne!(path, fixture.project);
    assert_eq!(fixture.requests("thread/start").len(), 2);
    assert_eq!(fixture.requests("turn/start").len(), 2);
    assert_eq!(fixture.requests("command/exec").len(), 1);
    let wire = fixture.wire();
    let isolation_check = wire
        .iter()
        .position(|entry| entry["message"]["method"] == "command/exec")
        .unwrap();
    let first_turn = wire
        .iter()
        .position(|entry| entry["message"]["method"] == "turn/start")
        .unwrap();
    assert!(isolation_check < first_turn);
    let status = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(status["status"], "complete");
    assert_eq!(status["run_id"], id);
    assert_eq!(status["path"], result["path"]);
    assert_eq!(fixture.control(&["stop", "--run-id", &id]), status);
    let token = fs::read_to_string(fixture.run_dir(&id).join("control-token")).unwrap();
    assert_eq!(token.len(), 64);
    assert!(
        !serde_json::to_string(&session.events)
            .unwrap()
            .contains(&token)
    );
    assert!(
        !serde_json::to_string(&fixture.wire())
            .unwrap()
            .contains(&token)
    );
    assert!(
        fixture
            .requests("turn/start")
            .iter()
            .all(|request| request.to_string().contains("CLI-CONTEXT"))
    );
    fixture.assert_preserved_and_stopped();
}

#[test]
fn execution_time_limit_preserves_clean_partial_work() {
    let fixture = Fixture::new("wait");
    let mut session = fixture.start_with_seconds(3);
    let id = session.run_id();
    fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    session.until(|event| event["type"] == "started", SHORT_LIMIT);
    let stopped = session.until(|event| event["type"] == "stopped", SHORT_LIMIT);
    assert!(session.wait(SHORT_LIMIT).success());
    assert!(
        stopped["message"]
            .as_str()
            .unwrap()
            .contains("execution time limit"),
        "{stopped}"
    );
    assert_eq!(stopped["partial_paths"].as_array().unwrap().len(), 2);
    let run_dir = fixture.run_dir(&id);
    let saved: Value =
        serde_json::from_slice(&fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(saved["status"], "paused");
    let launches = fs::read_dir(run_dir.join("launches"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(launches.len(), 1);
    let report: Value =
        serde_json::from_slice(&fs::read(launches[0].join("shutdown-report.json")).unwrap())
            .unwrap();
    assert_eq!(report["reason"], "shutdown complete", "{report}");
    assert_eq!(report["ownership_resolved"], true, "{report}");
    assert_eq!(report["survivors"], json!([]), "{report}");
    assert_eq!(report["errors"], json!([]), "{report}");
    let spec: Value =
        serde_json::from_slice(&fs::read(launches[0].join("watchdog.json")).unwrap()).unwrap();
    assert_eq!(
        spec["deadline_unix_ms"].as_u64().unwrap(),
        saved["expires"].as_u64().unwrap() * 1000 + 2000
    );
    for worker in saved["workers"].as_array().unwrap() {
        for method in [
            "turn/interrupt",
            "thread/backgroundTerminals/clean",
            "thread/archive",
        ] {
            assert_eq!(
                fixture
                    .requests(method)
                    .iter()
                    .filter(|request| { request["params"]["threadId"] == worker["thread"] })
                    .count(),
                1,
                "missing native {method} acknowledgment for {worker}"
            );
        }
    }
    let status = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(status["status"], "stopped");
    assert_eq!(status["partial_paths"], stopped["partial_paths"]);
    fixture.assert_preserved_and_stopped();
}

#[test]
fn authenticated_update_and_stop_work_while_untrusted_controls_have_no_effect() {
    let fixture = Fixture::new("wait");
    let mut session = fixture.start();
    let id = session.run_id();
    fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    session.until(|event| event["type"] == "started", SHORT_LIMIT);
    let initial = fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    assert_eq!(initial["status"], "running");
    for token in [None, Some("wrong-token")] {
        for mut command in [
            json!({"type":"stop"}),
            json!({"type":"update","text":"UNAUTHORIZED-UPDATE"}),
            json!({"type":"status","keep_alive":true}),
        ] {
            if let Some(token) = token {
                command["token"] = json!(token);
            }
            assert!(raw_control(&id, command)["error"].is_string());
        }
    }
    assert!(session.child.try_wait().unwrap().is_none());
    let unchanged = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(unchanged["status"], "running");
    assert_eq!(unchanged["request_revision"], 1);
    assert!(fixture.requests("turn/steer").is_empty());
    let update = fixture.root.join("update.txt");
    fs::write(&update, "AUTHORIZED-UPDATE: retain progress.").unwrap();
    fixture.control(&[
        "update",
        "--run-id",
        &id,
        "--message-file",
        update.to_str().unwrap(),
    ]);
    session.until(|event| event["request_revision"] == 2, SHORT_LIMIT);
    let deadline = Instant::now() + SHORT_LIMIT;
    while fixture.requests("turn/steer").len() != 2 {
        assert!(
            Instant::now() < deadline,
            "update was not sent to both workers"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        fixture
            .requests("turn/steer")
            .iter()
            .all(|request| request.to_string().contains("AUTHORIZED-UPDATE"))
    );
    assert!(
        !fixture
            .wire()
            .iter()
            .any(|entry| entry.to_string().contains("UNAUTHORIZED-UPDATE"))
    );
    fixture.control(&["stop", "--run-id", &id]);
    let stopped = session.until(|event| event["type"] == "stopped", SHORT_LIMIT);
    assert_eq!(stopped["partial_paths"].as_array().unwrap().len(), 2);
    assert!(session.wait(SHORT_LIMIT).success());
    let status = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(status["status"], "stopped");
    assert_eq!(status["request_revision"], 2);
    assert_eq!(status["partial_paths"], stopped["partial_paths"]);
    fixture.assert_preserved_and_stopped();
}

#[test]
fn monitoring_expires_after_sixty_seconds_despite_untrusted_renewals() {
    let fixture = Fixture::new("wait");
    let mut session = fixture.start();
    let id = session.run_id();
    fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    session.until(|event| event["type"] == "started", SHORT_LIMIT);
    let status = fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    assert_eq!(status["monitoring_lease_seconds"], 60);
    let last_authorized_contact = Instant::now();
    let turns_before = fixture.requests("turn/start").len();
    assert_eq!(turns_before, 2);
    let mut next_probe = Duration::from_secs(35);
    while session.child.try_wait().unwrap().is_none() {
        session.pump();
        let elapsed = last_authorized_contact.elapsed();
        assert!(
            elapsed < Duration::from_secs(90),
            "monitoring lease did not stop the run"
        );
        if elapsed >= next_probe && elapsed < Duration::from_secs(55) {
            for command in [
                json!({"type":"status","keep_alive":true}),
                json!({"token":"wrong-token","type":"status","keep_alive":true}),
            ] {
                assert!(raw_control(&id, command)["error"].is_string());
            }
            // Read-only status must not extend the lease either.
            assert_eq!(
                fixture.control(&["status", "--run-id", &id])["status"],
                "running"
            );
            next_probe += Duration::from_secs(5);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let elapsed = last_authorized_contact.elapsed();
    assert!(
        elapsed >= Duration::from_secs(58),
        "run stopped before its monitoring lease expired"
    );
    assert!(
        elapsed < Duration::from_secs(80),
        "lease cleanup exceeded its bound"
    );
    assert!(session.wait(SHORT_LIMIT).success());
    assert!(
        session
            .events
            .iter()
            .any(|event| event["type"] == "stopped")
    );
    assert_eq!(
        fixture.requests("turn/start").len(),
        turns_before,
        "a new model turn started after monitoring stopped"
    );
    assert_eq!(fixture.requests("thread/start").len(), 2);
    assert!(fixture.requests("turn/steer").is_empty());
    let final_status = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(final_status["status"], "stopped");
    assert_eq!(final_status["partial_paths"].as_array().unwrap().len(), 2);
    fixture.assert_preserved_and_stopped();
}

#[test]
fn run_can_be_updated_and_stopped_after_its_installed_package_is_removed() {
    let fixture = Fixture::new("wait");
    let installed = fixture.root.join("installed plugin/bin");
    fs::create_dir_all(&installed).unwrap();
    let executable = installed.join("delm");
    fs::copy(env!("CARGO_BIN_EXE_delm"), &executable).unwrap();
    let template = fixture.command();
    let mut launch = Command::new(&executable);
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            launch.env(key, value);
        } else {
            launch.env_remove(key);
        }
    }
    let stderr = fixture.root.join("package-removal.stderr");
    let mut child = launch
        .current_dir(&fixture.root)
        .arg("run")
        .arg("--project")
        .arg(&fixture.project)
        .arg("--task-file")
        .arg(fixture.root.join("task.txt"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let fd = stdout.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut session = Session {
        child,
        stdout,
        pending: Vec::new(),
        events: Vec::new(),
        stderr,
    };
    let settings = session.until(|event| event["type"] == "settings", SHORT_LIMIT);
    let retained = PathBuf::from(settings["control_executable"].as_str().unwrap());
    assert!(
        retained.starts_with(
            fixture
                .home
                .join("Library/Application Support/DeLM/runtimes")
        )
    );
    assert_ne!(retained, executable);
    let id = session.run_id();
    fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    session.until(|event| event["type"] == "started", SHORT_LIMIT);
    // Simulate the native plugin manager deleting the old cached version.
    fs::remove_dir_all(installed.parent().unwrap()).unwrap();
    let control = |arguments: &[&str]| {
        let output = Command::new(&retained)
            .args(arguments)
            .env("HOME", &fixture.home)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let status = control(&["status", "--run-id", &id, "--keep-alive"]);
    assert_eq!(status["status"], "running");
    assert_eq!(status["control_executable"], retained.to_str().unwrap());
    let update = fixture.root.join("package-update.txt");
    fs::write(
        &update,
        "Continue the original request after the plugin update.",
    )
    .unwrap();
    control(&[
        "update",
        "--run-id",
        &id,
        "--message-file",
        update.to_str().unwrap(),
    ]);
    session.until(|event| event["request_revision"] == 2, SHORT_LIMIT);
    control(&["stop", "--run-id", &id]);
    session.until(|event| event["type"] == "stopped", SHORT_LIMIT);
    assert!(session.wait(SHORT_LIMIT).success());
    assert_eq!(control(&["status", "--run-id", &id])["status"], "stopped");
    fixture.assert_preserved_and_stopped();
}

#[test]
fn public_inputs_are_captured_for_both_workers_and_updates() {
    let fixture = Fixture::new("wait");
    let source = fixture.root.join("selected.svg");
    fs::write(&source, "<svg>first</svg>").unwrap();
    let manifest = fixture.root.join("inputs.json");
    fs::write(
        &manifest,
        json!({"version":1,"files":[{"kind":"image","path":source}]}).to_string(),
    )
    .unwrap();
    let mut session = fixture.start();
    let id = session.run_id();
    fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    session.until(|event| event["type"] == "started", SHORT_LIMIT);
    let starts = fixture.requests("turn/start");
    assert_eq!(starts.len(), 2);
    let original = starts[0]["params"]["input"][1]["path"].as_str().unwrap();
    assert_eq!(starts[1]["params"]["input"][1]["path"], original);
    assert_ne!(Path::new(original), source);
    fs::write(&source, "<svg>second</svg>").unwrap();
    assert_eq!(fs::read_to_string(original).unwrap(), "<svg>first</svg>");
    fixture.control(&[
        "update",
        "--run-id",
        &id,
        "--message-file",
        fixture.root.join("task.txt").to_str().unwrap(),
        "--inputs-file",
        manifest.to_str().unwrap(),
    ]);
    session.until(|event| event["request_revision"] == 2, SHORT_LIMIT);
    // A status from the second worker follows both steer acknowledgments.
    session.until(
        |event| {
            event["message"]
                .as_str()
                .is_some_and(|text| text.starts_with("Worker 2: Fixture received"))
        },
        SHORT_LIMIT,
    );
    let steers = fixture.requests("turn/steer");
    assert_eq!(steers.len(), 2);
    for steer in steers {
        let path = steer["params"]["input"][1]["path"].as_str().unwrap();
        assert_ne!(path, original);
        assert_eq!(fs::read_to_string(path).unwrap(), "<svg>second</svg>");
    }
    fixture.control(&["stop", "--run-id", &id]);
    session.until(|event| event["type"] == "stopped", SHORT_LIMIT);
    assert!(session.wait(SHORT_LIMIT).success());
    fixture.assert_preserved_and_stopped();
}

#[test]
fn public_answers_target_one_question_and_leave_the_other_pending() {
    let fixture = Fixture::new("questions");
    let mut session = fixture.start();
    let id = session.run_id();
    fixture.control(&["status", "--run-id", &id, "--keep-alive"]);
    let first = session.until(|event| event["type"] == "question", SHORT_LIMIT);
    let second = session.until(
        |event| event["type"] == "question" && event["id"] != first["id"],
        SHORT_LIMIT,
    );
    let status = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(status["questions"].as_object().unwrap().len(), 2);
    assert_eq!(
        status["questions"][first["id"].as_str().unwrap()]["questions"][0]["options"][0]["label"],
        "SVG"
    );
    fixture.control(&[
        "update",
        "--run-id",
        &id,
        "--message-file",
        fixture.root.join("task.txt").to_str().unwrap(),
    ]);
    session.until(|event| event["request_revision"] == 2, SHORT_LIMIT);
    let status = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(
        status["questions"].as_object().unwrap().len(),
        2,
        "ordinary updates must not answer questions"
    );
    let answer_file = fixture.root.join("answer.json");
    fs::write(&answer_file, r#"{"format":["SVG"]}"#).unwrap();
    fixture.control(&[
        "answer",
        "--run-id",
        &id,
        "--question-id",
        first["id"].as_str().unwrap(),
        "--answers-file",
        answer_file.to_str().unwrap(),
    ]);
    session.until(|event| event["request_revision"] == 3, SHORT_LIMIT);
    let status = fixture.control(&["status", "--run-id", &id]);
    assert_eq!(status["questions"].as_object().unwrap().len(), 1);
    assert!(
        status["questions"]
            .get(second["id"].as_str().unwrap())
            .is_some()
    );
    let responses = fixture
        .wire()
        .into_iter()
        .filter(|event| {
            event["direction"] == "in" && event["message"]["result"].get("answers").is_some()
        })
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 1);
    let duplicate = fixture.cli(&[
        "answer",
        "--run-id",
        &id,
        "--question-id",
        first["id"].as_str().unwrap(),
        "--answers-file",
        answer_file.to_str().unwrap(),
    ]);
    assert!(!duplicate.status.success());
    fixture.control(&["stop", "--run-id", &id]);
    session.until(|event| event["type"] == "stopped", SHORT_LIMIT);
    assert!(session.wait(SHORT_LIMIT).success());
    fixture.assert_preserved_and_stopped();
}
