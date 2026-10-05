//! Inspect and export saved changes without altering the original project.
//! A recovery bundle is a delta: unchanged baseline files are not invented.
use super::*;
use std::os::unix::fs::DirBuilderExt;

#[derive(Debug, Serialize, Deserialize)]
struct Bundle {
    version: u8,
    original: PathBuf,
    workers: Vec<Delta>,
    #[serde(default)]
    excluded_paths: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Delta {
    worker: usize,
    changes: BTreeMap<String, (Option<FileEntry>, Option<FileEntry>)>,
}

#[derive(Debug, Serialize)]
pub struct RecoveryEntry {
    pub path: String,
    pub before: Option<FileEntry>,
    pub after: Option<FileEntry>,
}

#[derive(Debug, Serialize)]
pub struct RecoveryWorker {
    /// Public worker identities are one-based.
    pub worker: usize,
    pub changes: Vec<RecoveryEntry>,
}

#[derive(Debug, Serialize)]
pub struct RecoveryInspection {
    pub bundle: PathBuf,
    pub original: PathBuf,
    pub partial: bool,
    pub workers: Vec<RecoveryWorker>,
    pub verified_blobs: usize,
    pub excluded_paths: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct RecoveryExport {
    pub destination: PathBuf,
    pub worker: usize,
    pub partial: bool,
    pub files: PathBuf,
    pub base: PathBuf,
    pub manifest: PathBuf,
}

fn blob_file(bundle: &File, entry: &FileEntry) -> Result<File> {
    let hash = entry
        .sha256
        .as_deref()
        .context("Recovery file has no content digest")?;
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid recovery content digest"
    );
    let file = open_at(bundle, OsStr::new(hash), libc::O_RDONLY)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() == entry.size,
        "Recovery blob is not a file of the recorded length"
    );
    Ok(file)
}

fn verify_blob(bundle: &File, entry: &FileEntry) -> Result<()> {
    let mut file = blob_file(bundle, entry)?;
    let identity = Identity::read(&file)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    ensure!(
        Some(format!("{:x}", hash.finalize())) == entry.sha256
            && Identity::read(&file)? == identity,
        "Recovery blob does not match its recorded bytes"
    );
    Ok(())
}

fn load(bundle: &Path) -> Result<(PathBuf, File, Bundle, usize)> {
    let root = open_dir(bundle).context("Select a complete saved recovery bundle")?;
    let path = bundle.canonicalize()?;
    let mut file = open_at(&root, OsStr::new("complete.json"), libc::O_RDONLY)
        .context("Recovery is not complete; preserve its workspaces until recovery finishes")?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 64 * 1024 * 1024,
        "Recovery manifest is invalid or too large"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024 * 1024,
        "Recovery manifest grew beyond its bound"
    );
    let saved: Bundle = serde_json::from_slice(&bytes)?;
    ensure!(
        saved.version == 1 && saved.original.is_absolute() && saved.workers.len() <= 2,
        "Unsupported recovery manifest"
    );
    let mut workers = BTreeSet::new();
    let mut blobs = BTreeSet::new();
    for worker in &saved.workers {
        ensure!(
            worker.worker < 2 && workers.insert(worker.worker),
            "Invalid recovery worker identity"
        );
        ensure!(
            worker.changes.len() <= MAX_ENTRIES,
            "Recovery delta is too large"
        );
        for (path, (before, after)) in &worker.changes {
            components(path, false)?;
            ensure!(
                before.is_some() || after.is_some(),
                "Recovery entry has neither version"
            );
            for entry in [before, after].into_iter().flatten() {
                ensure!(entry.mode & !0o7777 == 0, "Invalid saved file mode");
                match entry.kind {
                    FileKind::File => {
                        let digest = entry
                            .sha256
                            .clone()
                            .context("Recovery file has no content digest")?;
                        if blobs.insert(digest) {
                            verify_blob(&root, entry)?;
                        } else {
                            blob_file(&root, entry)?;
                        }
                    }
                    FileKind::Symlink => {
                        ensure!(entry.link_target.is_some(), "Missing saved link target");
                    }
                    FileKind::Directory => {}
                }
            }
        }
    }
    Ok((path, root, saved, blobs.len()))
}

pub fn inspect_recovery(bundle: &Path) -> Result<RecoveryInspection> {
    let (bundle, _, saved, verified_blobs) = load(bundle)?;
    Ok(RecoveryInspection {
        bundle,
        original: saved.original,
        partial: true,
        verified_blobs,
        excluded_paths: saved.excluded_paths,
        workers: saved
            .workers
            .into_iter()
            .map(|worker| RecoveryWorker {
                worker: worker.worker + 1,
                changes: worker
                    .changes
                    .into_iter()
                    .map(|(path, (before, after))| RecoveryEntry {
                        path,
                        before,
                        after,
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn export_version(root: &Path, blobs: &File, delta: &Delta, after: bool) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(root)?;
    for (path, versions) in &delta.changes {
        let Some(entry) = (if after { &versions.1 } else { &versions.0 }) else {
            continue;
        };
        // Symlink targets remain inert in manifest.json. A delta may omit their
        // unchanged targets or contain links outside the old project; exporting
        // a live link could escape or accidentally resolve to unrelated data.
        if entry.kind == FileKind::Symlink {
            continue;
        }
        let destination = root.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if entry.kind == FileKind::Directory {
            if !destination.exists() {
                fs::create_dir(&destination)?;
            }
        } else {
            let mut source = blob_file(blobs, entry)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&destination)?;
            std::io::copy(&mut source, &mut output)?;
            output.sync_all()?;
            let mut captured = File::open(&destination)?;
            let mut digest = Sha256::new();
            std::io::copy(&mut captured, &mut digest)?;
            ensure!(
                Some(format!("{:x}", digest.finalize())) == entry.sha256,
                "Exported recovery bytes do not match: {path}"
            );
            // Preserve ordinary read/write/execute bits, never recreate set-id
            // or filesystem flags merely by inspecting saved partial changes.
            output.set_permissions(fs::Permissions::from_mode(entry.mode & 0o777))?;
        }
    }
    // Directory permissions come last, so restrictive parent modes cannot make
    // children fail halfway through a correct export.
    for (path, versions) in delta.changes.iter().rev() {
        if let Some(entry) = if after { &versions.1 } else { &versions.0 }
            && entry.kind == FileKind::Directory
        {
            fs::set_permissions(
                root.join(path),
                fs::Permissions::from_mode(entry.mode & 0o777),
            )?;
        }
    }
    open_dir(root)?.sync_all()?;
    Ok(())
}

/// Export one worker's before/after changes into a new destination. Public
/// worker numbers are 1 and 2. Original-project writes and deletion application
/// are intentionally separate actions; the manifest records both explicitly.
pub fn export_recovery(bundle: &Path, destination: &Path, worker: usize) -> Result<RecoveryExport> {
    ensure!((1..=2).contains(&worker), "Select worker 1 or 2");
    let (bundle, blobs, saved, _) = load(bundle)?;
    let selected = saved
        .workers
        .iter()
        .find(|delta| delta.worker + 1 == worker)
        .context("No saved changes exist for that worker")?;
    let parent = destination
        .parent()
        .context("Export destination needs a parent")?
        .canonicalize()?;
    let name = destination
        .file_name()
        .context("Export destination needs a name")?;
    let destination = parent.join(name);
    ensure!(
        !destination.starts_with(&bundle)
            && !bundle.starts_with(&destination)
            && !destination.starts_with(&saved.original)
            && !saved.original.starts_with(&destination),
        "Export into a new folder outside the original project and recovery bundle"
    );
    ensure!(
        fs::symlink_metadata(&destination)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "Export destination already exists or cannot be inspected"
    );
    let staging = parent.join(format!(".delm-recovery-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let result = (|| -> Result<()> {
        export_version(&staging.join("files"), &blobs, selected, true)?;
        export_version(&staging.join("base"), &blobs, selected, false)?;
        prepare::write_json(
            &staging.join("manifest.json"),
            &serde_json::json!({
                "version":1,"worker":worker,"partial":true,"original":saved.original,
                "changes":selected.changes,"excluded_paths":saved.excluded_paths,
                "instructions":"files/ contains changed result files; base/ contains their saved original versions. Unchanged files are not included. Deletions and symlink targets remain inert in this manifest. Ordinary permission bits are preserved; set-id bits, ACLs, extended attributes, and filesystem flags are not activated. Review before applying changes to your project."
            }),
        )?;
        super::delivery::rename_guarded(
            &open_dir(&parent)?,
            staging.file_name().unwrap(),
            &open_dir(&parent)?,
            name,
            false,
        )?;
        open_dir(&parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;
    Ok(RecoveryExport {
        files: destination.join("files"),
        base: destination.join("base"),
        manifest: destination.join("manifest.json"),
        destination,
        worker,
        partial: true,
    })
}
