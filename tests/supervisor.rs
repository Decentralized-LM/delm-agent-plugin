#![cfg(target_os = "macos")]
use delm::supervisor::{ProcessIdentity, ShutdownReport};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture exceeded time bound");
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn grouped(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[test]
#[ignore = "subprocess fixture invoked by the supervision tests"]
fn fixture_process() {
    let path = PathBuf::from(std::env::var_os("DELM_FIXTURE_DIR").expect("fixture root"));
    if std::env::var("DELM_FIXTURE_ROLE").unwrap() == "runtime" {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "fixture_process", "--ignored", "--nocapture"])
            .env("DELM_FIXTURE_ROLE", "host")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        grouped(&mut command);
        let mut host = command.spawn().unwrap();
        fs::write(path.join("host-pid"), host.id().to_string()).unwrap();
        let _ = host.wait();
    } else {
        let mut command = if std::env::var_os("DELM_FIXTURE_IGNORE_TERM").is_some() {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "trap '' TERM; exec /bin/sleep 60"]);
            command
        } else {
            let mut command = Command::new("/bin/sleep");
            command.arg("60");
            command
        };
        grouped(&mut command);
        let mut detached = command.spawn().unwrap();
        fs::write(path.join("tool-pid"), detached.id().to_string()).unwrap();
        let _ = detached.wait();
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    runtime: Child,
    host: ProcessIdentity,
    tool: ProcessIdentity,
}
impl Fixture {
    fn new() -> Self {
        Self::with_term_behavior(false)
    }
    fn with_term_behavior(ignore_term: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "fixture_process", "--ignored", "--nocapture"])
            .env("DELM_FIXTURE_DIR", temp.path())
            .env("DELM_FIXTURE_ROLE", "runtime")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if ignore_term {
            command.env("DELM_FIXTURE_IGNORE_TERM", "1");
        }
        let runtime = command.spawn().unwrap();
        until(|| temp.path().join("host-pid").exists() && temp.path().join("tool-pid").exists());
        let host = ProcessIdentity::capture(
            fs::read_to_string(temp.path().join("host-pid"))
                .unwrap()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let tool = ProcessIdentity::capture(
            fs::read_to_string(temp.path().join("tool-pid"))
                .unwrap()
                .parse()
                .unwrap(),
        )
        .unwrap();
        Self {
            temp,
            runtime,
            host,
            tool,
        }
    }
    fn watchdog(&self, deadline_ms: u64) -> Child {
        let spec = self.temp.path().join("watchdog.json");
        let mut spec_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&spec)
            .unwrap();
        spec_file.write_all(&serde_json::to_vec(&serde_json::json!({
            "runtime":ProcessIdentity::capture(self.runtime.id()).unwrap(), "host":self.host,
            "deadline_unix_ms":deadline_ms, "report":self.temp.path().join("shutdown-report.json"),
            "owned_paths":[self.temp.path().canonicalize().unwrap()]
        })).unwrap()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_delm"))
            .args(["watchdog", "--spec"])
            .arg(&spec)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        if ready != "ready\n" {
            let mut error = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            panic!("watchdog failed readiness: {error}");
        }
        child
    }
    fn report(&self, watchdog: &mut Child) -> ShutdownReport {
        until(|| watchdog.try_wait().unwrap().is_some());
        assert!(watchdog.wait().unwrap().success());
        let report: ShutdownReport = serde_json::from_slice(
            &fs::read(self.temp.path().join("shutdown-report.json")).unwrap(),
        )
        .unwrap();
        assert!(report.survivors.is_empty(), "{report:?}");
        assert!(!self.host.is_running().unwrap() && !self.tool.is_running().unwrap());
        report
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for id in [self.host, self.tool] {
            if id.is_running().unwrap_or(false) {
                unsafe {
                    libc::kill(id.pid as i32, libc::SIGKILL);
                }
            }
        }
        let _ = self.runtime.kill();
        let _ = self.runtime.wait();
    }
}

#[test]
fn runtime_sigkill_stops_observed_new_sessions_but_preserves_uncertain_work() {
    let mut fixture = Fixture::new();
    let mut unrelated = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let unrelated_id = ProcessIdentity::capture(unrelated.id()).unwrap();
    let mut watchdog = fixture.watchdog(now_ms() + 15_000);
    let partial = fixture.temp.path().join("private-partial-work");
    fs::write(&partial, "saved private edits").unwrap();
    fixture.runtime.kill().unwrap();
    fixture.runtime.wait().unwrap();
    let report = fixture.report(&mut watchdog);
    assert!(report.owned_processes.contains(&fixture.tool));
    assert!(!report.clean() && !report.ownership_resolved);
    assert_eq!(fs::read_to_string(partial).unwrap(), "saved private edits");
    assert!(unrelated_id.is_running().unwrap());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[test]
fn deadline_and_lifetime_eof_both_stop_owned_writers() {
    for close_pipe in [false, true] {
        let fixture = Fixture::with_term_behavior(!close_pipe);
        let mut watchdog = fixture.watchdog(now_ms() + if close_pipe { 15_000 } else { 250 });
        if close_pipe {
            watchdog.stdin.take();
        }
        let report = fixture.report(&mut watchdog);
        assert!(!report.clean() && !report.ownership_resolved);
        assert_eq!(
            report.reason,
            if close_pipe {
                "runtime pipe closed"
            } else {
                "deadline"
            }
        );
    }
}

#[test]
fn cooperative_shutdown_stops_native_new_session_and_keeps_unrelated_process() {
    let fixture = Fixture::new();
    assert_eq!(
        unsafe { libc::getsid(fixture.tool.pid as i32) },
        fixture.tool.pid as i32
    );
    let mut unrelated = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let unrelated_id = ProcessIdentity::capture(unrelated.id()).unwrap();
    let mut watchdog = fixture.watchdog(now_ms() + 15_000);
    watchdog
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"shutdown\nfinish\n")
        .unwrap();
    let report = fixture.report(&mut watchdog);
    assert!(report.clean(), "{report:?}");
    assert!(report.owned_processes.contains(&fixture.tool));
    assert!(unrelated_id.is_running().unwrap());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[test]
fn an_accepted_shutdown_has_bounded_cleanup_grace_at_the_work_deadline() {
    let fixture = Fixture::new();
    let mut watchdog = fixture.watchdog(now_ms() + 1000);
    watchdog
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"shutdown\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    assert!(watchdog.try_wait().unwrap().is_none());
    watchdog
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"finish\n")
        .unwrap();
    let report = fixture.report(&mut watchdog);
    assert!(report.clean(), "{report:?}");
}

#[test]
fn an_unowned_working_directory_or_open_file_vetoes_cleanup_without_signal() {
    for open_file in [false, true] {
        let fixture = Fixture::new();
        let partial = fixture.temp.path().join("private-partial-work");
        fs::write(&partial, "saved private edits").unwrap();
        let mut command = Command::new("/bin/sh");
        if open_file {
            command
                .args(["-c", "exec 3< \"$1\"; exec /bin/sleep 60", "sh"])
                .arg(&partial);
        } else {
            command
                .args(["-c", "exec /bin/sleep 60"])
                .current_dir(fixture.temp.path());
        }
        let mut unowned = command.spawn().unwrap();
        let id = ProcessIdentity::capture(unowned.id()).unwrap();
        let mut watchdog = fixture.watchdog(now_ms() + 15_000);
        watchdog
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"shutdown\nfinish\n")
            .unwrap();
        let report = fixture.report(&mut watchdog);
        assert!(
            !report.clean()
                && report
                    .errors
                    .iter()
                    .any(|e| e.contains(&format!("unowned PID {} references", id.pid))),
            "{report:?}"
        );
        assert!(id.is_running().unwrap());
        assert_eq!(fs::read_to_string(partial).unwrap(), "saved private edits");
        unowned.kill().unwrap();
        unowned.wait().unwrap();
    }
}

#[test]
fn explicit_stop_ends_tools_while_lifetime_pipe_remains_open() {
    let fixture = Fixture::new();
    let mut watchdog = fixture.watchdog(now_ms() + 15_000);
    let partial = fixture.temp.path().join("private-partial-work");
    fs::write(&partial, "saved private edits").unwrap();
    watchdog
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"stop\n")
        .unwrap();
    // Simulate synchronous runtime work that cannot service its main event loop.
    std::thread::sleep(Duration::from_millis(150));
    assert!(watchdog.stdin.is_some());
    let report = fixture.report(&mut watchdog);
    assert_eq!(report.reason, "stop requested");
    assert!(!report.clean());
    assert_eq!(fs::read_to_string(partial).unwrap(), "saved private edits");
}
