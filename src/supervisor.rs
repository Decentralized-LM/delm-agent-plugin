//! Independent stock-host supervision using OS identities, ancestry, and sessions.
//! Abrupt shutdown cannot prove complete ownership and always preserves work.
//! Deliberately daemonized processes are outside this trusted-project backend.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(25);
const GRACE: Duration = Duration::from_secs(2);
const STOP_BOUND: Duration = Duration::from_secs(5);
const MAX_PROCESSES: usize = 131_072;
const SHUTDOWN_BOUND: Duration = Duration::from_secs(15);
const QUIET: Duration = Duration::from_millis(150);
const MAX_FDS: usize = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started_seconds: u64,
    pub started_micros: u64,
    pub uid: u32,
}
#[derive(Clone, Copy)]
struct Process {
    identity: ProcessIdentity,
    parent: u32,
    group: u32,
    session: u32,
    zombie: bool,
}

#[cfg(target_os = "macos")]
fn process(pid: u32) -> Result<Option<Process>> {
    ensure!(pid > 1 && pid <= i32::MAX as u32, "invalid process ID");
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        if [Some(libc::ESRCH), Some(libc::ENOENT)].contains(&error.raw_os_error()) {
            return Ok(None);
        }
        return Err(error).context("inspect owned process identity");
    }
    ensure!(result == size, "incomplete native process identity");
    let info = unsafe { info.assume_init() };
    ensure!(info.pbi_pid == pid, "native process identity mismatch");
    let session = unsafe { libc::getsid(pid as i32) };
    if session < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(error).context("inspect process session");
    }
    Ok(Some(Process {
        identity: ProcessIdentity {
            pid,
            started_seconds: info.pbi_start_tvsec,
            started_micros: info.pbi_start_tvusec,
            uid: info.pbi_uid,
        },
        parent: info.pbi_ppid,
        group: info.pbi_pgid,
        session: session as u32,
        zombie: info.pbi_status == 5,
    }))
}
#[cfg(not(target_os = "macos"))]
fn process(_pid: u32) -> Result<Option<Process>> {
    bail!("process supervision requires macOS")
}

impl ProcessIdentity {
    pub fn is_descendant_of(&self, ancestor: u32) -> Result<bool> {
        if !self.is_running()? {
            return Ok(false);
        }
        let mut pid = self.pid;
        for _ in 0..128 {
            let Some(current) = process(pid)? else {
                return Ok(false);
            };
            if current.parent == ancestor {
                return Ok(true);
            }
            if current.parent <= 1 || current.parent == pid {
                return Ok(false);
            }
            pid = current.parent;
        }
        Ok(false)
    }
    pub fn capture(pid: u32) -> Result<Self> {
        Ok(process(pid)?
            .context("owned process exited before observation")?
            .identity)
    }
    pub fn is_running(&self) -> Result<bool> {
        Ok(process(self.pid)?.is_some_and(|p| p.identity == *self && !p.zombie))
    }
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}
fn snapshot() -> Result<BTreeMap<u32, Process>> {
    #[cfg(target_os = "macos")]
    {
        let mut ids = vec![0i32; MAX_PROCESSES];
        let count = unsafe {
            libc::proc_listallpids(
                ids.as_mut_ptr().cast(),
                (ids.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        ensure!(
            count > 0 && (count as usize) < ids.len(),
            "native process inventory failed or exceeded bound"
        );
        ids.truncate(count as usize);
        let uid = unsafe { libc::geteuid() };
        let mut found = BTreeMap::new();
        for pid in ids.into_iter().filter(|pid| *pid > 1) {
            // Processes of other users may be inaccessible. Owned identities
            // receive a separate mandatory inspection before any signal.
            if let Ok(Some(info)) = process(pid as u32)
                && info.identity.uid == uid
            {
                found.insert(pid as u32, info);
            }
        }
        Ok(found)
    }
    #[cfg(not(target_os = "macos"))]
    {
        bail!("process inventory requires macOS")
    }
}

#[derive(Serialize, Deserialize)]
struct Spec {
    runtime: ProcessIdentity,
    host: ProcessIdentity,
    deadline_unix_ms: u64,
    report: PathBuf,
    owned_paths: Vec<PathBuf>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownReport {
    pub reason: String,
    #[serde(default)]
    pub ownership_resolved: bool,
    pub owned_processes: Vec<ProcessIdentity>,
    pub survivors: Vec<ProcessIdentity>,
    pub errors: Vec<String>,
}
impl ShutdownReport {
    pub fn clean(&self) -> bool {
        self.ownership_resolved && self.survivors.is_empty() && self.errors.is_empty()
    }
}

/// Keep this guard alive until all writers stop. Drop closes the lifetime pipe;
/// the separate process then performs the same bounded shutdown as runtime loss.
pub struct Guard {
    child: Child,
    control: Arc<Mutex<Option<ChildStdin>>>,
    report: PathBuf,
}

/// A cancellation handle never owns the lifetime pipe independently of Guard.
#[derive(Clone)]
pub struct StopHandle {
    control: Arc<Mutex<Option<ChildStdin>>>,
}
impl StopHandle {
    pub fn stop(&self) -> Result<()> {
        if let Some(control) = self
            .control
            .lock()
            .map_err(|_| anyhow::anyhow!("watchdog control lock poisoned"))?
            .as_mut()
        {
            control.write_all(b"stop\n")?;
        }
        Ok(())
    }
}
impl Guard {
    pub fn start(
        runtime_pid: u32,
        host_pid: u32,
        deadline_unix_ms: u64,
        run_dir: &Path,
    ) -> Result<Self> {
        Self::start_with_paths(
            runtime_pid,
            host_pid,
            deadline_unix_ms,
            run_dir,
            &[run_dir.to_path_buf()],
        )
    }

    /// Pass the whole private run root, including worker trees, for the final
    /// metadata-only fence. This never grants permission to signal by path.
    pub fn start_with_paths(
        runtime_pid: u32,
        host_pid: u32,
        deadline_unix_ms: u64,
        run_dir: &Path,
        owned_paths: &[PathBuf],
    ) -> Result<Self> {
        ensure!(
            runtime_pid == std::process::id(),
            "watchdog must be owned by the current runtime"
        );
        let runtime = ProcessIdentity::capture(runtime_pid)?;
        let host = process(host_pid)?.context("native host already exited")?;
        ensure!(
            host.parent == runtime_pid && host.group == host_pid && host.session == host_pid,
            "native host must be a direct child in its own session"
        );
        ensure!(
            host.identity.uid == runtime.uid,
            "native host owner mismatch"
        );
        ensure!(deadline_unix_ms > now_ms()?, "run deadline already expired");
        let root = fs::canonicalize(run_dir)?;
        let metadata = fs::metadata(&root)?;
        ensure!(
            metadata.uid() == runtime.uid && metadata.mode() & 0o077 == 0,
            "watchdog storage must be private and runtime-owned"
        );
        let owned_paths = validate_paths(owned_paths, &root, runtime.uid)?;
        let report = root.join("shutdown-report.json");
        let spec = Spec {
            runtime,
            host: host.identity,
            deadline_unix_ms,
            report: report.clone(),
            owned_paths,
        };
        let path = root.join("watchdog.json");
        write_new(&path, &spec)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .args(["watchdog", "--spec"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("start independent process watchdog")?;
        let output = child
            .stdout
            .take()
            .context("watchdog readiness pipe missing")?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = String::new();
            let read = BufReader::new(output).take(1024).read_line(&mut line);
            let _ = sender.send(read.map(|_| line));
        });
        match receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(line)) if line == "ready\n" => {}
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("independent watchdog did not become ready; no worker turn may start");
            }
        }
        let control = Arc::new(Mutex::new(child.stdin.take()));
        Ok(Self {
            child,
            control,
            report,
        })
    }

    /// Call before bounded native interrupt/terminal-clean/archive requests.
    /// The host may then exit intentionally. A subsequent finish is still
    /// required; a crash or a dropped guard never becomes a clean shutdown.
    pub fn begin_shutdown(&self) -> Result<()> {
        self.control
            .lock()
            .map_err(|_| anyhow::anyhow!("watchdog control lock poisoned"))?
            .as_mut()
            .context("watchdog is stopping")?
            .write_all(b"shutdown\n")?;
        Ok(())
    }

    pub fn stop_handle(&self) -> StopHandle {
        StopHandle {
            control: Arc::clone(&self.control),
        }
    }

    pub fn finish(mut self) -> Result<ShutdownReport> {
        if let Some(control) = self
            .control
            .lock()
            .map_err(|_| anyhow::anyhow!("watchdog control lock poisoned"))?
            .as_mut()
        {
            // A watcher already handling a crash can have closed its input. Its
            // durable unresolved report remains authoritative in that case.
            let _ = control.write_all(b"finish\n");
        }
        self.control
            .lock()
            .map_err(|_| anyhow::anyhow!("watchdog control lock poisoned"))?
            .take();
        let end = Instant::now() + STOP_BOUND + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(
                    status.success(),
                    "watchdog failed; process ownership is unresolved and work must be retained"
                );
                let report: ShutdownReport = serde_json::from_reader(File::open(&self.report)?)?;
                return Ok(report);
            }
            ensure!(
                Instant::now() < end,
                "watchdog shutdown is unresolved; preserve work and do not clean up"
            );
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Cancellation tasks can retain StopHandle clones, but cannot extend
        // the run after the runtime guard leaves scope.
        self.control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }
}

fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    File::open(path.parent().context("watchdog record has no parent")?)?.sync_all()?;
    Ok(())
}

fn validate_paths(paths: &[PathBuf], storage: &Path, uid: u32) -> Result<Vec<PathBuf>> {
    ensure!(
        !paths.is_empty() && paths.len() <= 8,
        "invalid private run paths"
    );
    let storage = fs::canonicalize(storage)?;
    let mut canonical = Vec::new();
    for path in paths {
        let path = fs::canonicalize(path)?;
        let metadata = fs::metadata(&path)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
            "supervised storage must be private and runtime-owned"
        );
        canonical.push(path);
    }
    ensure!(
        canonical.iter().any(|p| storage.starts_with(p)),
        "watchdog is outside private storage"
    );
    Ok(canonical)
}

fn birth(identity: ProcessIdentity) -> (u64, u64) {
    (identity.started_seconds, identity.started_micros)
}

struct Tracker {
    owned: BTreeMap<u32, ProcessIdentity>,
    groups: BTreeMap<u32, ProcessIdentity>,
    sessions: BTreeMap<u32, ProcessIdentity>,
    errors: BTreeSet<String>,
}
impl Tracker {
    fn new(host: ProcessIdentity) -> Self {
        Self {
            owned: BTreeMap::from([(host.pid, host)]),
            groups: BTreeMap::from([(host.pid, host)]),
            sessions: BTreeMap::from([(host.pid, host)]),
            errors: BTreeSet::new(),
        }
    }
    fn discover(&mut self, snapshot: &BTreeMap<u32, Process>) {
        // Retire empty/reused domains. A numeric PGID or SID alone never
        // authorizes adopting processes after its original domain disappears.
        self.groups.retain(|group, leader| {
            snapshot.get(group).is_none_or(|p| p.identity == *leader)
                && snapshot.values().any(|p| p.group == *group)
        });
        self.sessions.retain(|session, leader| {
            snapshot.get(session).is_none_or(|p| p.identity == *leader)
                && snapshot.values().any(|p| p.session == *session)
        });
        loop {
            // An already observed child can call setsid/setpgid between scans.
            // Bind its new domain before looking for orphaned members of it.
            for (pid, identity) in &self.owned {
                if let Some(info) = snapshot.get(pid).filter(|p| p.identity == *identity) {
                    if info.group == *pid {
                        self.groups.insert(*pid, *identity);
                    }
                    if info.session == *pid {
                        self.sessions.insert(*pid, *identity);
                    }
                }
            }
            let mut additions = Vec::new();
            for (pid, info) in snapshot {
                if let Some(known) = self.owned.get(pid) {
                    if known != &info.identity {
                        self.errors.insert(format!(
                            "owned PID {pid} was reused; ownership is unresolved"
                        ));
                    }
                    continue;
                }
                let parent_owned = snapshot.get(&info.parent).is_some_and(|parent| {
                    self.owned.get(&info.parent) == Some(&parent.identity)
                        && birth(info.identity) >= birth(parent.identity)
                });
                let group_owned = self
                    .groups
                    .get(&info.group)
                    .is_some_and(|leader| birth(info.identity) >= birth(*leader));
                let session_owned = self
                    .sessions
                    .get(&info.session)
                    .is_some_and(|leader| birth(info.identity) >= birth(*leader));
                if parent_owned || group_owned || session_owned {
                    additions.push((*pid, info.identity));
                }
            }
            if additions.is_empty() {
                break;
            }
            self.owned.extend(additions);
            if self.owned.len() > MAX_PROCESSES {
                self.errors.insert("owned process bound exceeded".into());
                break;
            }
        }
    }
    fn signal(&mut self, signal: i32) {
        for identity in self.owned.values() {
            match identity.is_running() {
                Ok(true) => {
                    // No process-name matching, cwd-based kill, or unfenced killpg.
                    if unsafe { libc::kill(identity.pid as i32, signal) } != 0 {
                        let error = std::io::Error::last_os_error();
                        if error.raw_os_error() != Some(libc::ESRCH) {
                            self.errors.insert(format!(
                                "could not signal owned PID {}: {error}",
                                identity.pid
                            ));
                        }
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    self.errors.insert(format!(
                        "could not verify owned PID {}: {error}",
                        identity.pid
                    ));
                }
            }
        }
    }
    fn running(&mut self) -> Vec<ProcessIdentity> {
        self.owned
            .values()
            .filter_map(|identity| match identity.is_running() {
                Ok(true) => Some(*identity),
                Ok(false) => None,
                Err(error) => {
                    self.errors
                        .insert(format!("cannot resolve PID {}: {error}", identity.pid));
                    Some(*identity)
                }
            })
            .collect()
    }
    fn refresh(&mut self) -> Option<BTreeMap<u32, Process>> {
        match snapshot() {
            Ok(snapshot) => {
                self.discover(&snapshot);
                Some(snapshot)
            }
            Err(error) => {
                self.errors.insert(error.to_string());
                None
            }
        }
    }
}

// Native metadata queries reveal paths only, never file contents, argv, or env.
// Stock shell sessions can fork a background child and exit between snapshots.
// Such an unowned reference vetoes cleanup but never grants signal authority.
#[cfg(target_os = "macos")]
fn references_run(pid: u32, roots: &[PathBuf]) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    fn matches(raw: &[[libc::c_char; 32]; 32], roots: &[PathBuf]) -> bool {
        let bytes: Vec<_> = raw
            .iter()
            .flatten()
            .take_while(|b| **b != 0)
            .map(|b| *b as u8)
            .collect();
        let path = Path::new(std::ffi::OsStr::from_bytes(&bytes));
        roots.iter().any(|root| path.starts_with(root))
    }
    let mut cwd = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of_val(&cwd) as i32;
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            cwd.as_mut_ptr().cast(),
            size,
        )
    };
    if result <= 0 {
        return Err(std::io::Error::last_os_error())
            .context("inspect process working directory metadata");
    }
    ensure!(
        result == size,
        "incomplete process working directory metadata"
    );
    let cwd = unsafe { cwd.assume_init() };
    if matches(&cwd.pvi_cdir.vip_path, roots) || matches(&cwd.pvi_rdir.vip_path, roots) {
        return Ok(true);
    }
    let size = std::mem::size_of::<libc::proc_fdinfo>();
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if bytes < 0 {
        return Err(std::io::Error::last_os_error()).context("inspect process descriptor count");
    }
    let count = bytes as usize / size + 32;
    ensure!(
        count <= MAX_FDS,
        "process descriptor inventory exceeded bound"
    );
    let mut fds: Vec<libc::proc_fdinfo> = (0..count)
        .map(|_| libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        })
        .collect();
    let capacity = (count * size) as i32;
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            capacity,
        )
    };
    if bytes < 0 {
        return Err(std::io::Error::last_os_error()).context("inspect process descriptor metadata");
    }
    ensure!(
        bytes < capacity && (bytes as usize).is_multiple_of(size),
        "process descriptor inventory changed beyond bound"
    );
    #[repr(C)]
    struct FileInfo {
        flags: u32,
        status: u32,
        offset: libc::off_t,
        kind: i32,
        guard: u32,
    }
    #[repr(C)]
    struct VnodeFd {
        file: FileInfo,
        vnode: libc::vnode_info_path,
    }
    for fd in fds
        .iter()
        .take(bytes as usize / size)
        .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32)
    {
        let mut info = std::mem::MaybeUninit::<VnodeFd>::zeroed();
        let size = std::mem::size_of_val(&info) as i32;
        let result = unsafe {
            libc::proc_pidfdinfo(
                pid as i32,
                fd.proc_fd,
                2, /* PROC_PIDFDVNODEPATHINFO */
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if result <= 0 {
            let error = std::io::Error::last_os_error();
            // A descriptor can close between inventory and inspection.
            if [Some(libc::EBADF), Some(libc::ENOENT), Some(libc::ESRCH)]
                .contains(&error.raw_os_error())
            {
                continue;
            }
            return Err(error).context("inspect process vnode metadata");
        }
        ensure!(result == size, "incomplete process vnode metadata");
        if matches(&unsafe { info.assume_init() }.vnode.vip_path, roots) {
            return Ok(true);
        }
    }
    Ok(false)
}
#[cfg(not(target_os = "macos"))]
fn references_run(_pid: u32, _roots: &[PathBuf]) -> Result<bool> {
    bail!("process metadata requires macOS")
}

fn reference_fence(tracker: &mut Tracker, spec: &Spec, snapshot: &BTreeMap<u32, Process>) {
    for info in snapshot
        .values()
        .filter(|p| {
            !p.zombie
                && p.identity.pid != spec.runtime.pid
                && p.identity.pid != std::process::id()
                && birth(p.identity) >= birth(spec.host)
                && tracker.owned.get(&p.identity.pid) != Some(&p.identity)
        })
        .collect::<Vec<_>>()
    {
        let result = references_run(info.identity.pid, &spec.owned_paths);
        // Discard metadata from a process that exited or was reused meanwhile.
        if !info.identity.is_running().unwrap_or(true) {
            continue;
        }
        match result {
            Ok(true) => {
                tracker.errors.insert(format!(
                    "unowned PID {} references private run storage; preserve all work",
                    info.identity.pid
                ));
            }
            Ok(false) => {}
            Err(error) => {
                tracker.errors.insert(format!(
                    "cannot exclude a private run reference for PID {}: {error}",
                    info.identity.pid
                ));
            }
        }
    }
}

#[derive(Default)]
struct Control {
    bytes: Vec<u8>,
    shutdown: Option<Instant>,
    stop: Option<Instant>,
    finished: bool,
    eof: bool,
}
impl Control {
    fn read(&mut self, input: &mut impl Read) -> Result<()> {
        let mut consumed = 0;
        loop {
            let mut buffer = [0u8; 1024];
            match input.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(n) => {
                    consumed += n;
                    ensure!(consumed <= 4096, "watchdog control rate exceeded bound");
                    self.bytes.extend_from_slice(&buffer[..n]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
            ensure!(self.bytes.len() <= 4096, "watchdog control exceeded bound");
            while let Some(end) = self.bytes.iter().position(|b| *b == b'\n') {
                match &self.bytes[..end] {
                    b"shutdown" => {
                        self.shutdown.get_or_insert_with(Instant::now);
                    }
                    b"stop" => {
                        self.stop.get_or_insert_with(Instant::now);
                    }
                    b"finish" => self.finished = true,
                    _ => bail!("invalid watchdog control message"),
                }
                self.bytes.drain(..=end);
            }
        }
        Ok(())
    }
}

/// Internal CLI entry point. No model or project code may call this channel.
pub fn watchdog(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.len() <= 16 * 1024
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "invalid watchdog specification"
    );
    let mut spec: Spec = serde_json::from_reader(file)?;
    ensure!(
        spec.runtime.is_running()? && spec.host.is_running()?,
        "watchdog owners are not live"
    );
    let host = process(spec.host.pid)?.context("native host disappeared")?;
    ensure!(
        host.parent == spec.runtime.pid
            && host.group == spec.host.pid
            && host.session == spec.host.pid,
        "native host ownership changed"
    );
    ensure!(
        spec.report.parent() == path.parent(),
        "watchdog report must belong to its private run"
    );
    spec.owned_paths = validate_paths(
        &spec.owned_paths,
        path.parent().context("missing watchdog storage")?,
        spec.runtime.uid,
    )?;
    let stdin = std::io::stdin();
    let flags = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL) };
    ensure!(
        flags >= 0
            && unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == 0,
        "cannot watch runtime lifetime pipe"
    );
    let mut input = stdin.lock();
    let mut tracker = Tracker::new(spec.host);
    let monotonic_end =
        Instant::now() + Duration::from_millis(spec.deadline_unix_ms.saturating_sub(now_ms()?));
    let mut control = Control::default();
    tracker.discover(&snapshot()?);
    println!("ready");
    std::io::stdout().flush()?;
    let reason = loop {
        if let Err(error) = control.read(&mut input) {
            tracker.errors.insert(error.to_string());
            break "runtime pipe failed";
        }
        if control.finished {
            break "shutdown complete";
        }
        if control.eof {
            break "runtime pipe closed";
        }
        match spec.runtime.is_running() {
            Ok(true) => {}
            Ok(false) => break "runtime exited",
            Err(error) => {
                tracker.errors.insert(error.to_string());
                break "runtime identity unresolved";
            }
        }
        match spec.host.is_running() {
            Ok(false) if control.shutdown.is_none() => break "native host exited",
            Ok(_) => {}
            Err(error) => {
                tracker.errors.insert(error.to_string());
                break "native host identity unresolved";
            }
        }
        if control
            .shutdown
            .is_some_and(|start| start.elapsed() >= SHUTDOWN_BOUND)
        {
            break "native shutdown timed out";
        }
        if control.stop.is_some_and(|start| start.elapsed() >= GRACE) {
            break "stop requested";
        }
        let wall_time = match now_ms() {
            Ok(now) => now,
            Err(error) => {
                tracker.errors.insert(error.to_string());
                break "wall clock unavailable";
            }
        };
        // Runtime has already ended model work when it sends shutdown. Its
        // native interruption/archive may finish during bounded cleanup grace.
        // If the deadline wins before this marker, retain unresolved work.
        if control.shutdown.is_none()
            && (Instant::now() >= monotonic_end || wall_time >= spec.deadline_unix_ms)
        {
            break "deadline";
        }
        if tracker.refresh().is_none() {
            break "process inventory failed";
        }
        std::thread::sleep(POLL);
    }
    .to_owned();
    let cooperative = control.finished
        && control.shutdown.is_some()
        && spec.runtime.is_running().unwrap_or(false);
    if !cooperative {
        tracker.errors.insert("native shutdown was not confirmed; ownership is unresolved, preserve all work and do not automatically resume".into());
    }
    let end = Instant::now() + STOP_BOUND;
    let force = Instant::now() + GRACE;
    let mut quiet_since = None;
    loop {
        tracker.refresh();
        tracker.signal(if Instant::now() >= force {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        });
        if tracker.running().is_empty() {
            let quiet = quiet_since.get_or_insert_with(Instant::now);
            if quiet.elapsed() >= QUIET {
                break;
            }
        } else {
            quiet_since = None;
        }
        if Instant::now() >= end {
            break;
        }
        std::thread::sleep(POLL);
    }
    // Two metadata inventories after all observed writers stop. Path references
    // are evidence of uncertainty, never evidence of process ownership.
    for pass in 0..2 {
        if pass != 0 {
            std::thread::sleep(QUIET);
        }
        if let Some(snapshot) = tracker.refresh() {
            // A late owned child never gets a free pass because the first
            // quiet interval happened before its observation.
            tracker.signal(libc::SIGKILL);
            reference_fence(&mut tracker, &spec, &snapshot);
        }
    }
    while !tracker.running().is_empty() && Instant::now() < end {
        tracker.refresh();
        tracker.signal(libc::SIGKILL);
        std::thread::sleep(POLL);
    }
    let survivors = tracker.running();
    let report = ShutdownReport {
        reason,
        ownership_resolved: cooperative && tracker.errors.is_empty() && survivors.is_empty(),
        owned_processes: tracker.owned.values().copied().collect(),
        survivors,
        errors: tracker.errors.into_iter().collect(),
    };
    write_new(&spec.report, &report)?;
    Ok(())
}
