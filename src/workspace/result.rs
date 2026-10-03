use super::*;

/// The caller supplies the worker's effective read-only runtime grants and
/// denials. This is a result policy, never authority to read more worker files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResultPolicy {
    pub readonly_runtime_roots: Vec<PathBuf>,
    pub denied_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeLink {
    pub target: PathBuf,
    pub file: FileEntry,
}

/// Keep every result entry, including ignored outputs and environment contents.
/// A real virtual environment may additionally retain its external interpreter
/// dependency. Source links and aliases to that dependency remain self-contained.
pub fn manifest_for_result(root: &Path, policy: &ResultPolicy) -> Result<Manifest> {
    let mut result = manifest_inventory(root)?;
    let root = root.canonicalize()?;
    let mut qualified = BTreeSet::new();
    let mut targets: BTreeMap<PathBuf, FileEntry> = BTreeMap::new();
    for (path, entry) in &result.files {
        if entry.kind != FileKind::Symlink {
            continue;
        }
        let Some(home) = environment_home(&root, path, &result.files)? else {
            continue;
        };
        let target = root
            .join(path)
            .canonicalize()
            .with_context(|| format!("resolve virtual environment interpreter: {path}"))?;
        if target.starts_with(&root) {
            continue;
        }
        let anchor = external_interpreter_anchor(path, &result.files)?;
        ensure!(
            allowed_runtime(policy, &target, &anchor, &home)?
                && anchor
                    .components()
                    .all(|component| component != Component::ParentDir),
            "virtual environment interpreter is outside qualified read-only runtimes: {path}"
        );
        let name = target.file_name().and_then(OsStr::to_str).unwrap_or("");
        ensure!(
            python_name(name),
            "virtual environment target is not a Python interpreter: {path}"
        );
        let declared = home.join(name).canonicalize();
        let alias = home
            .join(
                Path::new(path)
                    .file_name()
                    .context("interpreter name absent")?,
            )
            .canonicalize();
        ensure!(
            declared.as_ref().is_ok_and(|value| value == &target)
                || alias.as_ref().is_ok_and(|value| value == &target),
            "virtual environment interpreter disagrees with pyvenv.cfg home: {path}"
        );
        let dependency = if let Some(dependency) = targets.get(&target) {
            dependency.clone()
        } else {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&target)?;
            let metadata = file.metadata()?;
            ensure!(
                metadata.is_file() && metadata.mode() & 0o111 != 0,
                "virtual environment interpreter is not an executable regular file: {path}"
            );
            let (_, dependency, _) = inspect(&file, FileKind::File, None, deadline())?;
            targets.insert(target.clone(), dependency.clone());
            dependency
        };
        qualified.insert(path.clone());
        result.runtime_links.insert(
            path.clone(),
            RuntimeLink {
                target,
                file: dependency,
            },
        );
    }
    validate_links_except(&result.files, &qualified)?;
    Ok(result)
}

fn allowed_runtime(
    policy: &ResultPolicy,
    target: &Path,
    anchor: &Path,
    home: &Path,
) -> Result<bool> {
    let mut readable = false;
    for scope in &policy.readonly_runtime_roots {
        if scope.is_absolute() && within_scope(target, scope)? {
            readable = true;
        }
    }
    if !readable {
        return Ok(false);
    }
    for scope in &policy.denied_roots {
        for path in [target, anchor, home] {
            if within_scope(path, scope)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

// The stock permission mapping also uses physical ancestry. Match it here so
// /var aliases and case-insensitive APFS spelling cannot drop a denial.
fn within_scope(path: &Path, scope: &Path) -> Result<bool> {
    if path.starts_with(scope) {
        return Ok(true);
    }
    let id = match fs::metadata(scope) {
        Ok(metadata) => (metadata.dev(), metadata.ino()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect runtime permission scope"),
    };
    for ancestor in path.ancestors() {
        match fs::metadata(ancestor) {
            Ok(metadata) if (metadata.dev(), metadata.ino()) == id => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect runtime permission ancestry"),
        }
    }
    Ok(false)
}

// CPython creates same-bin aliases ending in one absolute base interpreter.
// Do not turn a route through source, a peer, or arbitrary project symlinks into
// a runtime allowance merely because its final destination is a Python binary.
fn external_interpreter_anchor(
    path: &str,
    entries: &BTreeMap<String, FileEntry>,
) -> Result<PathBuf> {
    let mut current = PathBuf::from(path);
    for _ in 0..40 {
        let link = current
            .to_str()
            .and_then(|key| entries.get(key))
            .and_then(|entry| entry.link_target.as_ref())
            .context("virtual environment interpreter alias is not captured")?;
        let target = Path::new(link);
        if target.is_absolute() {
            return Ok(target.to_path_buf());
        }
        ensure!(
            target.components().count() == 1
                && target
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(python_name),
            "virtual environment interpreter alias leaves its bin directory: {path}"
        );
        current = current
            .parent()
            .context("interpreter alias parent absent")?
            .join(target);
    }
    bail!("virtual environment interpreter alias cycle: {path}")
}

fn python_name(name: &str) -> bool {
    let Some(version) = name.strip_prefix("python") else {
        return false;
    };
    version.is_empty()
        || version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn environment_home(
    root: &Path,
    path: &str,
    entries: &BTreeMap<String, FileEntry>,
) -> Result<Option<PathBuf>> {
    let path = Path::new(path);
    if !path
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(python_name)
    {
        return Ok(None);
    }
    let Some(bin) = path
        .parent()
        .filter(|parent| parent.file_name() == Some(OsStr::new("bin")))
    else {
        return Ok(None);
    };
    let Some(environment) = bin.parent() else {
        return Ok(None);
    };
    let config = environment.join("pyvenv.cfg");
    let Some(config_name) = config.to_str() else {
        return Ok(None);
    };
    let Some(entry) = entries
        .get(config_name)
        .filter(|entry| entry.kind == FileKind::File)
    else {
        return Ok(None);
    };
    ensure!(
        entry.size <= 64 * 1024,
        "virtual environment configuration exceeds bound"
    );
    let bytes = fs::read(safe_path(root, config_name)?)?;
    ensure!(
        Some(format!("{:x}", Sha256::digest(&bytes))) == entry.sha256,
        "virtual environment configuration changed during result capture"
    );
    let config =
        std::str::from_utf8(&bytes).context("virtual environment configuration is not UTF-8")?;
    let fields: BTreeMap<_, _> = config
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect();
    let Some(home) = fields
        .get("home")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
    else {
        return Ok(None);
    };
    if !fields
        .get("include-system-site-packages")
        .is_some_and(|value| ["true", "false"].contains(value))
        || !fields.contains_key("version")
    {
        return Ok(None);
    }
    Ok(Some(home))
}
