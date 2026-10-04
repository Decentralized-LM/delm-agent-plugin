//! Shared preview ownership. Servers still launch through normal native tools,
//! so requesting a preview cannot bypass the user's execution permissions.
use crate::{board::Board, supervisor::ProcessIdentity};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize)]
struct Service {
    owner: usize,
    source_worker: usize,
    paths: Vec<String>,
    input_digest: String,
    #[serde(skip)]
    inputs: Value,
    #[serde(skip)]
    claimed_at_micros: u64,
    generation: String,
    revision: String,
    state: String,
    url: Option<String>,
    process: Option<ProcessIdentity>,
    reader: Option<usize>,
    invalidated_check: bool,
}

#[derive(Default)]
pub struct Services {
    entries: BTreeMap<String, Service>,
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"name":"delm_service","description":"Coordinate one shared preview per named service. Claim with explicit served-input paths before launching through your normal execution tool. Source/config/lockfile hashes bind the preview to actual inputs, not its revision label. If a peer owns it, reuse its URL when ready. Start on an automatically assigned loopback port; ready records the actual URL, server PID and served code revision. Acquire/release_check gives exclusive ownership of a shared browser/database scenario and verifies the served scope matches your imported files; release_check reports whether scoped inputs changed during the check. The scope does not prove undeclared dependencies or external state. Stop the process before release. Do not edit a served checked revision during its check.",
        "inputSchema":{"type":"object","additionalProperties":false,"properties":{
            "action":{"enum":["list","claim","ready","acquire_check","release_check","release"]},
            "paths":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":2048},
            "name":{"type":"string"},"generation":{"type":"string"},"revision":{"type":"string"},
            "url":{"type":"string"},"pid":{"type":"integer","minimum":2}},"required":["action"]}}),
    ]
}

fn field<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 2048)
        .with_context(|| format!("Missing or invalid {key}"))
}

fn loopback_port(url: &str) -> Result<u16> {
    let tail = url
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| url.strip_prefix("http://localhost:"))
        .context("Preview URL must use http://127.0.0.1:<port> or http://localhost:<port>")?;
    let port: u16 = tail.split('/').next().context("Missing port")?.parse()?;
    ensure!(port > 0, "Report the actual bound port, not port zero");
    Ok(port)
}

/// Check the actual bound socket of the already-owned process. A convenient
/// loopback URL and an unrelated descendant PID are not proof of readiness.
fn verify_listener(process: ProcessIdentity, url: &str) -> Result<()> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let port = loopback_port(url)?;
    let accepts_ipv6 = url.starts_with("http://localhost:");
    ensure!(process.is_running()?, "Preview process has stopped");
    let mut child = Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &process.pid.to_string(),
            &format!("-iTCP:{port}"),
            "-sTCP:LISTEN",
            "-Fpn",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Inspect preview listener ownership")?;
    let stdout = child
        .stdout
        .take()
        .context("Listener inspection output missing")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes)
    });
    let until = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(anyhow::anyhow!("Preview listener inspection timed out"));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error.into());
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!("Listener inspection reader failed"))??;
    ensure!(
        status?.success() && bytes.len() <= 65536,
        "Preview PID does not own the reported listening port"
    );
    let output = std::str::from_utf8(&bytes)?;
    ensure!(
        output
            .lines()
            .any(|line| line == format!("p{}", process.pid))
            && output
                .lines()
                .any(|line| line == format!("n127.0.0.1:{port}")
                    || (accepts_ipv6 && line == format!("n[::1]:{port}"))),
        "Preview must bind the reported port on loopback"
    );
    ensure!(
        process.is_running()?,
        "Preview process changed during listener inspection"
    );
    Ok(())
}

fn scope(args: &Value) -> Result<Vec<String>> {
    args["paths"]
        .as_array()
        .context("claim requires explicit served-input paths")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .context("service paths must be strings")
        })
        .collect()
}

fn now_micros() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_micros()
        .try_into()?)
}

#[cfg(target_os = "macos")]
fn verify_cwd(process: ProcessIdentity, project: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of_val(&info) as i32;
    let result = unsafe {
        libc::proc_pidinfo(
            process.pid as i32,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    ensure!(
        result == size,
        "Could not verify preview process working directory"
    );
    let info = unsafe { info.assume_init() };
    let bytes = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect::<Vec<_>>();
    let cwd = std::fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(&bytes)))?;
    ensure!(
        cwd.starts_with(project),
        "Preview process must run from the scoped worker project"
    );
    ensure!(
        process.is_running()?,
        "Preview process changed during directory inspection"
    );
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn verify_cwd(_process: ProcessIdentity, _project: &Path) -> Result<()> {
    bail!("Preview ownership inspection requires macOS")
}

fn refresh(entry: &mut Service, board: &Board) -> Result<()> {
    let current = board.input_snapshot(entry.source_worker, &entry.paths)?;
    if current != entry.inputs {
        entry.state = "stale".into();
        if entry.reader.is_some() {
            entry.invalidated_check = true;
        }
    }
    if entry
        .process
        .is_some_and(|p| !p.is_running().unwrap_or(false))
    {
        entry.state = "stopped".into();
        if entry.reader.is_some() {
            entry.invalidated_check = true;
        }
    }
    Ok(())
}

impl Services {
    pub fn call(&mut self, worker: usize, args: Value, host: u32, board: &Board) -> Result<Value> {
        ensure!((1..=2).contains(&worker), "Unknown worker");
        let action = field(&args, "action")?;
        if action == "list" {
            for entry in self.entries.values_mut() {
                refresh(entry, board)?;
            }
            return Ok(json!({"services":self.entries}));
        }
        let name = field(&args, "name")?;
        ensure!(
            name.len() <= 80
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "Use a short service name"
        );
        if action == "claim" {
            let revision = field(&args, "revision")?;
            let paths = scope(&args)?;
            let inputs = board.input_snapshot(worker, &paths)?;
            if let Some(entry) = self.entries.get_mut(name) {
                refresh(entry, board)?;
                return Ok(
                    json!({"claimed":false,"already_owned":entry.owner == worker, "service":entry,
                    "requested_revision_matches":entry.revision == revision,"requested_inputs_match":entry.inputs == inputs}),
                );
            }
            ensure!(self.entries.len() < 16, "Too many preview services");
            let entry = Service {
                owner: worker,
                source_worker: worker,
                generation: uuid::Uuid::new_v4().to_string(),
                revision: revision.into(),
                input_digest: format!("{:x}", Sha256::digest(serde_json::to_vec(&inputs)?)),
                inputs,
                paths,
                claimed_at_micros: now_micros()?,
                state: "starting".into(),
                url: None,
                process: None,
                reader: None,
                invalidated_check: false,
            };
            let response = json!({"claimed":true,"service":entry});
            self.entries.insert(name.into(), entry);
            return Ok(response);
        }
        let entry = self
            .entries
            .get_mut(name)
            .context("Claim the service first")?;
        ensure!(
            field(&args, "generation")? == entry.generation,
            "Stale service generation; read the current registry"
        );
        match action {
            "ready" => {
                ensure!(
                    entry.owner == worker && entry.reader.is_none(),
                    "Only the owner can publish an unchecked preview"
                );
                ensure!(
                    entry.state == "starting" || entry.state == "ready",
                    "Preview inputs changed or process stopped; release it and claim a fresh generation before restarting"
                );
                ensure!(
                    entry.revision == field(&args, "revision")?,
                    "Ready must use the claimed revision"
                );
                ensure!(
                    board.input_snapshot(entry.source_worker, &entry.paths)? == entry.inputs,
                    "Preview inputs changed during startup; stop it and claim a fresh generation"
                );
                let url = field(&args, "url")?;
                loopback_port(url)?;
                let pid: u32 = args["pid"]
                    .as_u64()
                    .context("Missing server PID")?
                    .try_into()?;
                let process = ProcessIdentity::capture(pid)?;
                ensure!(
                    process.is_descendant_of(host)?,
                    "Preview process is not owned by this run"
                );
                ensure!(
                    process
                        .started_seconds
                        .saturating_mul(1_000_000)
                        .saturating_add(process.started_micros)
                        >= entry.claimed_at_micros,
                    "Preview process predates the input snapshot; launch it after claiming this service"
                );
                ensure!(
                    entry.process.is_none_or(|old| old == process),
                    "A live service generation cannot replace its owned process"
                );
                verify_cwd(process, board.worker_path(entry.source_worker)?)?;
                verify_listener(process, url)?;
                entry.process = Some(process);
                entry.url = Some(url.into());
                entry.state = "ready".into();
            }
            "acquire_check" => {
                refresh(entry, board)?;
                ensure!(
                    entry.state == "ready",
                    "Preview stopped or its scoped inputs changed; restart against the current contribution"
                );
                ensure!(
                    entry.revision == field(&args, "revision")?,
                    "Preview serves a different revision"
                );
                ensure!(
                    board.input_snapshot(worker, &entry.paths)? == entry.inputs,
                    "Import the served input versions before checking this preview"
                );
                ensure!(
                    entry.reader.is_none_or(|w| w == worker),
                    "Another worker owns this shared preview check; do independent work or use an independent browser context"
                );
                verify_listener(
                    entry.process.context("Preview process is missing")?,
                    entry.url.as_deref().context("Preview URL is missing")?,
                )?;
                entry.reader = Some(worker);
                entry.invalidated_check = false;
            }
            "release_check" => {
                ensure!(entry.reader == Some(worker), "You do not own this check");
                let current = refresh(entry, board);
                let same = current.is_ok()
                    && board
                        .input_snapshot(worker, &entry.paths)
                        .is_ok_and(|inputs| inputs == entry.inputs);
                let listening = entry
                    .process
                    .zip(entry.url.as_deref())
                    .is_some_and(|(process, url)| verify_listener(process, url).is_ok());
                let valid = same && listening && !entry.invalidated_check && entry.state == "ready";
                entry.reader = None;
                if !valid {
                    entry.invalidated_check = true;
                    if !listening && entry.state == "ready" {
                        entry.state = "stale".into();
                    }
                }
                return Ok(json!({"released":true,"check_valid":valid,"service":entry}));
            }
            "release" => {
                ensure!(
                    entry.owner == worker && entry.reader.is_none(),
                    "Only the owner may release an unchecked service"
                );
                ensure!(
                    entry
                        .process
                        .is_none_or(|p| !p.is_running().unwrap_or(true)),
                    "Stop the owned preview through its native process handle before releasing it"
                );
                self.entries.remove(name);
                return Ok(json!({"released":true}));
            }
            _ => bail!("Unknown service action"),
        }
        Ok(json!({"service":entry}))
    }

    /// Release a stopped worker's check leases. Empty/dead service claims are
    /// removed; a live owned server transfers to the peer with a new generation
    /// and retains its source scope. No process is signalled by this operation.
    pub fn retire_worker(&mut self, worker: usize) -> Result<Value> {
        ensure!((1..=2).contains(&worker), "Unknown worker");
        let mut removed = Vec::new();
        let mut transferred = Vec::new();
        for (name, entry) in &mut self.entries {
            if entry.reader == Some(worker) {
                entry.reader = None;
                entry.invalidated_check = true;
            }
            if entry.owner != worker {
                continue;
            }
            if entry
                .process
                .map(|p| p.is_running())
                .transpose()?
                .unwrap_or(false)
            {
                entry.owner = 3 - worker;
                entry.generation = uuid::Uuid::new_v4().to_string();
                transferred.push(name.clone());
            } else {
                removed.push(name.clone());
            }
        }
        for name in &removed {
            self.entries.remove(name);
        }
        Ok(json!({"removed":removed,"transferred":transferred}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        path::PathBuf,
        process::{Child, Command, Stdio},
    };

    struct Fixture {
        _temp: tempfile::TempDir,
        board: Board,
        workers: [PathBuf; 2],
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let run = temp.path().join("run");
            let baseline = temp.path().join("baseline");
            let workers = [temp.path().join("worker1"), temp.path().join("worker2")];
            for path in [&run, &baseline, &workers[0], &workers[1]] {
                fs::create_dir(path).unwrap();
            }
            for path in [&baseline, &workers[0], &workers[1]] {
                fs::write(path.join("main.js"), "ready").unwrap();
            }
            let mut board = Board::open(&run, &baseline, workers.clone()).unwrap();
            for worker in 1..=2 {
                board.set_worker_policy(worker,&json!({"default_permissions":"test","permissions":{"test":{"filesystem":{workers[worker-1].to_string_lossy().as_ref():"write"}}}})).unwrap();
            }
            Self {
                _temp: temp,
                board,
                workers,
            }
        }
    }
    struct Server(Child, u16);
    impl Server {
        fn start(root: &Path) -> Self {
            let mut child=Command::new("/usr/bin/python3").args(["-u","-c","import socket,sys,time; s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); print(s.getsockname()[1],flush=True); sys.stdin.readline(); s.close(); time.sleep(30)"])
                .current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            Self(child, line.trim().parse().unwrap())
        }
        fn stop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop();
        }
    }
    fn claim() -> Value {
        json!({"action":"claim","name":"web","revision":"publication:7","paths":["main.js"]})
    }
    fn ready(generation: &Value, server: &Server) -> Value {
        json!({"action":"ready","name":"web","generation":generation,"revision":"publication:7","url":format!("http://127.0.0.1:{}/",server.1),"pid":server.0.id()})
    }

    #[test]
    fn two_workers_reuse_one_claim_and_stale_releases_fail() {
        let fixture = Fixture::new();
        let mut services = Services::default();
        let board = &fixture.board;
        let one = services.call(1, claim(), 0, board).unwrap();
        let two = services.call(2, claim(), 0, board).unwrap();
        assert_eq!(two["claimed"], false);
        assert_eq!(one["service"], two["service"]);
        assert_eq!(two["requested_inputs_match"], true);
        let mut changed = claim();
        changed["revision"] = json!("another-version");
        let repeat = services.call(1, changed, 0, board).unwrap();
        assert_eq!(repeat["claimed"], false);
        assert_eq!(repeat["already_owned"], true);
        assert_eq!(repeat["requested_revision_matches"], false);
        fs::write(fixture.workers[1].join("main.js"), "different").unwrap();
        assert_eq!(
            services.call(2, claim(), 0, board).unwrap()["requested_inputs_match"],
            false
        );
        assert!(services.call(2,json!({"action":"release","name":"web","generation":one["service"]["generation"]}),0,board).is_err());
        assert!(
            services
                .call(
                    1,
                    json!({"action":"release","name":"web","generation":"old"}),
                    0,
                    board
                )
                .is_err()
        );
        services
            .call(
                1,
                json!({"action":"release","name":"web","generation":one["service"]["generation"]}),
                0,
                board,
            )
            .unwrap();
        assert!(services.entries.is_empty());
    }

    #[test]
    fn only_actual_loopback_ports_are_accepted() {
        assert_eq!(loopback_port("http://127.0.0.1:4311/").unwrap(), 4311);
        for url in [
            "http://localhost:0/",
            "http://example.com:4311/",
            "http://localhost:4311@evil/",
        ] {
            assert!(loopback_port(url).is_err());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn listener_check_rejects_a_live_pid_without_the_reported_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let process = ProcessIdentity::capture(std::process::id()).unwrap();
        verify_listener(process, &format!("http://127.0.0.1:{port}/")).unwrap();
        drop(listener);
        assert!(verify_listener(process, &format!("http://127.0.0.1:{port}/")).is_err());
        let ipv6 = std::net::TcpListener::bind("[::1]:0").unwrap();
        let port = ipv6.local_addr().unwrap().port();
        verify_listener(process, &format!("http://localhost:{port}/")).unwrap();
        assert!(verify_listener(process, &format!("http://127.0.0.1:{port}/")).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn closing_the_socket_while_the_server_process_lives_invalidates_the_check() {
        let fixture = Fixture::new();
        let board = &fixture.board;
        let mut services = Services::default();
        let host = std::process::id();
        let claimed = services.call(1, claim(), host, board).unwrap();
        let generation = &claimed["service"]["generation"];
        let mut server = Server::start(&fixture.workers[0]);
        services
            .call(1, ready(generation, &server), host, board)
            .unwrap();
        services.call(2,json!({"action":"acquire_check","name":"web","generation":generation,"revision":"publication:7"}),host,board).unwrap();
        server
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"close\n")
            .unwrap();
        let process = ProcessIdentity::capture(server.0.id()).unwrap();
        let url = format!("http://127.0.0.1:{}/", server.1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while verify_listener(process, &url).is_ok() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(process.is_running().unwrap());
        let result = services
            .call(
                2,
                json!({"action":"release_check","name":"web","generation":generation}),
                host,
                board,
            )
            .unwrap();
        assert_eq!(result["check_valid"], false);
        assert_eq!(result["service"]["state"], "stale");
        assert_eq!(result["service"]["reader"], Value::Null);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn checks_bind_both_peers_to_actual_served_inputs_and_invalidate_mutations() {
        let fixture = Fixture::new();
        let board = &fixture.board;
        let mut services = Services::default();
        let host = std::process::id();
        let claimed = services.call(1, claim(), host, board).unwrap();
        let generation = &claimed["service"]["generation"];
        let mut server = Server::start(&fixture.workers[0]);
        services
            .call(1, ready(generation, &server), host, board)
            .unwrap();
        let acquire = json!({"action":"acquire_check","name":"web","generation":generation,"revision":"publication:7"});
        fs::write(fixture.workers[1].join("main.js"), "different").unwrap();
        assert!(services.call(2, acquire.clone(), host, board).is_err());
        fs::write(fixture.workers[1].join("main.js"), "ready").unwrap();
        services.call(2, acquire.clone(), host, board).unwrap();
        assert!(services.call(1, acquire.clone(), host, board).is_err());
        fs::write(fixture.workers[0].join("main.js"), "changed while checking").unwrap();
        let release = services
            .call(
                2,
                json!({"action":"release_check","name":"web","generation":generation}),
                host,
                board,
            )
            .unwrap();
        assert_eq!(release["check_valid"], false);
        assert_eq!(release["service"]["state"], "stale");
        assert!(
            services
                .call(1, ready(generation, &server), host, board)
                .is_err()
        );
        assert!(services.call(2, acquire, host, board).is_err());
        assert!(
            services
                .call(
                    1,
                    json!({"action":"release","name":"web","generation":generation}),
                    host,
                    board
                )
                .is_err()
        );
        server.stop();
        services
            .call(
                1,
                json!({"action":"release","name":"web","generation":generation}),
                host,
                board,
            )
            .unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn readiness_rejects_old_or_other_project_processes_and_wrong_ports() {
        let fixture = Fixture::new();
        let board = &fixture.board;
        let mut services = Services::default();
        let host = std::process::id();
        let old = Server::start(&fixture.workers[0]);
        let claimed = services.call(1, claim(), host, board).unwrap();
        let generation = &claimed["service"]["generation"];
        assert!(
            services
                .call(1, ready(generation, &old), host, board)
                .is_err()
        );
        let unrelated = Server::start(&fixture.workers[1]);
        assert!(
            services
                .call(1, ready(generation, &unrelated), host, board)
                .is_err()
        );
        let owner = Server::start(&fixture.workers[0]);
        let mut wrong = ready(generation, &owner);
        wrong["url"] = json!(format!("http://127.0.0.1:{}/", unrelated.1));
        assert!(services.call(1, wrong, host, board).is_err());
        assert!(
            ProcessIdentity::capture(unrelated.0.id())
                .unwrap()
                .is_running()
                .unwrap()
        );
        services
            .call(1, ready(generation, &owner), host, board)
            .unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn stopped_worker_transfers_owned_live_preview_without_signalling_it() {
        let fixture = Fixture::new();
        let board = &fixture.board;
        let mut services = Services::default();
        let host = std::process::id();
        let claimed = services.call(1, claim(), host, board).unwrap();
        let generation = &claimed["service"]["generation"];
        let mut server = Server::start(&fixture.workers[0]);
        services
            .call(1, ready(generation, &server), host, board)
            .unwrap();
        services.call(1,json!({"action":"acquire_check","name":"web","generation":generation,"revision":"publication:7"}),host,board).unwrap();
        let retired = services.retire_worker(1).unwrap();
        assert_eq!(retired["transferred"], json!(["web"]));
        let adopted = services.call(2, claim(), host, board).unwrap();
        let next = &adopted["service"]["generation"];
        assert_eq!(adopted["already_owned"], true);
        assert_eq!(adopted["service"]["source_worker"], 1);
        assert_ne!(next, generation);
        assert!(
            ProcessIdentity::capture(server.0.id())
                .unwrap()
                .is_running()
                .unwrap()
        );
        services.call(2,json!({"action":"acquire_check","name":"web","generation":next,"revision":"publication:7"}),host,board).unwrap();
        let checked = services
            .call(
                2,
                json!({"action":"release_check","name":"web","generation":next}),
                host,
                board,
            )
            .unwrap();
        assert_eq!(checked["check_valid"], true);
        server.stop();
        assert_eq!(
            services.retire_worker(2).unwrap()["removed"],
            json!(["web"])
        );
    }
}
