#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(12);

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        // The committed fixture must be quiescent before its strict snapshot.
        // Newer Git can detach auto-maintenance and remove its lock afterward.
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

// Include Git administration, directory modes, executable bits and all bytes.
// A size-only comparison would miss both equal-size edits and new Git metadata.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    fn walk(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let bytes = if metadata.file_type().is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else if metadata.is_file() {
            fs::read(path).unwrap()
        } else {
            Vec::new()
        };
        result.insert(
            path.strip_prefix(root).unwrap().into(),
            (metadata.permissions().mode(), bytes),
        );
        if metadata.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                walk(root, &child.unwrap().path(), result);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    host: PathBuf,
    before: BTreeMap<PathBuf, (u32, Vec<u8>)>,
}

impl Fixture {
    fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir(&project).unwrap();
        fs::write(project.join("source.txt"), "original\n").unwrap();
        git(&project, &["init", "-q", "--template="]);
        git(&project, &["add", "."]);
        git(
            &project,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-qm",
                "Fixture",
            ],
        );
        let host = temp.path().join("worker_host.py");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/worker_host.py"),
            &host,
        )
        .unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).unwrap();
        let before = snapshot(&project);
        let fixture = Self {
            _temp: temp,
            home,
            project,
            host,
            before,
        };
        fixture.mode(mode);
        fixture
    }

    fn mode(&self, mode: &str) {
        fs::write(
            self.host.with_extension("json"),
            json!({"mode":mode}).to_string(),
        )
        .unwrap();
    }

    fn request(&self, seconds: u64) -> Value {
        json!({"type":"start", "project":self.project, "task":"Create result.txt",
            "model":"fixture", "model_provider":"openai", "auth_home":self.home.join(".codex"),
            "host_executable":self.host, "seconds":seconds,
            "policy":{"approval_policy":"never", "sandbox":{"type":"workspace-write","network_access":false},
                "file_system":{"kind":"restricted","entries":[
                    {"path":{"type":"special","value":{"kind":"root"}},"access":"read"},
                    {"path":{"type":"path","path":self.project},"access":"write"}]},
                "network":"restricted", "network_proxy_active":false}})
    }

    fn start(&self, seconds: u64) -> Session {
        self.launch(self.request(seconds))
    }

    fn launch(&self, request: Value) -> Session {
        let stderr = self
            .host
            .with_extension(format!("{}.stderr", uuid::Uuid::new_v4()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_delm"))
            .arg("--stdio")
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let fd = stdout.as_raw_fd();
        // One owner can close this pipe to exercise host-reader loss. Polling is
        // bounded so a broken runtime cannot hang cargo test in a blocking read.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut session = Session {
            child,
            stdin,
            stdout: Some(stdout),
            buffer: Vec::new(),
            events: Vec::new(),
            stderr,
        };
        session.send(request);
        session
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
            .filter(|event| event["direction"] == "in" && event["message"]["method"] == method)
            .map(|event| event["message"].clone())
            .collect()
    }

    fn wait_request(&self, session: &mut Session, method: &str) {
        let deadline = Instant::now() + LIMIT;
        while self.requests(method).is_empty() {
            assert!(
                Instant::now() < deadline,
                "fixture never received {method}: {:?}",
                self.wire()
            );
            assert!(
                session.child.try_wait().unwrap().is_none(),
                "runtime exited early: {}",
                session.stderr()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_original(&self) {
        assert_eq!(
            snapshot(&self.project),
            self.before,
            "original repository changed"
        );
    }

    fn run_path(&self, event: &Value) -> PathBuf {
        self.home
            .join("Library/Application Support/DeLM/runs")
            .join(event["run_id"].as_str().unwrap())
    }

    fn assert_delivered(&self, revision: u64) {
        let mut actual = snapshot(&self.project);
        let result = actual
            .remove(Path::new("result.txt"))
            .expect("result was not delivered into the original project");
        assert!([b"thread-1\n".to_vec(), b"thread-2\n".to_vec()].contains(&result.1));
        if revision > 1 {
            let revised = actual
                .remove(Path::new("revised.txt"))
                .expect("revised result was not delivered");
            assert_eq!(revised.1, format!("revision {revision}\n").into_bytes());
        }
        assert_eq!(
            actual, self.before,
            "delivery changed pre-existing project or Git data"
        );
    }

    fn assert_workspaces_removed(&self, event: &Value) {
        let workspace = self.run_path(event).join("workspace");
        for name in ["worker-1", "worker-2", "baseline"] {
            assert!(
                !workspace.join(name).exists(),
                "temporary {name} remains after cleanup"
            );
        }
    }

    fn assert_hosts_stopped(&self) {
        let pids = self
            .wire()
            .into_iter()
            .filter_map(|event| event["pid"].as_i64())
            .collect::<BTreeSet<_>>();
        assert!(!pids.is_empty(), "fixture did not start");
        for pid in pids {
            assert_eq!(
                unsafe { libc::kill(pid as libc::pid_t, 0) },
                -1,
                "fixture host {pid} survived runtime shutdown"
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
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    buffer: Vec<u8>,
    events: Vec<Value>,
    stderr: PathBuf,
}

impl Session {
    fn send(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn next(&mut self, deadline: Instant) -> Value {
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line = self.buffer.drain(..=end).collect::<Vec<_>>();
                let event = serde_json::from_slice(&line).unwrap();
                self.events.push(event);
                return self.events.last().unwrap().clone();
            }
            let mut bytes = [0; 8192];
            match self.stdout.as_mut().unwrap().read(&mut bytes) {
                Ok(0) => panic!(
                    "runtime output closed: {:?}; {}",
                    self.events,
                    self.stderr()
                ),
                Ok(n) => self.buffer.extend_from_slice(&bytes[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "runtime timed out: {:?}; {}",
                        self.events,
                        self.stderr()
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("runtime read failed: {error}"),
            }
        }
    }

    fn until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + LIMIT;
        loop {
            let event = self.next(deadline);
            if predicate(&event) {
                return event;
            }
            assert!(
                !terminal(&event),
                "unexpected terminal event: {:?}",
                self.events
            );
        }
    }

    fn finish(&mut self) -> Value {
        let event = self.until(terminal);
        assert!(
            self.wait().success(),
            "runtime failed: {:?}; {}",
            self.events,
            self.stderr()
        );
        event
    }

    fn accept(&mut self, event: &Value) {
        self.send(json!({"type":"accept_result", "request_revision":event["request_revision"]}));
    }

    fn wait(&mut self) -> ExitStatus {
        self.stdin.take();
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "runtime did not exit: {:?}; {}",
                self.events,
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn terminal(event: &Value) -> bool {
    matches!(event["type"].as_str(), Some("result" | "stopped" | "error"))
}

fn running(event: &Value) -> bool {
    event["type"] == "started"
}

fn assert_partial(event: &Value) {
    assert_eq!(event["type"], "stopped", "{event}");
    let paths = event["partial_paths"].as_array().unwrap();
    assert_eq!(
        paths.len(),
        1,
        "stopped runs expose one compact recovery bundle"
    );
    let recovery = Path::new(paths[0].as_str().unwrap());
    let manifest: Value =
        serde_json::from_slice(&fs::read(recovery.join("complete.json")).unwrap()).unwrap();
    assert_eq!(manifest["workers"].as_array().unwrap().len(), 2);
    for name in ["worker-1", "worker-2", "baseline"] {
        assert!(
            !recovery.parent().unwrap().join(name).exists(),
            "temporary {name} remains"
        );
    }
    assert_eq!(event["details"]["cleanup_complete"], true);
}

#[test]
fn normal_completion_delivers_into_original_and_removes_both_workspaces() {
    let fixture = Fixture::new("complete");
    let mut session = fixture.start(30);
    let ready = session.until(|event| event["type"] == "ready");
    session.accept(&ready);
    let result = session.finish();
    assert_eq!(result["type"], "result", "{:?}", session.events);
    let path = Path::new(result["path"].as_str().unwrap());
    assert!(path.join("result.txt").is_file());
    assert_eq!(
        fs::canonicalize(path).unwrap(),
        fs::canonicalize(&fixture.project).unwrap()
    );
    fixture.assert_workspaces_removed(&result);
    assert_eq!(result["details"]["delivery"]["cleanup_complete"], true);
    assert!(Path::new(result["details"]["completion"].as_str().unwrap()).is_file());
    assert_eq!(fixture.requests("thread/start").len(), 2);
    fixture.assert_hosts_stopped();
    fixture.assert_delivered(1);
}

#[test]
fn team_count_three_and_four_route_native_approval_to_last_worker_and_deliver() {
    for count in [3, 4] {
        let fixture = Fixture::new("approvals");
        let mut request = fixture.request(20);
        request["worker_count"] = json!(count);
        let mut session = fixture.launch(request);
        let approvals: Vec<_> = (0..count)
            .map(|_| session.until(|event| event["type"] == "approval"))
            .collect();
        let identities = approvals
            .iter()
            .map(|event| event["details"]["worker"].as_u64().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(identities, (1..=count as u64).collect());
        for approval in &approvals {
            let decision = if approval["details"]["worker"] == count {
                "accept"
            } else {
                "decline"
            };
            session.send(
                json!({"type":"respond", "id":approval["id"], "response":{"decision":decision}}),
            );
        }
        let ready = session.until(|event| event["type"] == "ready");
        session.accept(&ready);
        let result = session.finish();
        assert_eq!(result["type"], "result", "{:?}", session.events);
        assert_eq!(result["details"]["worker"], count);
        assert_eq!(
            fs::read_to_string(fixture.project.join("result.txt")).unwrap(),
            format!("thread-{count}\n")
        );
        let mut delivered = snapshot(&fixture.project);
        delivered.remove(Path::new("result.txt")).unwrap();
        assert_eq!(
            delivered, fixture.before,
            "delivery changed pre-existing data"
        );
        let run = fixture.run_path(&result);
        let saved: Value =
            serde_json::from_slice(&fs::read(run.join("run.json")).unwrap()).unwrap();
        assert_eq!(saved["request"]["worker_count"], count);
        assert_eq!(saved["workers"].as_array().unwrap().len(), count);
        assert_eq!(
            saved["workspace"]["workers"].as_array().unwrap().len(),
            count
        );
        let starts = fixture.requests("thread/start");
        assert_eq!(starts.len(), count);
        assert_eq!(fixture.requests("turn/start").len(), count);
        for (index, start) in starts.iter().enumerate() {
            let instructions = start["params"]["developerInstructions"].as_str().unwrap();
            assert!(instructions.contains(&format!("You are worker {} of {count}.", index + 1)));
            assert!(
                start["params"]["cwd"]
                    .as_str()
                    .unwrap()
                    .ends_with(&format!("worker-{}", index + 1))
            );
        }
        for method in ["thread/backgroundTerminals/clean", "thread/archive"] {
            let stopped = fixture
                .requests(method)
                .iter()
                .map(|request| request["params"]["threadId"].as_str().unwrap().to_owned())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                stopped,
                (1..=count).map(|id| format!("thread-{id}")).collect()
            );
        }
        for name in std::iter::once("baseline".to_owned())
            .chain((1..=count).map(|id| format!("worker-{id}")))
        {
            assert!(!run.join("workspace").join(name).exists());
        }
        fixture.assert_hosts_stopped();
    }
}

#[test]
fn team_count_four_updates_and_stops_every_worker_with_complete_recovery() {
    let fixture = Fixture::new("wait");
    let mut request = fixture.request(20);
    request["worker_count"] = json!(4);
    let mut session = fixture.launch(request);
    session.until(running);
    session.send(json!({"type":"message", "text":"Report the current status."}));
    let deadline = Instant::now() + LIMIT;
    while fixture.requests("turn/steer").len() < 4 {
        assert!(
            Instant::now() < deadline,
            "user update did not reach all workers"
        );
        assert!(session.child.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    session.send(json!({"type":"stop"}));
    let result = session.finish();
    assert_eq!(result["type"], "stopped");
    assert_eq!(result["details"]["cleanup_complete"], true);
    let recovery = Path::new(result["partial_paths"][0].as_str().unwrap());
    let manifest: Value =
        serde_json::from_slice(&fs::read(recovery.join("complete.json")).unwrap()).unwrap();
    assert_eq!(manifest["worker_count"], 4);
    assert_eq!(manifest["workers"].as_array().unwrap().len(), 4);
    let interrupted = fixture
        .requests("turn/interrupt")
        .iter()
        .map(|request| request["params"]["threadId"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        interrupted,
        (1..=4).map(|id| format!("thread-{id}")).collect()
    );
    for id in 1..=4 {
        assert!(
            !recovery
                .parent()
                .unwrap()
                .join(format!("worker-{id}"))
                .exists()
        );
    }
    assert!(!recovery.parent().unwrap().join("baseline").exists());
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn stop_preserves_compact_recovery_and_removes_both_private_projects() {
    let fixture = Fixture::new("wait");
    let mut session = fixture.start(30);
    session.until(running);
    session.send(json!({"type":"stop"}));
    assert_partial(&session.finish());
    assert_eq!(fixture.requests("turn/start").len(), 2);
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn stop_cancels_a_native_initialize_that_never_replies() {
    let fixture = Fixture::new("block_initialize");
    let mut session = fixture.start(30);
    fixture.wait_request(&mut session, "initialize");
    let started = Instant::now();
    session.send(json!({"type":"stop"}));
    assert_partial(&session.finish());
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "Stop waited for RPC timeout"
    );
    assert!(fixture.requests("thread/start").is_empty());
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn deadline_cancels_a_native_initialize_that_never_replies() {
    let fixture = Fixture::new("block_initialize");
    let started = Instant::now();
    // Leave enough setup time when all filesystem fixtures run concurrently.
    // The native RPC timeout is 45 seconds, well beyond this run allowance.
    let mut session = fixture.start(6);
    fixture.wait_request(&mut session, "initialize");
    let stopped = session.finish();
    assert_eq!(stopped["type"], "stopped");
    // The independent hard deadline may win the cooperative shutdown race.
    // In that case DeLM preserves both projects but does not present them as
    // safe to reopen until process ownership has been resolved.
    if stopped.get("partial_paths").is_some() {
        assert_partial(&stopped);
    } else {
        assert!(
            stopped["message"]
                .as_str()
                .unwrap()
                .contains("cleanup is unresolved")
        );
        let run = fixture
            .home
            .join("Library/Application Support/DeLM/runs")
            .join(stopped["run_id"].as_str().unwrap());
        for worker in ["worker-1", "worker-2"] {
            assert!(
                run.join("workspace")
                    .join(worker)
                    .join("source.txt")
                    .is_file()
            );
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "deadline waited for RPC timeout"
    );
    assert_eq!(fixture.requests("initialize").len(), 1);
    assert!(fixture.requests("thread/start").is_empty());
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn a_full_update_queue_cancels_instead_of_blocking_the_stop_reader() {
    let fixture = Fixture::new("block_initialize");
    let mut session = fixture.start(30);
    fixture.wait_request(&mut session, "initialize");
    let burst = (0..80)
        .map(|index| {
            format!(
                "{}\n",
                json!({"type":"message","text":format!("queued update {index}")})
            )
        })
        .collect::<String>();
    session
        .stdin
        .as_mut()
        .unwrap()
        .write_all(burst.as_bytes())
        .unwrap();
    session.stdin.as_mut().unwrap().flush().unwrap();
    let stopped = session.finish();
    assert_partial(&stopped);
    assert!(
        stopped["message"]
            .as_str()
            .unwrap()
            .contains("last update was not accepted"),
        "{stopped}"
    );
    assert!(fixture.requests("thread/start").is_empty());
    let run = fixture
        .home
        .join("Library/Application Support/DeLM/runs")
        .join(stopped["run_id"].as_str().unwrap());
    let saved: Value = serde_json::from_slice(&fs::read(run.join("run.json")).unwrap()).unwrap();
    assert_eq!(
        saved["revision"], 65,
        "the 64 accepted updates must be retained"
    );
    assert!(
        saved["request"]["task"]
            .as_str()
            .unwrap()
            .contains("queued update 63")
    );
    assert!(
        !saved["request"]["task"]
            .as_str()
            .unwrap()
            .contains("queued update 64")
    );
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn losing_the_output_reader_cancels_even_when_input_stays_open() {
    let fixture = Fixture::new("wait");
    let mut session = fixture.start(30);
    session.until(running);
    session.stdout.take();
    session.send(json!({"type":"message", "text":"Report the current status."}));
    // Retain stdin here: EOF must not be the cause of cancellation.
    let deadline = Instant::now() + LIMIT;
    let status = loop {
        if let Some(status) = session.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "output loss did not exit: {}; wire={:?}",
            session.stderr(),
            fixture.wire().iter().rev().take(8).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success(), "broken native output should be reported");
    assert!(
        session.stderr().contains("Broken pipe"),
        "{}",
        session.stderr()
    );
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn a_lost_turn_start_ack_never_creates_a_second_team() {
    let fixture = Fixture::new("lose_turn_ack");
    let mut session = fixture.start(30);
    fixture.wait_request(&mut session, "turn/start");
    session.send(json!({"type":"stop"}));
    assert_partial(&session.finish());
    assert_eq!(fixture.requests("initialize").len(), 1);
    assert_eq!(fixture.requests("thread/start").len(), 2);
    assert_eq!(
        fixture.requests("turn/start").len(),
        1,
        "unknown start must not be retried"
    );
    assert!(fixture.requests("thread/resume").is_empty());
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn native_model_mismatch_is_rejected_before_starting_a_turn() {
    assert_native_mismatch("mismatch_model", "model");
}

#[test]
fn native_permission_profile_mismatch_is_rejected_before_starting_a_turn() {
    assert_native_mismatch("mismatch_profile", "activePermissionProfile");
}

fn assert_native_mismatch(mode: &str, reason: &str) {
    let fixture = Fixture::new(mode);
    let mut request = fixture.request(30);
    request["auth_settings"] =
        json!({"native_thread_settings":{"activePermissionProfile":{"id":":workspace"}}});
    let mut session = fixture.launch(request);
    let stopped = session.finish();
    assert_partial(&stopped);
    assert!(
        stopped["message"].as_str().unwrap().contains(reason),
        "{stopped}"
    );
    assert_eq!(fixture.requests("thread/start").len(), 1);
    assert!(fixture.requests("turn/start").is_empty());
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn matching_fresh_chatgpt_identity_allows_worker_turns() {
    let fixture = Fixture::new("complete");
    let mut request = fixture.request(30);
    request["auth_settings"] = json!({"account_identity":{"type":"chatgpt", "email":"fixture@localhost", "workspace_id":"fixture-workspace"}});
    let mut session = fixture.launch(request);
    let ready = session.until(|event| event["type"] == "ready");
    session.accept(&ready);
    assert_eq!(session.finish()["type"], "result");
    let reads = fixture.requests("account/read");
    assert_eq!(
        reads.len(),
        1,
        "identity must be read from the current host"
    );
    assert_eq!(reads[0]["params"]["refreshToken"], false);
    assert_eq!(fixture.requests("turn/start").len(), 2);
    let wire = fixture.wire();
    let account = wire
        .iter()
        .position(|event| event["message"]["method"] == "account/read")
        .unwrap();
    let thread = wire
        .iter()
        .position(|event| event["message"]["method"] == "thread/start")
        .unwrap();
    assert!(
        account < thread,
        "account identity must be checked before worker creation"
    );
    fixture.assert_hosts_stopped();
    fixture.assert_delivered(1);
}

#[test]
fn changed_fresh_chatgpt_workspace_is_rejected_before_any_worker_turn() {
    let fixture = Fixture::new("mismatch_account");
    let mut request = fixture.request(30);
    request["auth_settings"] = json!({"account_identity":{"type":"chatgpt", "email":"fixture@localhost", "workspace_id":"fixture-workspace"}});
    let mut session = fixture.launch(request);
    let stopped = session.finish();
    assert_partial(&stopped);
    assert!(
        stopped["message"]
            .as_str()
            .unwrap()
            .contains("native account changed"),
        "{stopped}"
    );
    assert_eq!(fixture.requests("account/read").len(), 1);
    assert!(fixture.requests("thread/start").is_empty());
    assert!(fixture.requests("turn/start").is_empty());
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn stale_completion_is_refused_until_the_current_revision_is_declared() {
    let fixture = Fixture::new("stale_revision");
    let mut session = fixture.start(30);
    let ready = session.until(|event| event["type"] == "ready");
    assert_eq!(ready["request_revision"], 1);
    session.accept(&ready);
    assert_eq!(session.finish()["type"], "result");
    let wire = fixture.wire();
    let stale = wire
        .iter()
        .find(|event| {
            event["direction"] == "out"
                && event["message"]["params"]["tool"] == "delm_complete"
                && event["message"]["params"]["arguments"]["expected_revision"] == 0
        })
        .unwrap();
    let id = &stale["message"]["id"];
    let refusal = wire
        .iter()
        .find(|event| event["direction"] == "in" && &event["message"]["id"] == id)
        .unwrap();
    assert_eq!(refusal["message"]["result"]["success"], false);
    assert!(
        refusal["message"]["result"]["contentItems"][0]["text"]
            .as_str()
            .unwrap()
            .contains("revision")
    );
    assert!(
        wire.iter()
            .any(|event| event["message"]["params"]["arguments"]["expected_revision"] == 1)
    );
    fixture.assert_hosts_stopped();
    fixture.assert_delivered(1);
}

#[test]
fn a_user_update_at_ready_requires_a_fresh_completion() {
    let fixture = Fixture::new("complete");
    let mut session = fixture.start(30);
    let old = session.until(|event| event["type"] == "ready");
    assert_eq!(old["request_revision"], 1);
    session.send(json!({"type":"message", "text":"Also create revised.txt for the new revision."}));
    // A previously queued acceptance cannot release the stale candidate.
    session.accept(&old);
    let fresh = session.until(|event| event["type"] == "ready" && event["request_revision"] == 2);
    session.accept(&fresh);
    let result = session.finish();
    assert_eq!(result["type"], "result", "{:?}", session.events);
    let path = Path::new(result["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("revised.txt")).unwrap(),
        "revision 2\n"
    );
    let completion: Value = serde_json::from_slice(
        &fs::read(result["details"]["completion"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    fixture.assert_workspaces_removed(&result);
    assert_eq!(completion["revision"], 2);
    assert_eq!(fixture.requests("thread/start").len(), 2);
    fixture.assert_hosts_stopped();
    fixture.assert_delivered(2);
}

#[test]
fn an_update_during_preparation_advances_the_initial_worker_revision() {
    let fixture = Fixture::new("complete");
    let mut session = fixture.start(30);
    session.send(json!({"type":"message", "text":"Also create revised.txt for this update."}));
    let ready = session.until(|event| event["type"] == "ready");
    assert_eq!(ready["request_revision"], 2);
    session.accept(&ready);
    let result = session.finish();
    assert_eq!(result["type"], "result");
    assert_eq!(
        fs::read_to_string(Path::new(result["path"].as_str().unwrap()).join("revised.txt"))
            .unwrap(),
        "revision 2\n"
    );
    let turns = fixture.requests("turn/start");
    assert_eq!(
        turns.len(),
        2,
        "preparation updates should be in the original turns"
    );
    for request in turns {
        let text = request["params"]["input"][0]["text"].as_str().unwrap();
        assert!(text.contains("revision 2"), "{text}");
        assert!(text.contains("Also create revised.txt"), "{text}");
    }
    assert!(fixture.requests("turn/steer").is_empty());
    fixture.assert_hosts_stopped();
    fixture.assert_delivered(2);
}

#[test]
fn stopped_runs_cannot_resume_deleted_workspaces_or_create_a_second_team() {
    let fixture = Fixture::new("wait");
    let mut first = fixture.start(60);
    first.until(running);
    first.send(json!({"type":"stop"}));
    let stopped = first.finish();
    assert_partial(&stopped);
    fixture.assert_hosts_stopped();
    fixture.assert_original();
    let before = fixture.wire();
    let mut resumed = fixture.launch(
        json!({"type":"resume", "run_id":stopped["run_id"], "authorization":fixture.request(60)}),
    );
    let refused = resumed.until(terminal);
    assert_eq!(refused["type"], "error");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("not available to resume")
    );
    assert!(!resumed.wait().success());
    assert_eq!(
        fixture.wire(),
        before,
        "a stopped run must not launch or resume workers"
    );
    fixture.assert_workspaces_removed(&stopped);
    assert_partial(&stopped);
}

#[test]
fn resume_rejects_changed_reasoning_effort_before_launching_a_host() {
    assert_resume_setting_rejected("reasoning_effort", "medium");
}

#[test]
fn resume_rejects_changed_service_tier_before_launching_a_host() {
    assert_resume_setting_rejected("service_tier", "flex");
}

fn assert_resume_setting_rejected(field: &str, changed: &str) {
    let fixture = Fixture::new("wait");
    let mut request = fixture.request(60);
    request["reasoning_effort"] = json!("high");
    request["service_tier"] = json!("priority");
    let mut first = fixture.launch(request.clone());
    first.until(running);
    first.send(json!({"type":"stop"}));
    let paused = first.finish();
    assert_partial(&paused);
    // Legacy paused runs still need authorization validation before any host
    // connection. Simulate only their header; a rejected resume must never
    // inspect or recreate the removed worker projects.
    let saved_path = fixture.run_path(&paused).join("run.json");
    let mut saved: Value = serde_json::from_slice(&fs::read(&saved_path).unwrap()).unwrap();
    saved["status"] = json!("paused");
    fs::write(&saved_path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let wire_before = fixture.wire();
    request[field] = json!(changed);
    let mut resumed = fixture
        .launch(json!({"type":"resume", "run_id":paused["run_id"], "authorization":request}));
    let rejected = resumed.until(terminal);
    assert_eq!(rejected["type"], "error");
    assert!(
        rejected["message"]
            .as_str()
            .unwrap()
            .contains("stale authorization"),
        "{rejected}"
    );
    assert!(
        !resumed.wait().success(),
        "a rejected resume must report failure"
    );
    assert_eq!(
        fixture.wire(),
        wire_before,
        "rejected resume contacted a native host"
    );
    assert_partial(&paused);
    fixture.assert_hosts_stopped();
    fixture.assert_original();
}

#[test]
fn retired_native_approvals_are_rejected_without_blocking_completion() {
    let fixture = Fixture::new("stale_approval");
    let mut session = fixture.start(30);
    let ready = session.until(|event| event["type"] == "ready");
    assert!(
        !session
            .events
            .iter()
            .any(|event| event["type"] == "approval")
    );
    session.accept(&ready);
    let result = session.finish();
    assert_eq!(result["type"], "result");
    let rejected = fixture
        .wire()
        .into_iter()
        .filter(|event| {
            event["direction"] == "in"
                && event["message"]["error"]["message"] == "Approval belongs to a retired turn"
        })
        .count();
    assert_eq!(rejected, 2);
    fixture.assert_delivered(1);
}

#[test]
fn acceptance_waits_for_a_late_native_request_without_losing_the_candidate() {
    let fixture = Fixture::new("late_approval");
    let mut session = fixture.start(30);
    let ready = session.until(|event| event["type"] == "ready");
    let approval = session.until(|event| event["type"] == "approval");
    session.accept(&ready);
    session.send(json!({"type":"respond","id":approval["id"],"response":{"decision":"decline"}}));
    // The already accepted candidate resumes after the pending request clears;
    // no extra accept_result command is needed and the peer need not finish.
    let result = session.finish();
    assert_eq!(result["type"], "result");
    assert!(
        session
            .events
            .iter()
            .any(|event| event["type"] == "approval_resolved")
    );
    fixture.assert_delivered(1);
    fixture.assert_workspaces_removed(&result);
}

#[test]
fn native_approvals_wait_for_correlated_user_responses_without_changing_them() {
    let fixture = Fixture::new("approvals");
    let mut session = fixture.start(30);
    let first = session.until(|event| event["type"] == "approval");
    let second = session.until(|event| event["type"] == "approval");
    assert_ne!(first["id"], second["id"]);
    assert_ne!(first["details"]["worker"], second["details"]["worker"]);
    session.send(
        json!({"type":"respond","id":first["id"],"response":{"decision":"acceptForSession"}}),
    );
    let refusal = session.until(|event| event["type"] == "notice");
    assert!(refusal["message"].as_str().unwrap().contains("not offered"));
    let requests = fixture
        .wire()
        .into_iter()
        .filter(|event| {
            event["direction"] == "out"
                && event["message"]["method"] == "item/commandExecution/requestApproval"
        })
        .map(|event| event["message"]["id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert!(
        !fixture
            .wire()
            .iter()
            .any(|event| event["direction"] == "in" && requests.contains(&event["message"]["id"])),
        "an invalid or absent user decision must never be auto-approved"
    );
    for event in [&first, &second] {
        session.send(json!({"type":"respond","id":event["id"],"response":{"decision":"accept"}}));
    }
    let ready = session.until(|event| event["type"] == "ready");
    session.accept(&ready);
    let result = session.finish();
    assert_eq!(result["type"], "result");
    for id in requests {
        let replies = fixture
            .wire()
            .into_iter()
            .filter(|event| event["direction"] == "in" && event["message"]["id"] == id)
            .collect::<Vec<_>>();
        assert_eq!(replies.len(), 1);
        assert_eq!(
            replies[0]["message"]["result"],
            json!({"decision":"accept"})
        );
    }
    fixture.assert_delivered(1);
    fixture.assert_workspaces_removed(&result);
    fixture.assert_hosts_stopped();
}
