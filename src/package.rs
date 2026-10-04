//! Preserve executable resources independently of Codex's replaceable plugin cache.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
};

const MAX_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn retained_executable() -> Result<PathBuf> {
    preserve(
        &std::env::current_exe()?.canonicalize()?,
        &crate::run::state::runtime_storage()?,
    )
}

/// Re-exec before starting Tokio or workers, keeping the foreground process and
/// its PID, streams, arguments, and environment. No persistent daemon is added.
pub fn preserve_for_run(project: &Path) -> Result<()> {
    let project = project.canonicalize().context("Project is unavailable")?;
    crate::run::state::check_storage_boundary(&project)?;
    let current = std::env::current_exe()?.canonicalize()?;
    let retained = preserve(&current, &crate::run::state::runtime_storage()?)?;
    if current != retained {
        let error = Command::new(&retained)
            .args(std::env::args_os().skip(1))
            .exec();
        return Err(error).context("Start the retained DeLM executable");
    }
    Ok(())
}

fn open_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Runtime must be an ordinary file");
    ensure!(
        metadata.len() > 0 && metadata.len() <= MAX_EXECUTABLE_BYTES,
        "Runtime executable has an unsupported size"
    );
    Ok(file)
}

fn digest(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    let mut total = 0;
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= MAX_EXECUTABLE_BYTES,
            "Runtime grew while being read"
        );
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let info = fs::symlink_metadata(path)?;
    ensure!(
        info.is_dir() && info.uid() == unsafe { libc::getuid() } && info.mode() & 0o077 == 0,
        "Runtime storage must be a private directory owned by this user: {}",
        path.display()
    );
    Ok(())
}

fn verify_retained(path: &Path, expected: &str) -> Result<()> {
    let mut file = open_file(path)?;
    let info = file.metadata()?;
    ensure!(
        info.uid() == unsafe { libc::getuid() }
            && info.nlink() == 1
            && info.mode() & 0o777 == 0o500,
        "Retained runtime permissions changed; preserving the existing file"
    );
    ensure!(
        digest(&mut file)? == expected,
        "Retained runtime contents changed; preserving the existing file"
    );
    Ok(())
}

fn preserve(source: &Path, root: &Path) -> Result<PathBuf> {
    private_directory(root)?;
    let mut input = open_file(source)?;
    ensure!(
        input.metadata()?.mode() & 0o111 != 0,
        "Runtime is not executable"
    );
    let hash = digest(&mut input)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(root.join(".lock"))?;
    let info = lock.metadata()?;
    ensure!(
        info.is_file()
            && info.uid() == unsafe { libc::getuid() }
            && info.nlink() == 1
            && info.mode() & 0o077 == 0,
        "Runtime storage lock is not private"
    );
    lock.lock_exclusive()?;
    let directory = root.join(&hash);
    private_directory(&directory)?;
    let target = directory.join("delm");
    match fs::symlink_metadata(&target) {
        Ok(_) => verify_retained(&target, &hash)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let pending = directory.join(format!("{}.pending", uuid::Uuid::new_v4()));
            let result = (|| {
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&pending)?;
                input.seek(SeekFrom::Start(0))?;
                let size = std::io::copy(
                    &mut (&mut input).take(MAX_EXECUTABLE_BYTES + 1),
                    &mut output,
                )?;
                ensure!(
                    size <= MAX_EXECUTABLE_BYTES,
                    "Runtime grew during preservation"
                );
                output.set_permissions(fs::Permissions::from_mode(0o500))?;
                output.sync_all()?;
                verify_retained(&pending, &hash)?;
                fs::rename(&pending, &target)?;
                File::open(&directory)?.sync_all()?;
                Ok::<_, anyhow::Error>(())
            })();
            if result.is_err() {
                // Only this invocation's incomplete temporary copy is removed.
                let _ = fs::remove_file(&pending);
            }
            result?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn retained_versions_survive_package_removal_without_redundant_copies() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("installed runtime");
        let root = temp.path().join("private runtimes");
        executable(&source, "#!/bin/sh\nprintf first");
        let first = preserve(&source, &root).unwrap();
        assert_eq!(first, preserve(&source, &root).unwrap());
        executable(&source, "#!/bin/sh\nprintf second");
        let second = preserve(&source, &root).unwrap();
        assert_ne!(first, second);
        fs::remove_file(source).unwrap();
        assert_eq!(Command::new(first).output().unwrap().stdout, b"first");
        assert_eq!(Command::new(second).output().unwrap().stdout, b"second");
        assert_eq!(fs::read_dir(root).unwrap().count(), 3);
    }

    #[test]
    fn modified_or_linked_runtime_is_refused_and_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let root = temp.path().join("runtimes");
        executable(&source, "#!/bin/sh\nexit 0");
        let target = preserve(&source, &root).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&target, "changed").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(preserve(&source, &root).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "changed");
        fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(&source, &target).unwrap();
        assert!(preserve(&source, &root).is_err());
        assert!(target.is_symlink());
    }

    #[test]
    fn linked_or_shared_storage_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        executable(&source, "#!/bin/sh\nexit 0");
        let directory = temp.path().join("real");
        private_directory(&directory).unwrap();
        let linked = temp.path().join("linked");
        std::os::unix::fs::symlink(&directory, &linked).unwrap();
        assert!(preserve(&source, &linked).is_err());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(preserve(&source, &directory).is_err());
    }
}
