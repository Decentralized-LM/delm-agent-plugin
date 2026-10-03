//! Private writable state for development tools used by one worker.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Prepare package caches and executable paths without sharing mutable host state.
/// The caller must separately qualify read access to `toolchain_paths` and
/// `rustup_home`, and restrict writes to this worker's project and environment.
pub(crate) fn prepare(
    root: &Path,
    toolchain_paths: &[PathBuf],
    rustup_home: Option<&Path>,
) -> Result<Value> {
    ensure!(
        root.is_absolute(),
        "Private environment path must be absolute"
    );
    ensure!(
        toolchain_paths.iter().all(|path| path.is_absolute())
            && rustup_home.is_none_or(Path::is_absolute),
        "Development toolchain paths must be absolute"
    );
    fs::create_dir_all(root).context("Create private development environment")?;
    ensure_directory(root)?;

    let home = directory(root, "home")?;
    let temp = directory(root, "tmp")?;
    let cache = directory(root, "home/.cache")?;
    let config = directory(root, "home/.config")?;
    let data = directory(root, "home/.local/share")?;
    let state = directory(root, "home/.local/state")?;
    let user_base = directory(root, "home/.local")?;
    let user_bin = directory(root, "home/.local/bin")?;
    let npm_cache = directory(root, "home/.cache/npm")?;
    let npm_config = directory(root, "home/.config/npm")?;
    let pip_cache = directory(root, "home/.cache/pip")?;
    let cargo = directory(root, "home/.cargo")?;
    let cargo_bin = directory(root, "home/.cargo/bin")?;
    let browsers = directory(root, "home/.cache/ms-playwright")?;

    let mut seen = HashSet::new();
    let path = std::env::join_paths(
        [user_bin, cargo_bin]
            .into_iter()
            .chain(toolchain_paths.iter().cloned())
            .filter(|path| seen.insert(path.clone())),
    )
    .context("Build private development PATH")?;
    let mut environment = json!({
        "PATH": path.to_string_lossy(),
        "HOME": home,
        "TMPDIR": temp,
        "TMP": temp,
        "TEMP": temp,
        "LANG": "en_US.UTF-8",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "XDG_CACHE_HOME": cache,
        "XDG_CONFIG_HOME": config,
        "XDG_DATA_HOME": data,
        "XDG_STATE_HOME": state,
        "NPM_CONFIG_CACHE": npm_cache,
        "NPM_CONFIG_PREFIX": user_base,
        "NPM_CONFIG_USERCONFIG": npm_config.join("npmrc"),
        "NPM_CONFIG_GLOBALCONFIG": npm_config.join("globalrc"),
        "PIP_CACHE_DIR": pip_cache,
        "PIP_CONFIG_FILE": "/dev/null",
        "PYTHONUSERBASE": user_base,
        "CARGO_HOME": cargo,
        "PLAYWRIGHT_BROWSERS_PATH": browsers,
    });
    // Retain the installed compiler without sharing Cargo downloads or config.
    if let Some(rustup_home) = rustup_home {
        environment["RUSTUP_HOME"] = json!(rustup_home);
    }
    Ok(environment)
}

fn ensure_directory(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "Private development directory must not be a symlink or file: {}",
        path.display()
    );
    Ok(())
}

fn directory(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(_) => ensure_directory(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .with_context(|| format!("Create private directory {}", path.display()))?;
            }
            Err(error) => return Err(error).context("Inspect private development directory"),
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn workers_keep_package_state_and_commands_separate() {
        let temp = tempfile::tempdir().unwrap();
        let first_root = temp.path().join("worker-1");
        let second_root = temp.path().join("worker-2");
        let toolchains = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/bin")];
        let first = prepare(&first_root, &toolchains, None).unwrap();
        let second = prepare(&second_root, &toolchains, None).unwrap();

        for key in [
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "NPM_CONFIG_CACHE",
            "NPM_CONFIG_PREFIX",
            "NPM_CONFIG_USERCONFIG",
            "NPM_CONFIG_GLOBALCONFIG",
            "PIP_CACHE_DIR",
            "PYTHONUSERBASE",
            "CARGO_HOME",
            "PLAYWRIGHT_BROWSERS_PATH",
        ] {
            assert!(Path::new(first[key].as_str().unwrap()).starts_with(&first_root));
            assert!(Path::new(second[key].as_str().unwrap()).starts_with(&second_root));
        }
        let paths: Vec<_> = std::env::split_paths(first["PATH"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            vec![
                first_root.join("home/.local/bin"),
                first_root.join("home/.cargo/bin"),
                PathBuf::from("/usr/bin"),
            ]
        );
        assert!(paths[0].is_dir());
        assert!(paths[1].is_dir());
        assert!(first.get("PIP_USER").is_none());
        assert!(first.get("PYTHONPATH").is_none());
        assert!(first.get("RUSTUP_HOME").is_none());
        assert_eq!(first["PIP_CONFIG_FILE"], "/dev/null");
    }

    #[test]
    fn retains_existing_private_state_and_qualified_compiler() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worker");
        let rustup = temp.path().join("installed-rustup");
        fs::create_dir(&rustup).unwrap();
        let first = prepare(&root, &[], Some(&rustup)).unwrap();
        let package = root.join("home/.cache/npm/retained-package");
        fs::write(&package, b"package").unwrap();
        let second = prepare(&root, &[], Some(&rustup)).unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read(package).unwrap(), b"package");
        assert_eq!(second["RUSTUP_HOME"], json!(rustup));
        assert_ne!(second["CARGO_HOME"], second["RUSTUP_HOME"]);
    }

    #[test]
    fn refuses_redirected_private_directories_without_writing_through_them() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worker");
        let outside = temp.path().join("outside");
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("home/.cache")).unwrap();
        assert!(prepare(&root, &[], None).is_err());
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn rejects_relative_paths_before_creating_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worker");
        assert!(prepare(&root, &[PathBuf::from("relative-bin")], None).is_err());
        assert!(!root.exists());
        assert!(prepare(&root, &[], Some(Path::new("relative-rustup"))).is_err());
        assert!(!root.exists());
    }
}
