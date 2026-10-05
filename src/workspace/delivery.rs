//! Deliver source deltas without touching the user's Git index or dependencies.
//!
//! The runtime must stop all worker writers and owned services before calling
//! either public operation. Every replacement is journaled before it happens.
use super::prepare::{Ownership, identity, write_json};
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReport {
    pub project: PathBuf,
    pub delivered: bool,
    pub changed_paths: Vec<String>,
    /// These paths contain a three-way merge with concurrent original edits;
    /// worker-only check evidence does not cover the merged result.
    #[serde(default)]
    pub merged_paths: Vec<String>,
    /// Source delivery preserves the original environment. Changes to these
    /// manifests require the host to prepare/check that environment in place.
    #[serde(default)]
    pub environment_files_changed: Vec<String>,
    /// Worker-local dependency trees were deliberately omitted. Their presence
    /// does not establish that the original project's environment is ready.
    #[serde(default)]
    pub environment_directories_omitted: Vec<String>,
    #[serde(default)]
    pub verification_required: bool,
    pub conflicts: Vec<String>,
    pub recovery: Option<PathBuf>,
    pub cleanup_complete: bool,
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// An explicit [] is a source-only output contract. Additional ignored
    /// files are retained for review without making that contract incomplete.
    #[serde(default)]
    pub artifacts_declared: bool,
    #[serde(default)]
    pub undelivered_outputs: Vec<String>,
    #[serde(default)]
    pub excluded_paths: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub recovery: PathBuf,
    pub cleanup_complete: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Change {
    path: String,
    before: Option<FileEntry>,
    after: Option<FileEntry>,
    stage: String,
    displaced: String,
    applied: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u8,
    project: PathBuf,
    project_identity: (u64, u64),
    worker: usize,
    changes: Vec<Change>,
    complete: bool,
}

#[cfg(test)]
thread_local! {
    static BEFORE_VERIFICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

fn owned(prepared: &PreparedWorkspace) -> Result<Ownership> {
    let workspace = prepared.run_dir.join("workspace");
    let ownership: Ownership =
        serde_json::from_slice(&fs::read(workspace.join("ownership.json"))?)?;
    ensure!(
        ownership.version == 1
            && ownership.root == identity(&workspace)?
            && ownership.original == prepared.original,
        "workspace ownership does not match delivery"
    );
    for (name, expected) in &ownership.children {
        let path = workspace.join(name);
        if path.try_exists()? {
            ensure!(
                identity(&path)? == *expected,
                "owned workspace was replaced: {name}"
            );
        }
    }
    Ok(ownership)
}

fn cleanup_all(prepared: &PreparedWorkspace, ownership: &Ownership) -> Result<()> {
    let workspace = prepared.run_dir.join("workspace");
    ensure!(
        identity(&workspace)? == ownership.root,
        "workspace parent was replaced"
    );
    let mut children: Vec<_> = ownership.children.iter().collect();
    // Keep base bytes available if removing a worker fails and recovery is needed.
    children.sort_by_key(|(name, _)| !name.starts_with("worker-"));
    for (name, expected) in children {
        let path = workspace.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => ensure!(
                identity(&path)? == *expected,
                "cleanup target was replaced: {name}"
            ),
        }
        fs::remove_dir_all(path)
            .with_context(|| format!("saved changes, but could not remove {name}"))?;
    }
    open_dir(&workspace)?.sync_all()?;
    Ok(())
}

fn parent_at(root: &File, relative: &str) -> Result<(File, OsString)> {
    let parts = components(relative, false)?;
    let mut parent = root.try_clone()?;
    for part in &parts[..parts.len() - 1] {
        parent = open_at(&parent, part, libc::O_RDONLY | libc::O_DIRECTORY)?;
    }
    Ok((parent, parts.last().unwrap().to_os_string()))
}

fn entry_at(parent: &File, name: &OsStr) -> Result<Option<FileEntry>> {
    let name_c = cstr(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name_c.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    let stat = unsafe { stat.assume_init() };
    let (kind, flags) = match stat.st_mode as u32 & libc::S_IFMT as u32 {
        x if x == libc::S_IFREG as u32 => (FileKind::File, libc::O_RDONLY),
        x if x == libc::S_IFDIR as u32 => (FileKind::Directory, libc::O_RDONLY | libc::O_DIRECTORY),
        x if x == libc::S_IFLNK as u32 => {
            #[cfg(target_os = "macos")]
            {
                (FileKind::Symlink, libc::O_RDONLY | libc::O_SYMLINK)
            }
            #[cfg(not(target_os = "macos"))]
            {
                bail!("link delivery requires macOS")
            }
        }
        _ => bail!("unsupported delivery target type"),
    };
    let target = if kind == FileKind::Symlink {
        let mut target = vec![0; 65536];
        let n = unsafe {
            libc::readlinkat(
                parent.as_raw_fd(),
                name_c.as_ptr(),
                target.as_mut_ptr().cast(),
                target.len(),
            )
        };
        ensure!(
            n >= 0 && (n as usize) < target.len(),
            "cannot read delivery link"
        );
        target.truncate(n as usize);
        Some(String::from_utf8(target)?)
    } else {
        None
    };
    let file = open_at(parent, name, flags)?;
    ensure!(
        file.metadata()?.ino() == stat.st_ino,
        "delivery target changed during inspection"
    );
    Ok(Some(inspect(&file, kind, target, deadline())?.1))
}

fn entry(root: &File, path: &str) -> Result<Option<FileEntry>> {
    match parent_at(root, path) {
        Ok((parent, name)) => entry_at(&parent, &name),
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR))
            }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn checkpoint(directory: &Path, journal: &Journal) -> Result<()> {
    let temporary = directory.join(format!("journal-{}.tmp", uuid::Uuid::new_v4()));
    write_json(&temporary, journal)?;
    fs::rename(temporary, directory.join("journal.json"))?;
    open_dir(directory)?.sync_all()?;
    Ok(())
}

fn update_report(path: &Path, report: &DeliveryReport) -> Result<()> {
    let temporary = path.with_file_name(format!("result-{}.tmp", uuid::Uuid::new_v4()));
    write_json(&temporary, report)?;
    fs::rename(temporary, path)?;
    open_dir(path.parent().context("delivery report has no parent")?)?.sync_all()?;
    Ok(())
}

fn clean_staging(directory: &Path) -> Result<()> {
    for name in names(&open_dir(directory)?)? {
        // An editor may still hold a writable descriptor for a displaced
        // original inode. Keep that inode linked, even after a successful apply;
        // deleting it could discard a concurrent save through that descriptor.
        if name == "journal.json"
            || name == "result.json"
            || name.as_bytes().starts_with(b"previous-")
        {
            continue;
        }
        let path = directory.join(name);
        if fs::symlink_metadata(&path)?.is_dir() {
            fs::remove_dir(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    open_dir(directory)?.sync_all()?;
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)?;
    open_dir(path.parent().context("private directory parent absent")?)?.sync_all()?;
    Ok(())
}

fn stage_entry(source: &Path, relative: &str, entry: &FileEntry, target: &Path) -> Result<()> {
    let source_root = open_dir(source)?;
    let (parent, name) = parent_at(&source_root, relative)?;
    ensure!(
        entry_at(&parent, &name)?.as_ref() == Some(entry),
        "source changed before staging: {relative}"
    );
    match entry.kind {
        FileKind::File => {
            let file = open_at(&parent, &name, libc::O_RDONLY)?;
            clone_file_to_dir(
                &file,
                &open_dir(target.parent().unwrap())?,
                target.file_name().unwrap(),
            )?;
        }
        FileKind::Directory => {
            fs::create_dir(target)?;
            prepare::metadata(
                &open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY)?,
                &open_dir(target)?,
            )?;
        }
        FileKind::Symlink => {
            std::os::unix::fs::symlink(
                entry.link_target.as_ref().context("link target absent")?,
                target,
            )?;
            #[cfg(target_os = "macos")]
            {
                let source = open_at(&parent, &name, libc::O_RDONLY | libc::O_SYMLINK)?;
                let destination = open_at(
                    &open_dir(target.parent().unwrap())?,
                    target.file_name().unwrap(),
                    libc::O_RDONLY | libc::O_SYMLINK,
                )?;
                prepare::metadata(&source, &destination)?;
            }
        }
    }
    ensure!(
        entry_at(&parent, &name)?.as_ref() == Some(entry),
        "source changed while staging: {relative}"
    );
    open_dir(target.parent().unwrap())?.sync_all()?;
    Ok(())
}

fn text_merge(base: &Path, current: &Path, candidate: &Path) -> Result<Option<Vec<u8>>> {
    for path in [base, current, candidate] {
        if fs::metadata(path)?.len() > 8 * 1024 * 1024 {
            return Ok(None);
        }
        let content = fs::read(path)?;
        if content.contains(&0) || std::str::from_utf8(&content).is_err() {
            return Ok(None);
        }
    }
    let mut command = git::command();
    command
        .args(["merge-file", "--stdout", "--"])
        .arg(current)
        .arg(base)
        .arg(candidate);
    let output = git::execute(command, None)?;
    match output.status.code() {
        Some(0) => Ok(Some(output.stdout)),
        Some(1..=127) => Ok(None),
        _ => bail!(
            "three-way merge failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn same_metadata(a: &FileEntry, b: &FileEntry) -> bool {
    a.kind == b.kind
        && a.mode == b.mode
        && a.xattrs_sha256 == b.xattrs_sha256
        && a.acl_sha256 == b.acl_sha256
        && a.flags == b.flags
}

/// Apply the selected worker's source delta to the original saved working tree.
/// Conflicts produce a durable recovery bundle and do not overwrite user edits.
/// Call only after every worker writer and run-owned service has stopped.
pub fn deliver_result(
    prepared: &PreparedWorkspace,
    worker: usize,
    _policy: &ResultPolicy,
) -> Result<DeliveryReport> {
    ensure!(worker < 2, "unknown result worker");
    if prepared
        .run_dir
        .join("workspace/delivery/result.json")
        .is_file()
    {
        return deliver_internal(prepared, worker, None);
    }
    let accepted = ResultSelection::new(&prepared.baseline_manifest, Vec::new())?
        .capture(&prepared.workers[worker])?;
    deliver_internal(prepared, worker, Some(&accepted))
}

/// Deliver exactly the accepted source and requested-artifact manifest. The
/// caller supplies shutdown proof; this operation independently checks bytes.
pub fn deliver_accepted_result(
    prepared: &PreparedWorkspace,
    worker: usize,
    accepted: &AcceptedResult,
) -> Result<DeliveryReport> {
    ensure!(worker < 2, "unknown result worker");
    deliver_internal(prepared, worker, Some(accepted))
}

fn deliver_internal(
    prepared: &PreparedWorkspace,
    worker: usize,
    accepted: Option<&AcceptedResult>,
) -> Result<DeliveryReport> {
    let ownership = owned(prepared)?;
    ensure!(
        ownership.original_identity == Some(identity(&prepared.original)?),
        "selected project was replaced or has no captured identity"
    );
    let directory = prepared.run_dir.join("workspace/delivery");
    let report_path = directory.join("result.json");
    if report_path.exists() {
        let mut report: DeliveryReport = serde_json::from_slice(&fs::read(&report_path)?)?;
        ensure!(
            report.project == prepared.original,
            "delivery report project mismatch"
        );
        let journal: Journal = serde_json::from_slice(&fs::read(directory.join("journal.json"))?)?;
        ensure!(
            journal.version == 1
                && journal.worker == worker
                && journal.project == prepared.original
                && journal.project_identity == identity(&prepared.original)?,
            "Delivery journal does not identify this worker and project"
        );
        if report.conflicts.is_empty() {
            ensure!(journal.complete, "Delivery has no durable completion proof");
        }
        ensure!(
            if report.conflicts.is_empty() {
                report.changed_paths
                    == journal
                        .changes
                        .iter()
                        .map(|change| change.path.clone())
                        .collect::<Vec<_>>()
            } else {
                report.changed_paths.is_empty()
            },
            "Delivery report does not match its saved journal"
        );
        if !report.delivered || !report.undelivered_outputs.is_empty() {
            inspect_recovery(
                report
                    .recovery
                    .as_deref()
                    .context("Incomplete delivery has no recovery bundle")?,
            )?;
        }
        cleanup_all(prepared, &ownership)?;
        if report.delivered {
            clean_staging(&directory)?;
        }
        report.cleanup_complete = true;
        update_report(&report_path, &report)?;
        return Ok(report);
    }
    if directory.exists() {
        bail!(
            "interrupted delivery journal requires recovery at {}",
            directory.display()
        );
    }
    let accepted = accepted.context("Accepted result is missing")?;
    let mut expected = ResultSelection::new(
        &prepared.baseline_manifest,
        accepted.selection.artifacts.clone(),
    )?;
    expected.artifacts_declared = accepted.selection.artifacts_declared;
    ensure!(
        accepted.selection == expected,
        "Accepted result belongs to a different baseline"
    );
    let accepted = accepted.verify(&prepared.workers[worker])?;
    let candidate = &accepted.manifest;
    let environment_directories_omitted = accepted.environment_directories_omitted.clone();
    private_directory(&directory)?;
    let root = open_dir(&prepared.original)?;
    let changed: BTreeSet<_> = prepared
        .baseline_manifest
        .files
        .keys()
        .chain(candidate.files.keys())
        .filter(|p| prepared.baseline_manifest.files.get(*p) != candidate.files.get(*p))
        .cloned()
        .collect();
    let mut journal = Journal {
        version: 1,
        project: prepared.original.clone(),
        project_identity: identity(&prepared.original)?,
        worker,
        changes: Vec::new(),
        complete: false,
    };
    let mut conflicts = Vec::new();
    let mut merged_paths = Vec::new();
    for (index, path) in changed.iter().enumerate() {
        let base = prepared.baseline_manifest.files.get(path);
        let desired = candidate.files.get(path);
        let current = entry(&root, path)?;
        if current.as_ref() == desired {
            continue;
        }
        let stage = format!("next-{index}");
        let staged = directory.join(&stage);
        if let Some(desired) = desired {
            stage_entry(&prepared.workers[worker], path, desired, &staged)?;
        }
        let mut after = desired.cloned();
        if current.as_ref() != base {
            let mergeable = base.zip(current.as_ref()).zip(desired).is_some_and(
                |((base, current), desired)| {
                    base.kind == FileKind::File
                        && current.kind == FileKind::File
                        && desired.kind == FileKind::File
                        && (same_metadata(base, current)
                            || same_metadata(base, desired)
                            || same_metadata(current, desired))
                },
            );
            if mergeable {
                let original_copy = directory.join(format!("current-{index}"));
                stage_entry(
                    &prepared.original,
                    path,
                    current.as_ref().unwrap(),
                    &original_copy,
                )?;
                if let Some(merged) =
                    text_merge(&prepared.baseline.join(path), &original_copy, &staged)?
                {
                    if same_metadata(base.unwrap(), desired.unwrap()) {
                        fs::remove_file(&staged)?;
                        clone_file(&original_copy, &staged)?;
                    }
                    let mut file = OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&staged)?;
                    file.write_all(&merged)?;
                    file.sync_all()?;
                    after = entry_at(&open_dir(&directory)?, OsStr::new(&stage))?;
                    merged_paths.push(path.clone());
                } else {
                    conflicts.push(path.clone());
                }
            } else {
                conflicts.push(path.clone());
            }
        }
        // Directory metadata is changed in place, never by exchanging a live
        // nonempty tree. Other type transitions are conservatively recoverable.
        if current
            .as_ref()
            .zip(after.as_ref())
            .is_some_and(|(a, b)| a.kind != b.kind)
        {
            conflicts.push(path.clone());
        }
        if desired.is_none()
            && current
                .as_ref()
                .is_some_and(|e| e.kind == FileKind::Directory)
        {
            let directory_root = open_relative(&root, path, true)?;
            for name in names(&directory_root)? {
                let child = format!(
                    "{path}/{}",
                    name.to_str().context("non-UTF-8 concurrent file")?
                );
                if !changed.contains(&child) || candidate.files.contains_key(&child) {
                    conflicts.push(path.clone());
                }
            }
        }
        journal.changes.push(Change {
            path: path.clone(),
            before: current,
            after,
            stage,
            displaced: format!("previous-{index}"),
            applied: false,
        });
    }
    checkpoint(&directory, &journal)?;
    if !conflicts.is_empty() {
        conflicts.sort();
        conflicts.dedup();
        let recovery = preserve_partial(prepared, &accepted.selection.artifacts)?;
        let mut report = DeliveryReport {
            project: prepared.original.clone(),
            delivered: false,
            changed_paths: Vec::new(),
            merged_paths: Vec::new(),
            environment_files_changed: Vec::new(),
            environment_directories_omitted,
            verification_required: false,
            conflicts,
            recovery: Some(recovery),
            cleanup_complete: false,
            artifacts: accepted.artifact_files.clone(),
            artifacts_declared: accepted.selection.artifacts_declared,
            undelivered_outputs: accepted.undelivered_outputs.clone(),
            excluded_paths: accepted.excluded_paths.clone(),
        };
        write_json(&report_path, &report)?;
        cleanup_all(prepared, &ownership)?;
        report.cleanup_complete = true;
        update_report(&report_path, &report)?;
        return Ok(report);
    }
    // Create parent directories first; remove child entries before empty parents.
    journal.changes.sort_by_key(|c| {
        let depth = c.path.split('/').count();
        if c.after
            .as_ref()
            .is_some_and(|e| e.kind == FileKind::Directory)
        {
            (0, depth, c.path.clone())
        } else if c.after.is_some() {
            (1, depth, c.path.clone())
        } else {
            (2, MAX_DEPTH - depth, c.path.clone())
        }
    });
    checkpoint(&directory, &journal)?;
    for index in 0..journal.changes.len() {
        ensure!(
            identity(&prepared.original)? == journal.project_identity,
            "project replaced during delivery"
        );
        apply_change(&root, &directory, &journal.changes[index])?;
        journal.changes[index].applied = true;
        checkpoint(&directory, &journal)?;
    }
    #[cfg(test)]
    BEFORE_VERIFICATION.with(|hook| {
        if let Some(action) = hook.borrow_mut().take() {
            action();
        }
    });
    // Check every touched path again before declaring delivery. A user's later
    // edit is never repaired or rolled back automatically.
    for change in &journal.changes {
        ensure!(
            entry(&root, &change.path)? == change.after,
            "project changed during delivery: {}; journal retained",
            change.path
        );
        if let Some(before) = &change.before {
            let staged = open_dir(&directory)?;
            ensure!(
                entry_at(&staged, OsStr::new(&change.displaced))?.as_ref() == Some(before),
                "concurrent edit to displaced original {}; saved in delivery recovery",
                change.path
            );
            if before.kind == FileKind::Directory && change.after.is_none() {
                ensure!(
                    names(&open_at(
                        &staged,
                        OsStr::new(&change.displaced),
                        libc::O_RDONLY | libc::O_DIRECTORY
                    )?)?
                    .is_empty(),
                    "concurrent files in displaced original directory {}; saved in delivery recovery",
                    change.path
                );
            }
        }
    }
    journal.complete = true;
    checkpoint(&directory, &journal)?;
    let environment_files_changed: Vec<_> = journal
        .changes
        .iter()
        .filter(|c| environment_manifest(&c.path))
        .map(|c| c.path.clone())
        .collect();
    // An undeclared custom output is never destroyed just because Git ignores
    // it. Preserve it for review. An explicit artifact declaration determines
    // whether the accepted output contract is complete without these extras.
    let output_recovery = if accepted.undelivered_outputs.is_empty() {
        None
    } else {
        Some(preserve_partial(prepared, &accepted.selection.artifacts)?)
    };
    let mut report = DeliveryReport {
        project: prepared.original.clone(),
        delivered: accepted.selection.artifacts_declared || accepted.undelivered_outputs.is_empty(),
        changed_paths: journal.changes.iter().map(|c| c.path.clone()).collect(),
        verification_required: !merged_paths.is_empty()
            || !environment_files_changed.is_empty()
            || !environment_directories_omitted.is_empty(),
        merged_paths,
        environment_files_changed,
        environment_directories_omitted,
        conflicts: Vec::new(),
        recovery: output_recovery.or_else(|| {
            journal
                .changes
                .iter()
                .any(|change| change.before.is_some())
                .then(|| directory.clone())
        }),
        cleanup_complete: false,
        artifacts: accepted.artifact_files,
        artifacts_declared: accepted.selection.artifacts_declared,
        undelivered_outputs: accepted.undelivered_outputs,
        excluded_paths: accepted.excluded_paths,
    };
    write_json(&report_path, &report)?;
    cleanup_all(prepared, &ownership)?;
    clean_staging(&directory)?;
    report.cleanup_complete = true;
    update_report(&report_path, &report)?;
    Ok(report)
}

fn environment_manifest(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    [
        "package.json",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
        "pyproject.toml",
        "uv.lock",
        "Pipfile",
        "Pipfile.lock",
        "poetry.lock",
        "environment.yml",
        "environment.yaml",
        "Gemfile",
        "Gemfile.lock",
        "go.mod",
        "go.sum",
        "Cargo.toml",
        "Cargo.lock",
    ]
    .contains(&name)
        || (name.starts_with("requirements") && name.ends_with(".txt"))
}

// macOS exchange and no-replace renames prevent a new user file from being
// silently replaced in the interval between inspection and applying a change.
pub(super) fn rename_guarded(
    from: &File,
    name: &OsStr,
    to: &File,
    target: &OsStr,
    exchange: bool,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn renameatx_np(
                fromfd: i32,
                from: *const libc::c_char,
                tofd: i32,
                to: *const libc::c_char,
                flags: u32,
            ) -> i32;
        }
        let (name, target) = (cstr(name)?, cstr(target)?);
        ensure!(
            unsafe {
                renameatx_np(
                    from.as_raw_fd(),
                    name.as_ptr(),
                    to.as_raw_fd(),
                    target.as_ptr(),
                    if exchange { 2 } else { 4 },
                )
            } == 0,
            "guarded delivery rename: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (from, name, to, target, exchange);
        bail!("guarded delivery requires macOS")
    }
}

fn apply_change(root: &File, directory: &Path, change: &Change) -> Result<()> {
    let (parent, name) = parent_at(root, &change.path)?;
    let stage = open_dir(directory)?;
    ensure!(
        entry_at(&parent, &name)? == change.before,
        "concurrent edit at {}; delivery journal retained",
        change.path
    );
    if change
        .before
        .as_ref()
        .zip(change.after.as_ref())
        .is_some_and(|(a, b)| a.kind == FileKind::Directory && b.kind == FileKind::Directory)
    {
        fs::create_dir(directory.join(&change.displaced))?;
        prepare::metadata(
            &open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY)?,
            &open_at(
                &stage,
                OsStr::new(&change.displaced),
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?,
        )?;
        prepare::metadata(
            &open_at(
                &stage,
                OsStr::new(&change.stage),
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?,
            &open_at(&parent, &name, libc::O_RDONLY | libc::O_DIRECTORY)?,
        )?;
    } else if change.after.is_none() {
        if change
            .before
            .as_ref()
            .is_some_and(|e| e.kind == FileKind::Directory)
        {
            ensure!(
                names(&open_at(
                    &parent,
                    &name,
                    libc::O_RDONLY | libc::O_DIRECTORY
                )?)?
                .is_empty(),
                "directory contains concurrent or excluded files: {}",
                change.path
            );
        }
        rename_guarded(&parent, &name, &stage, OsStr::new(&change.displaced), false)?;
        let unchanged = entry_at(&stage, OsStr::new(&change.displaced))? == change.before;
        let still_empty = !change
            .before
            .as_ref()
            .is_some_and(|e| e.kind == FileKind::Directory)
            || names(&open_at(
                &stage,
                OsStr::new(&change.displaced),
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?)?
            .is_empty();
        if !unchanged || !still_empty {
            // Never overwrite a newly created destination while restoring.
            let _ = rename_guarded(&stage, OsStr::new(&change.displaced), &parent, &name, false);
            bail!(
                "entry changed during removal; preserved in project or delivery journal: {}",
                change.path
            );
        }
    } else if change.before.is_none() {
        rename_guarded(&stage, OsStr::new(&change.stage), &parent, &name, false)?;
    } else {
        rename_guarded(&stage, OsStr::new(&change.stage), &parent, &name, true)?;
        // The old entry is now safely displaced, including any race winner.
        if entry_at(&stage, OsStr::new(&change.stage))? != change.before {
            if entry_at(&parent, &name)? == change.after {
                rename_guarded(&stage, OsStr::new(&change.stage), &parent, &name, true)?;
            }
            bail!(
                "entry changed during replacement; preserved in delivery journal: {}",
                change.path
            );
        }
        rename_guarded(
            &stage,
            OsStr::new(&change.stage),
            &stage,
            OsStr::new(&change.displaced),
            false,
        )?;
    }
    parent.sync_all()?;
    stage.sync_all()?;
    ensure!(
        entry_at(&parent, &name)? == change.after,
        "delivery verification failed: {}",
        change.path
    );
    Ok(())
}

#[derive(Serialize)]
struct RecoveryDelta {
    worker: usize,
    changes: BTreeMap<String, (Option<FileEntry>, Option<FileEntry>)>,
}

fn preserve_partial(prepared: &PreparedWorkspace, artifacts: &[String]) -> Result<PathBuf> {
    for path in artifacts {
        components(path, false)?;
    }
    let destination = prepared.run_dir.join("workspace/recovery");
    if destination.join("complete.json").is_file() {
        inspect_recovery(&destination)?;
        return Ok(destination);
    }
    if !destination.exists() {
        private_directory(&destination)?;
    }
    let mut deltas = Vec::new();
    let mut excluded = BTreeMap::new();
    let baseline_paths = prepared.baseline_manifest.files.keys().cloned().collect();
    for (worker, path) in prepared.workers.iter().enumerate() {
        if !path.exists() {
            continue;
        }
        // Recovery may include broken links. Store their targets as inert
        // manifest data rather than creating links in the recovery package.
        let root = open_dir(path)?;
        let names = output::git_names(path, &["ls-files", "--cached", "--others", "-z"])?;
        let environments = output::environment_roots(&root, &names)?;
        let tracked = output::git_names(path, &["ls-files", "--cached", "-z"])?;
        let source = output::git_names(
            path,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
        )?;
        let skip = |relative: &str| {
            let baseline = output::contains_within(&baseline_paths, relative);
            let tracked = output::contains_within(&tracked, relative);
            let source = output::contains_within(&source, relative);
            let artifact = artifacts.iter().any(|scope| {
                relative == scope
                    || relative.starts_with(&format!("{scope}/"))
                    || scope.starts_with(&format!("{relative}/"))
            });
            !baseline
                && !tracked
                && (environments
                    .iter()
                    .any(|env| relative == env || relative.starts_with(&format!("{env}/")))
                    || (!artifact
                        && !source
                        && output::disposable(relative)
                        && !relative.ends_with(".log")))
        };
        let scan = inventory_filtered(&root, u64::MAX, deadline(), false, Some(&skip))?;
        let after = Manifest {
            files: scan
                .entries
                .into_iter()
                .filter(|(p, _)| !p.is_empty())
                .map(|(p, (_, e))| (p, e))
                .collect(),
            ..Manifest::default()
        };
        let mut credential_roots: Vec<String> = Vec::new();
        let mut changes = BTreeMap::new();
        for relative in prepared
            .baseline_manifest
            .files
            .keys()
            .chain(after.files.keys())
            .collect::<BTreeSet<_>>()
        {
            let before = prepared.baseline_manifest.files.get(relative);
            let result = after.files.get(relative);
            if let Some(entry) = result
                && (credential_roots
                    .iter()
                    .any(|scope| relative == scope || relative.starts_with(&format!("{scope}/")))
                    || prepare::recognized_credential(&root, relative, entry)?)
            {
                if entry.kind == FileKind::Directory {
                    credential_roots.push(relative.clone());
                }
                excluded.insert(
                    format!("worker-{}/{relative}", worker + 1),
                    "recognized credential; not exported",
                );
                continue;
            }
            if before == result {
                continue;
            }
            for (source, value) in [(&prepared.baseline, before), (path, result)] {
                if let Some(value) = value.filter(|e| e.kind == FileKind::File) {
                    let digest = value.sha256.as_ref().context("recovery file hash absent")?;
                    let blob = destination.join(digest);
                    if !blob.exists() {
                        stage_entry(source, relative, value, &blob)?;
                    }
                    let mut file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&blob)?;
                    let mut digest = Sha256::new();
                    std::io::copy(&mut file, &mut digest)?;
                    ensure!(
                        Some(format!("{:x}", digest.finalize())) == value.sha256,
                        "recovery blob changed"
                    );
                }
            }
            changes.insert(relative.clone(), (before.cloned(), result.cloned()));
        }
        deltas.push(RecoveryDelta { worker, changes });
    }
    write_json(
        &destination.join("complete.json"),
        &serde_json::json!({
            "version":1, "original":prepared.original, "workers":deltas, "excluded_paths":excluded,
            "format":"File bytes are SHA-256 named blobs. Manifests record modes, deletions, and inert symlink targets. No Git index changes are applied.",
            "delivery_journal": prepared.run_dir.join("workspace/delivery/journal.json")
        }),
    )?;
    Ok(destination)
}

/// Preserve useful partial source changes and then remove every owned worker
/// tree and baseline. The caller must already have stopped all writers.
pub fn preserve_partial_and_cleanup(prepared: &PreparedWorkspace) -> Result<RecoveryReport> {
    preserve_partial_and_cleanup_with_artifacts(prepared, &[])
}

/// Preserve explicitly requested artifact scopes even when their paths resemble
/// operational caches. Call only after every owned writer has stopped.
pub fn preserve_partial_and_cleanup_with_artifacts(
    prepared: &PreparedWorkspace,
    artifacts: &[String],
) -> Result<RecoveryReport> {
    let ownership = owned(prepared)?;
    let recovery = preserve_partial(prepared, artifacts)?;
    cleanup_all(prepared, &ownership)?;
    Ok(RecoveryReport {
        recovery,
        cleanup_complete: true,
    })
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
