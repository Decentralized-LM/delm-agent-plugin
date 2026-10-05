//! One accepted source-and-artifact contract, used before and after shutdown.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResultSelection {
    baseline_paths: BTreeSet<String>,
    baseline_exclusions: BTreeSet<String>,
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub artifacts_declared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptedResult {
    pub selection: ResultSelection,
    pub manifest: Manifest,
    pub artifact_files: Vec<String>,
    pub undelivered_outputs: Vec<String>,
    pub environment_directories_omitted: Vec<String>,
    pub excluded_paths: BTreeMap<String, String>,
}

fn in_scope(path: &str, scope: &str) -> bool {
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(super) fn contains_within(paths: &BTreeSet<String>, scope: &str) -> bool {
    if paths.contains(scope) {
        return true;
    }
    let prefix = format!("{scope}/");
    paths
        .range(prefix.clone()..)
        .next()
        .is_some_and(|path| path.starts_with(&prefix))
}

pub(super) fn disposable(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            "__pycache__"
                | ".pytest_cache"
                | ".mypy_cache"
                | ".ruff_cache"
                | ".cache"
                | ".vite"
                | ".turbo"
                | ".DS_Store"
        )
    }) || path.ends_with(".log")
        || path.starts_with(".next/cache/")
        || path == ".next/cache"
}

pub(super) fn git_names(worker: &Path, args: &[&str]) -> Result<BTreeSet<String>> {
    git::run(worker, worker, args)?
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)?.to_owned();
            components(&path, false)?;
            Ok(path)
        })
        .collect()
}

/// A configuration filename alone is not evidence of a disposable Python
/// environment. Require the standard fields and an interpreter in its bin tree.
pub(super) fn environment_roots(root: &File, names: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let mut environments = BTreeSet::new();
    for path in names {
        let parts: Vec<_> = path.split('/').collect();
        if let Some(index) = parts.iter().position(|part| *part == "node_modules") {
            environments.insert(parts[..=index].join("/"));
        }
        let Some(env) = path.strip_suffix("/pyvenv.cfg") else {
            continue;
        };
        let config = match open_relative(root, path, false) {
            Ok(file) if file.metadata()?.is_file() && file.metadata()?.len() <= 64 * 1024 => file,
            _ => continue,
        };
        let mut text = String::new();
        if config
            .take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .is_err()
        {
            continue;
        }
        let fields: BTreeMap<_, _> = text
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.trim(), value.trim()))
            .collect();
        let valid = fields
            .get("home")
            .is_some_and(|home| Path::new(home).is_absolute())
            && fields
                .get("include-system-site-packages")
                .is_some_and(|value| ["true", "false"].contains(value))
            && fields.get("version").is_some_and(|value| {
                value.split('.').count() >= 2
                    && value
                        .split('.')
                        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            })
            && names.iter().any(|name| {
                name.strip_prefix(&format!("{env}/bin/python"))
                    .is_some_and(|suffix| {
                        suffix.is_empty() || suffix.chars().all(|c| c.is_ascii_digit() || c == '.')
                    })
            });
        if valid {
            environments.insert(env.to_owned());
        }
    }
    Ok(environments)
}

impl ResultSelection {
    pub fn new(baseline: &Manifest, artifacts: Vec<String>) -> Result<Self> {
        ensure!(
            artifacts.len() <= 256,
            "At most 256 artifact scopes may be declared"
        );
        let mut unique = BTreeSet::new();
        for path in artifacts {
            components(&path, false)?;
            ensure!(path.len() <= 4096, "Artifact path is too long");
            ensure!(unique.insert(path), "Duplicate artifact path");
        }
        Ok(Self {
            baseline_paths: baseline.files.keys().cloned().collect(),
            baseline_exclusions: baseline.exclusions.keys().cloned().collect(),
            artifacts_declared: !unique.is_empty(),
            artifacts: unique.into_iter().collect(),
        })
    }

    pub fn capture(&self, worker: &Path) -> Result<AcceptedResult> {
        let root = open_dir(worker)?;
        let tracked = git_names(worker, &["ls-files", "--cached", "-z"])?;
        let source = git_names(
            worker,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
        )?;
        let names = git_names(worker, &["ls-files", "--cached", "--others", "-z"])?;
        let environments = environment_roots(&root, &names)?;
        let artifact = |path: &str| self.artifacts.iter().any(|scope| in_scope(path, scope));
        let dependency = |path: &str| environments.iter().any(|scope| in_scope(path, scope));
        for scope in &self.artifacts {
            ensure!(
                !dependency(scope),
                "A dependency environment is not a deliverable: {scope}"
            );
        }
        // Skip omitted environments and ignored operational caches before opening
        // or hashing their contents. Inherited/tracked source remains authoritative.
        let skip = |path: &str| {
            let protected = artifact(path)
                || self.artifacts.iter().any(|scope| in_scope(scope, path))
                || contains_within(&self.baseline_paths, path)
                || contains_within(&tracked, path);
            !protected && (dependency(path) || (disposable(path) && !source.contains(path)))
        };
        let inspected = inventory_filtered(&root, u64::MAX, deadline(), false, Some(&skip))?;
        let mut files: BTreeMap<_, _> = inspected
            .entries
            .into_iter()
            .filter(|(path, _)| !path.is_empty())
            .map(|(path, (_, entry))| (path, entry))
            .collect();
        let directories = files
            .iter()
            .filter(|(_, entry)| entry.kind == FileKind::Directory)
            .map(|(path, _)| path.clone())
            .collect();
        let ignored_dirs = git::ignored_directories(worker, worker, &directories)?;
        let mut excluded_paths = BTreeMap::new();
        let mut credential_roots: Vec<String> = Vec::new();
        let mut selected = BTreeSet::new();
        let mut artifact_files = Vec::new();
        let mut undelivered_outputs = Vec::new();
        for (path, entry) in &files {
            let secret = credential_roots.iter().any(|scope| in_scope(path, scope))
                || prepare::recognized_credential(&root, path, entry)?;
            if secret {
                ensure!(
                    !artifact(path),
                    "A recognized credential cannot be a deliverable: {path}"
                );
                ensure!(
                    !self.baseline_paths.contains(path) && !tracked.contains(path),
                    "Source changed into a recognized credential; delivery requires review: {path}"
                );
                if entry.kind == FileKind::Directory {
                    credential_roots.push(path.clone());
                }
                excluded_paths.insert(path.clone(), "recognized credential; not exported".into());
                continue;
            }
            if artifact(path) {
                ensure!(
                    !dependency(path),
                    "Artifact scope includes a dependency environment: {path}"
                );
                selected.insert(path.clone());
                if entry.kind != FileKind::Directory {
                    artifact_files.push(path.clone());
                }
            } else if self.baseline_paths.contains(path)
                || tracked.contains(path)
                || (!dependency(path)
                    && !self.baseline_exclusions.contains(path)
                    && (source.contains(path)
                        || (entry.kind == FileKind::Directory && !ignored_dirs.contains(path))))
            {
                selected.insert(path.clone());
            } else if entry.kind != FileKind::Directory && !dependency(path) && !disposable(path) {
                undelivered_outputs.push(path.clone());
            }
        }
        for scope in &self.artifacts {
            ensure!(
                files.contains_key(scope),
                "Declared artifact is missing: {scope}"
            );
            ensure!(
                selected.contains(scope),
                "Declared artifact was excluded: {scope}"
            );
        }
        for path in selected.clone() {
            for parent in Path::new(&path)
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
            {
                selected.insert(
                    parent
                        .to_str()
                        .context("Non-UTF-8 artifact parent")?
                        .to_owned(),
                );
            }
        }
        files.retain(|path, _| selected.contains(path));
        validate_links(&files)?;
        let environment_directories_omitted = environments
            .into_iter()
            .filter(|scope| !files.keys().any(|path| in_scope(path, scope)))
            .collect();
        Ok(AcceptedResult {
            selection: self.clone(),
            manifest: Manifest {
                files,
                exclusions: BTreeMap::new(),
                runtime_links: BTreeMap::new(),
            },
            artifact_files,
            undelivered_outputs,
            environment_directories_omitted,
            excluded_paths,
        })
    }
}

impl AcceptedResult {
    /// Re-select against the same baseline and declared artifacts. New source,
    /// changed ignore rules, removed artifacts and changed bytes remain visible.
    pub fn verify(&self, worker: &Path) -> Result<Self> {
        let current = self.selection.capture(worker)?;
        ensure!(
            current.manifest == self.manifest && current.artifact_files == self.artifact_files,
            "Accepted source or requested artifacts changed after completion; work is preserved"
        );
        Ok(current)
    }
}
