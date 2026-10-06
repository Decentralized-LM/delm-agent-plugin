use super::*;

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Ownership {
    pub(super) version: u8,
    pub(super) original: PathBuf,
    #[serde(default)]
    pub(super) original_identity: Option<(u64, u64)>,
    pub(super) root: (u64, u64),
    pub(super) children: BTreeMap<String, (u64, u64)>,
}

pub(super) fn validate_ownership_roster(
    prepared: &PreparedWorkspace,
    ownership: &Ownership,
) -> Result<()> {
    prepared.validate_layout()?;
    let required: BTreeSet<_> = std::iter::once("baseline".to_owned())
        .chain((1..=prepared.workers.len()).map(|worker| format!("worker-{worker}")))
        .collect();
    ensure!(
        required
            .iter()
            .all(|name| ownership.children.contains_key(name))
            && ownership.children.keys().all(|name| {
                required.contains(name) || matches!(name.as_str(), "capture-0" | "capture-1")
            }),
        "workspace ownership roster does not match selected workers"
    );
    Ok(())
}

pub(super) fn identity(path: &Path) -> Result<(u64, u64)> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "owned directory identity changed"
    );
    Ok((m.dev(), m.ino()))
}
pub(super) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    open_dir(path.parent().context("metadata path has no parent")?)?.sync_all()?;
    Ok(())
}

/// Each directory is registered before any project data is written into it.
/// Preparation has no worker writers, so failed captures are disposable copies.
struct PreparationGuard {
    workspace: PathBuf,
    root: (u64, u64),
    children: BTreeMap<String, (u64, u64)>,
    journal: File,
}

impl PreparationGuard {
    fn new(workspace: &Path) -> Result<Self> {
        let root = identity(workspace)?;
        let journal = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(workspace.join("preparation-ownership.jsonl"))?;
        let mut guard = Self {
            workspace: workspace.to_owned(),
            root,
            children: BTreeMap::new(),
            journal,
        };
        guard.record("root", "", root)?;
        Ok(guard)
    }

    fn record(&mut self, operation: &str, name: &str, identity: (u64, u64)) -> Result<()> {
        serde_json::to_writer(
            &mut self.journal,
            &serde_json::json!({
                "operation": operation, "name": name, "identity": identity,
            }),
        )?;
        self.journal.write_all(b"\n")?;
        self.journal.sync_all()?;
        open_dir(&self.workspace)?.sync_all()?;
        Ok(())
    }

    fn create(&mut self, name: &str) -> Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt;
        ensure!(
            identity(&self.workspace)? == self.root,
            "preparation root changed"
        );
        let path = self.workspace.join(name);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let owned = identity(&path)?;
        // Register in memory before a journal write can fail.
        self.children.insert(name.to_owned(), owned);
        self.record("create", name, owned)?;
        Ok(path)
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<PathBuf> {
        ensure!(
            identity(&self.workspace)? == self.root,
            "preparation root changed"
        );
        let owned = *self.children.get(old).context("unowned preparation path")?;
        ensure!(
            identity(&self.workspace.join(old))? == owned,
            "capture changed"
        );
        ensure!(
            !self.workspace.join(new).try_exists()?,
            "capture destination exists"
        );
        self.record("rename_intent", new, owned)?;
        fs::rename(self.workspace.join(old), self.workspace.join(new))?;
        self.children.remove(old);
        self.children.insert(new.to_owned(), owned);
        self.record("rename", new, owned)?;
        Ok(self.workspace.join(new))
    }

    fn cleanup(&mut self) -> Result<()> {
        ensure!(
            identity(&self.workspace)? == self.root,
            "preparation root changed; preserved"
        );
        // Preflight all identities before removing any directory. Never follow a
        // substituted link or treat an unrelated sibling as a failed capture.
        for (name, expected) in &self.children {
            ensure!(
                identity(&self.workspace.join(name))? == *expected,
                "preparation directory changed; preserved: {name}"
            );
        }
        for (name, expected) in &self.children {
            ensure!(
                identity(&self.workspace)? == self.root,
                "preparation root changed"
            );
            let path = self.workspace.join(name);
            ensure!(
                identity(&path)? == *expected,
                "preparation directory changed: {name}"
            );
            fs::remove_dir_all(path)?;
        }
        self.record("cleaned", "", self.root)?;
        Ok(())
    }
}

/// Initialize only the explicitly selected directory, never an ancestor. An
/// existing .git entry (including a broken link or unsupported Git file) is
/// left untouched for the normal validation path to accept or reject.
fn initialize_selected_root(original: &Path, root: &File, run_dir: &Path) -> Result<bool> {
    match fs::symlink_metadata(original.join(".git")) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // Build administration privately, then publish it with a no-replace rename.
    // A concurrent git init or any existing .git entry is never overwritten.
    use std::os::unix::fs::DirBuilderExt;
    let staging = run_dir.join("project-init");
    fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let owned = identity(&staging)?;
    let result = (|| -> Result<()> {
        let mut command = git::command();
        command
            .args(["init", "--quiet", "--template=", "--initial-branch=main"])
            .arg(&staging);
        let output = git::execute(command, None)?;
        ensure!(
            output.status.success(),
            "initialize selected project: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            identity(&staging)? == owned,
            "Git initialization staging changed"
        );
        let root_identity = (root.metadata()?.dev(), root.metadata()?.ino());
        ensure!(
            identity(original)? == root_identity,
            "selected project changed during initialization"
        );
        super::delivery::rename_guarded(
            &open_dir(&staging)?,
            OsStr::new(".git"),
            root,
            OsStr::new(".git"),
            false,
        )?;
        root.sync_all()?;
        Ok(())
    })();
    let cleanup = (|| -> Result<()> {
        ensure!(
            identity(&staging)? == owned,
            "Git initialization staging changed; preserved"
        );
        fs::remove_dir_all(&staging)?;
        Ok(())
    })();
    result.and(cleanup)?;
    Ok(true)
}

/// Capture a saved project and two workers. `project` is the selected root.
/// Failed preparation removes its identified partial captures before returning.
pub fn prepare(project: &Path, run_dir: &Path, limit: u64) -> Result<PreparedWorkspace> {
    prepare_with_workers(project, run_dir, limit, crate::config::DEFAULT_WORKER_COUNT)
}

/// Capture a saved project and the selected immutable worker roster.
pub fn prepare_with_workers(
    project: &Path,
    run_dir: &Path,
    limit: u64,
    worker_count: usize,
) -> Result<PreparedWorkspace> {
    crate::config::validate_worker_count(worker_count)?;
    #[cfg(not(target_os = "macos"))]
    bail!("native COW workspaces require macOS");
    let until = deadline();
    let _clock = git::DeadlineGuard::new(until);
    let original = fs::canonicalize(project).context("resolve selected project")?;
    let root = open_dir(&original)?;
    let original_identity = (root.metadata()?.dev(), root.metadata()?.ino());
    // Reject inexpensive layout failures before walking a potentially large tree.
    match fs::symlink_metadata(original.join(".git")) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "DeLM needs a self-contained Git checkout. Linked worktrees and linked Git administration are unsupported; select a normal checkout instead"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Inspect project Git administration"),
    }
    let parent = fs::canonicalize(run_dir.parent().context("private run path has no parent")?)?;
    let intended = parent.join(
        run_dir
            .file_name()
            .context("private run path has no name")?,
    );
    ensure!(
        !intended.starts_with(&original) && !original.starts_with(&intended),
        "private storage must be separate from the original project"
    );
    ensure!(
        fs::metadata(&parent)?.dev() == root.metadata()?.dev(),
        "DeLM requires the project and private storage on the same filesystem. Move or clone the project onto the volume containing your home directory before retrying"
    );
    // This no-follow inventory measures ignored inputs before creating captures.
    // Link targets are validated after Git selection, allowing omitted local
    // environments without admitting external links into the source snapshot.
    let mut scan = inventory(&root, limit, until, true)?;
    for (path, (_, entry)) in &scan.entries {
        ensure!(
            !path.ends_with("/.git") && !path.ends_with("/.gitmodules") && path != ".gitmodules",
            "nested repositories and submodules are unsupported: {path}. Select a self-contained repository without nested repositories or submodules"
        );
        ensure!(
            entry.kind != FileKind::Symlink || !path.starts_with(".git"),
            "Git administration links are unsupported"
        );
    }
    if !run_dir.exists() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(run_dir)
            .context("create private run directory")?;
    }
    let run_dir = fs::canonicalize(run_dir)?;
    ensure!(
        !run_dir.starts_with(&original) && !original.starts_with(&run_dir),
        "private storage must be separate from the original project"
    );
    let run = open_dir(&run_dir)?;
    ensure!(
        run.metadata()?.uid() == unsafe { libc::geteuid() },
        "run directory has a different owner"
    );
    ensure!(
        run.metadata()?.mode() & 0o077 == 0,
        "run directory must have private mode 0700"
    );
    ensure!(
        run.metadata()?.dev() == root.metadata()?.dev(),
        "DeLM requires the project and private storage on the same filesystem. Move or clone the project onto the volume containing your home directory before retrying"
    );
    let mut capacity = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    ensure!(
        unsafe { libc::fstatvfs(run.as_raw_fd(), capacity.as_mut_ptr()) } == 0,
        "cannot inspect private filesystem capacity"
    );
    let capacity = unsafe { capacity.assume_init() };
    ensure!(
        capacity.f_bavail > 0,
        "private filesystem has no free space"
    );
    if initialize_selected_root(&original, &root, &run_dir)? {
        scan = inventory(&root, limit, until, true)?;
    }
    let git_dir = open_at(
        &root,
        OsStr::new(".git"),
        libc::O_RDONLY | libc::O_DIRECTORY,
    )
    .context("select an explicit self-contained Git root; ancestor discovery is disabled")?;
    ensure!(
        git_dir.metadata()?.dev() == root.metadata()?.dev(),
        "external Git administration is unsupported"
    );
    let workspace = run_dir.join("workspace");
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&workspace)
        .context(
            "workspace already exists or cannot be created; reconcile the existing run first",
        )?;
    let mut guard = PreparationGuard::new(&workspace)?;
    let result = (|| -> Result<PreparedWorkspace> {
        let mut last_error = None;
        for attempt in 0..3 {
            within(until)?;
            let baseline = guard.create(&format!("capture-{attempt}"))?;
            match capture(&root, &original, &baseline, &scan, limit, until) {
                Ok((baseline_manifest, baseline_inventory)) => {
                    let saved = guard.rename(&format!("capture-{attempt}"), "baseline")?;
                    // Capture already inventoried this private baseline in full.
                    // No workers exist yet and we never write it during cloning.
                    // Each clone still checks source identities and independently
                    // verifies the worker's contents, metadata and staged state.
                    let baseline_root = open_dir(&saved)?;
                    let selected = baseline_inventory
                        .entries
                        .keys()
                        .filter(|p| !p.is_empty())
                        .cloned()
                        .collect();
                    let baseline_index = git::run(&saved, &saved, &["ls-files", "--stage", "-z"])?;
                    let workers: Vec<_> = (1..=worker_count)
                        .map(|worker| workspace.join(format!("worker-{worker}")))
                        .collect();
                    for worker in &workers {
                        guard.create(worker.file_name().unwrap().to_str().unwrap())?;
                        clone_selected(
                            &baseline_root,
                            worker,
                            &baseline_inventory,
                            &selected,
                            until,
                        )?;
                        ensure!(
                            manifest(worker)?.files == baseline_manifest.files,
                            "worker did not match saved baseline"
                        );
                        ensure!(
                            git::run(worker, worker, &["ls-files", "--stage", "-z"])?
                                == baseline_index,
                            "worker staged state differs from baseline"
                        );
                    }
                    write_json(
                        &workspace.join("baseline-manifest.json"),
                        &baseline_manifest,
                    )?;
                    let mut children = BTreeMap::new();
                    children.insert("baseline".to_owned(), identity(&saved)?);
                    for worker in &workers {
                        let name = worker.file_name().unwrap().to_str().unwrap();
                        children.insert(name.to_owned(), identity(worker)?);
                    }
                    for previous in 0..attempt {
                        let name = format!("capture-{previous}");
                        children.insert(name.clone(), identity(&workspace.join(name))?);
                    }
                    write_json(
                        &workspace.join("ownership.json"),
                        &Ownership {
                            version: 1,
                            original: original.clone(),
                            original_identity: Some(original_identity),
                            root: identity(&workspace)?,
                            children,
                        },
                    )?;
                    return Ok(PreparedWorkspace {
                        original,
                        run_dir,
                        baseline: saved,
                        workers,
                        baseline_manifest,
                    });
                }
                Err(error) => {
                    // Retry only verified source drift, never capacity, cloning or
                    // compatibility failure. Failed captures remain owned copies.
                    let current = inventory(&root, limit, until, true)?;
                    if current == scan {
                        return Err(error);
                    }
                    last_error = Some(error);
                    scan = current;
                }
            }
        }
        bail!(
            "source kept changing during three bounded capture attempts: {}",
            last_error.map(|e| e.to_string()).unwrap_or_default()
        )
    })();
    match result {
        Ok(prepared) => Ok(prepared),
        Err(error) => match guard.cleanup() {
            Ok(()) => Err(error),
            Err(cleanup) => {
                Err(error.context(format!("preparation cleanup incomplete: {cleanup:#}")))
            }
        },
    }
}

pub(super) fn capture(
    root: &File,
    original: &Path,
    baseline: &Path,
    scan: &Inventory,
    limit: u64,
    until: Instant,
) -> Result<(Manifest, Inventory)> {
    let (format, config) = git::configuration(root, scan)?;
    git::initialize(baseline, &format, &config)?;
    let admin: BTreeSet<_> = scan
        .entries
        .keys()
        .filter(|p| git::admitted_admin(p))
        .cloned()
        .collect();
    clone_selected(root, baseline, scan, &admin, until)?;
    let mut selected = git::validate(baseline, original)?;
    let mut exclusions = BTreeMap::new();
    // Empty directories carry layout and permissions even without Git entries.
    // Ignored directories are classified by the same controlled Git rules.
    let all_dirs: Vec<_> = scan
        .entries
        .iter()
        .filter(|(p, (_, e))| {
            !p.is_empty()
                && !p.starts_with(".git/")
                && *p != ".git"
                && e.kind == FileKind::Directory
        })
        .map(|(p, _)| p.clone())
        .collect();
    let mut to_classify = BTreeSet::new();
    for directory in all_dirs {
        within(until)?;
        let prefix = format!("{directory}/");
        if selected
            .range(prefix.clone()..)
            .next()
            .is_some_and(|p| p.starts_with(&prefix))
        {
            selected.insert(directory);
        } else {
            to_classify.insert(directory);
        }
    }
    let ignored = git::ignored_directories(baseline, original, &to_classify)?;
    for directory in to_classify {
        within(until)?;
        if !ignored.contains(&directory) {
            selected.insert(directory);
        }
    }
    for (path, (_, entry)) in &scan.entries {
        if path.is_empty() || path == ".git" || path.starts_with(".git/") {
            continue;
        }
        if !selected.contains(path) {
            exclusions.insert(
                path.clone(),
                "ignored input; reconstruct through private setup".to_owned(),
            );
            continue;
        }
        validate_input_format(root, path, entry)?;
        if recognized_credential(root, path, entry)? {
            selected.remove(path);
            exclusions.insert(
                path.clone(),
                "recognized credential; runtime authorization is separate".to_owned(),
            );
        }
    }
    selected.retain(|path| scan.entries.contains_key(path));
    let children: Vec<_> = selected.iter().cloned().collect();
    for path in children {
        let mut parent = Path::new(&path).parent();
        while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
            selected.insert(p.to_str().context("non-UTF-8 path")?.to_owned());
            parent = p.parent();
        }
    }
    let expected: BTreeMap<_, _> = selected
        .iter()
        .map(|p| (p.clone(), scan.entries[p].1.clone()))
        .collect();
    validate_links(&expected)?;
    clone_selected(root, baseline, scan, &selected, until)?;
    let actual = manifest(baseline)?;
    ensure!(
        actual.files == expected,
        "saved baseline contents or metadata changed during capture"
    );
    ensure!(
        inventory(root, limit, until, true)? == *scan,
        "source changed during capture"
    );
    ensure!(
        identity(original)? == (root.metadata()?.dev(), root.metadata()?.ino()),
        "selected source root was replaced during capture"
    );
    let captured = inventory(&open_dir(baseline)?, u64::MAX, until, true)?;
    let mut admitted_bytes = 0u64;
    for (path, (identity, entry)) in &captured.entries {
        if path.is_empty() || path == ".git" || selected.contains(path) || admin.contains(path) {
            admitted_bytes = admitted_bytes
                .checked_add(identity.size)
                .and_then(|size| size.checked_add(entry.xattrs_bytes))
                .context("saved admission size overflow")?;
        }
    }
    ensure!(
        admitted_bytes < limit,
        "saved admitted content reaches the original size limit"
    );
    Ok((
        Manifest {
            files: expected,
            exclusions,
            runtime_links: BTreeMap::new(),
        },
        captured,
    ))
}

pub(super) fn recognized_credential(root: &File, path: &str, entry: &FileEntry) -> Result<bool> {
    let name = Path::new(path)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if name == ".env"
        || name.starts_with(".env.")
            && ![".env.example", ".env.sample", ".env.template"].contains(&name.as_str())
        || [
            "id_rsa",
            "id_ed25519",
            "id_ecdsa",
            ".netrc",
            "credentials.json",
            "auth.json",
        ]
        .contains(&name.as_str())
    {
        return Ok(true);
    }
    if entry.kind == FileKind::File {
        let mut file = open_relative(root, path, false)?;
        let mut buffer = [0u8; 8192];
        let count = file.read(&mut buffer)?;
        let bytes = &buffer[..count];
        if name == ".npmrc" {
            let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
            if text.contains("_auth") || text.contains("password=") || text.contains("password =") {
                return Ok(true);
            }
        }
        if [
            b"-----BEGIN PRIVATE KEY-----".as_slice(),
            b"-----BEGIN ENCRYPTED PRIVATE KEY-----".as_slice(),
            b"-----BEGIN RSA PRIVATE KEY-----".as_slice(),
            b"-----BEGIN DSA PRIVATE KEY-----".as_slice(),
            b"-----BEGIN OPENSSH PRIVATE KEY-----".as_slice(),
            b"-----BEGIN EC PRIVATE KEY-----".as_slice(),
        ]
        .iter()
        .any(|needle| bytes.windows(needle.len()).any(|w| w == *needle))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_input_format(root: &File, path: &str, entry: &FileEntry) -> Result<()> {
    if entry.kind == FileKind::File {
        let mut file = open_relative(root, path, false)?;
        let mut buffer = [0u8; 8192];
        let count = file.read(&mut buffer)?;
        let bytes = &buffer[..count];
        ensure!(
            !bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\n"),
            "unresolved LFS pointer requires an adapter: {path}"
        );
        let sqlite = bytes.starts_with(b"SQLite format 3\0");
        ensure!(
            !sqlite,
            "database input requires a saved database adapter: {path}"
        );
    }
    Ok(())
}

fn clone_selected(
    source: &File,
    destination: &Path,
    scan: &Inventory,
    selected: &BTreeSet<String>,
    until: Instant,
) -> Result<()> {
    let dest = open_dir(destination)?;
    for relative in selected {
        within(until)?;
        let (expected, entry) = scan
            .entries
            .get(relative)
            .context("capture path absent from inventory")?;
        let target = destination.join(relative);
        if entry.kind == FileKind::Directory {
            match fs::create_dir(&target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_dir(&target)?;
                }
                Err(e) => return Err(e.into()),
            }
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let parts = components(relative, true)?;
        let parent = if parts.len() == 1 {
            dest.try_clone()?
        } else {
            open_relative(
                &dest,
                Path::new(relative).parent().unwrap().to_str().unwrap(),
                true,
            )?
        };
        if entry.kind == FileKind::File {
            let file = open_relative(source, relative, false)?;
            ensure!(
                Identity::read(&file)? == *expected,
                "source changed before clone: {relative}"
            );
            clone_file_to_dir(&file, &parent, parts.last().unwrap())?;
            ensure!(
                Identity::read(&file)? == *expected,
                "source changed during clone: {relative}"
            );
        } else {
            let source_parent = if parts.len() == 1 {
                source.try_clone()?
            } else {
                open_relative(
                    source,
                    Path::new(relative).parent().unwrap().to_str().unwrap(),
                    true,
                )?
            };
            #[cfg(target_os = "macos")]
            {
                let file = open_at(
                    &source_parent,
                    parts.last().unwrap(),
                    libc::O_RDONLY | libc::O_SYMLINK,
                )?;
                ensure!(
                    Identity::read(&file)? == *expected,
                    "source link changed before capture: {relative}"
                );
                let name = cstr(parts.last().unwrap())?;
                let target = CString::new(
                    entry
                        .link_target
                        .as_ref()
                        .context("missing link target")?
                        .as_bytes(),
                )?;
                ensure!(
                    unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) }
                        == 0,
                    "create contained link: {}",
                    std::io::Error::last_os_error()
                );
                let copied = open_at(
                    &parent,
                    parts.last().unwrap(),
                    libc::O_RDONLY | libc::O_SYMLINK,
                )?;
                metadata(&file, &copied)?;
                ensure!(
                    Identity::read(&file)? == *expected,
                    "source link changed during capture: {relative}"
                );
            }
        }
    }
    // Restore directory metadata after adding children, deepest directories first.
    for relative in selected.iter().rev() {
        if scan.entries[relative].1.kind == FileKind::Directory {
            let source_dir = open_relative(source, relative, true)?;
            ensure!(
                Identity::read(&source_dir)? == scan.entries[relative].0,
                "source directory changed before metadata capture: {relative}"
            );
            let dest_dir = open_relative(&dest, relative, true)?;
            metadata(&source_dir, &dest_dir)?;
        }
    }
    dest.sync_all()?;
    Ok(())
}

pub(super) fn metadata(source: &File, destination: &File) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn fcopyfile(
                source: i32,
                destination: i32,
                state: *mut libc::c_void,
                flags: u32,
            ) -> i32;
        }
        // Metadata only: ACL + stat + extended attributes, never COPYFILE_DATA.
        ensure!(
            unsafe {
                fcopyfile(
                    source.as_raw_fd(),
                    destination.as_raw_fd(),
                    std::ptr::null_mut(),
                    7,
                )
            } == 0,
            "preserve metadata: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (source, destination);
        bail!("metadata backend requires macOS")
    }
}

/// Retain the selected zero-based worker index in place. The caller must first
/// stop and reap all owned writers. Cancellation must never call this cleanup.
pub fn retain_result(prepared: &PreparedWorkspace, worker: usize) -> Result<PathBuf> {
    retain_result_with_policy(prepared, worker, &ResultPolicy::default())
}

/// Result-only runtime allowances never change source admission or board paths.
pub fn retain_result_with_policy(
    prepared: &PreparedWorkspace,
    worker: usize,
    policy: &ResultPolicy,
) -> Result<PathBuf> {
    prepared.validate_layout()?;
    ensure!(worker < prepared.workers.len(), "unknown result worker");
    let workspace = prepared.run_dir.join("workspace");
    let ownership: Ownership =
        serde_json::from_reader(File::open(workspace.join("ownership.json"))?)?;
    ensure!(
        ownership.version == 1
            && ownership.original == prepared.original
            && ownership.root == identity(&workspace)?,
        "workspace ownership mismatch"
    );
    validate_ownership_roster(prepared, &ownership)?;
    let winner = &prepared.workers[worker];
    validate_retained_git(winner)?;
    if workspace.join("retained-result.json").exists() {
        let record: serde_json::Value =
            serde_json::from_reader(File::open(workspace.join("retained-result.json"))?)?;
        ensure!(
            record["worker_index"].as_u64() == Some(worker as u64)
                && record["path"].as_str() == winner.to_str(),
            "a different result is already retained"
        );
        let final_manifest: Manifest =
            serde_json::from_reader(File::open(workspace.join("review/after.json"))?)?;
        ensure!(
            manifest_for_result(winner, policy)? == final_manifest,
            "retained result changed since selection"
        );
        cleanup(&workspace, &ownership, worker, winner)?;
        return Ok(winner.clone());
    }
    for (name, id) in &ownership.children {
        ensure!(
            identity(&workspace.join(name))? == *id,
            "owned child was replaced: {name}"
        );
    }
    let current = manifest_for_result(winner, policy)?;
    let review = workspace.join("review");
    fs::create_dir(&review)
        .context("review already exists; reconcile retention before retrying")?;
    fs::set_permissions(&review, fs::Permissions::from_mode(0o700))?;
    let changed: BTreeSet<_> = prepared
        .baseline_manifest
        .files
        .keys()
        .chain(current.files.keys())
        .filter(|p| prepared.baseline_manifest.files.get(*p) != current.files.get(*p))
        .cloned()
        .collect();
    for (side, root, manifest) in [
        ("before", &prepared.baseline, &prepared.baseline_manifest),
        ("after", winner, &current),
    ] {
        let side_dir = review.join(side);
        fs::create_dir(&side_dir)?;
        for relative in &changed {
            if manifest
                .files
                .get(relative)
                .is_some_and(|entry| entry.kind == FileKind::File)
            {
                let source = safe_path(root, relative)?;
                let target = side_dir.join(relative);
                fs::create_dir_all(target.parent().context("review entry parent absent")?)?;
                clone_file(&source, &target)?;
            }
        }
    }
    write_json(&review.join("before.json"), &prepared.baseline_manifest)?;
    write_json(&review.join("after.json"), &current)?;
    write_json(&review.join("changed-paths.json"), &changed)?;
    ensure!(
        manifest_for_result(winner, policy)? == current,
        "result changed while retention was being prepared"
    );
    validate_retained_git(winner)?;
    write_json(
        &workspace.join("retained-result.json"),
        &serde_json::json!({"path": winner, "worker_index": worker, "review": review, "original": prepared.original}),
    )?;
    cleanup(&workspace, &ownership, worker, winner)?;
    Ok(winner.clone())
}

fn validate_retained_git(winner: &Path) -> Result<()> {
    let root = open_dir(winner)?;
    open_at(
        &root,
        OsStr::new(".git"),
        libc::O_RDONLY | libc::O_DIRECTORY,
    )
    .context("retained result must have self-contained Git administration")?;
    let scan = inventory(&root, u64::MAX, deadline(), true)?;
    git::configuration(&root, &scan)?;
    Ok(())
}

fn cleanup(workspace: &Path, ownership: &Ownership, worker: usize, winner: &Path) -> Result<()> {
    let winner_name = format!("worker-{}", worker + 1);
    // remove_dir_all does not traverse symlinks. The caller has stopped owned
    // writers, and worker sandboxes cannot modify this runtime-owned parent.
    for (name, owned) in &ownership.children {
        if name == &winner_name {
            ensure!(identity(winner)? == *owned, "winner identity changed");
            continue;
        }
        let path = workspace.join(name);
        if fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            continue;
        }
        ensure!(
            identity(&path)? == *owned,
            "cleanup target identity changed"
        );
        fs::remove_dir_all(&path).with_context(|| {
            format!(
                "result retained at {}; cleanup incomplete",
                winner.display()
            )
        })?;
    }
    open_dir(workspace)?.sync_all()?;
    Ok(())
}
