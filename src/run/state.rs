use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn check_storage_boundary(project: &Path) -> Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?).canonicalize()?;
    let mut path = home.join("Library/Application Support/DeLM");
    while !path.exists() {
        path = path.parent().context("No storage ancestor")?.to_path_buf();
    }
    let ancestor = path.canonicalize()?;
    ensure!(
        !ancestor.starts_with(project),
        "Select a Git repository below your home directory; DeLM storage cannot be inside the original project"
    );
    Ok(())
}
fn directory_at(parent: &File, name: &OsStr, create: bool) -> Result<File> {
    let name = CString::new(name.as_bytes())?;
    if create && unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "DeLM storage child must be a real directory: {}: {}",
        name.to_string_lossy(),
        std::io::Error::last_os_error()
    );
    let file = unsafe { File::from_raw_fd(fd) };
    ensure!(
        file.metadata()?.uid() == unsafe { libc::getuid() },
        "DeLM storage directory has another owner"
    );
    Ok(file)
}

fn storage_at(path: &Path) -> Result<(PathBuf, File)> {
    let parent = path.parent().context("Missing storage parent")?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let parent_fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&parent)?;
    let name = path.file_name().context("Missing storage name")?;
    let directory = directory_at(&parent_fd, name, true)?;
    ensure!(
        directory.metadata()?.mode() & 0o077 == 0,
        "DeLM storage must be private to its owner; its existing permissions were not changed"
    );
    Ok((parent.join(name), directory))
}

fn root() -> Result<(PathBuf, File)> {
    let path = PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?)
        .join("Library/Application Support/DeLM");
    storage_at(&path)
}

/// Package resources must outlive native plugin cache upgrades and removal.
pub(crate) fn runtime_storage() -> Result<PathBuf> {
    let root = root()?;
    let directory = directory_at(&root.1, OsStr::new("runtimes"), true)?;
    ensure!(
        directory.metadata()?.mode() & 0o077 == 0,
        "DeLM runtime storage must be private to its owner"
    );
    Ok(root.0.join("runtimes"))
}

fn run_path_at(root: &(PathBuf, File), id: &uuid::Uuid) -> Result<PathBuf> {
    let runs = directory_at(&root.1, OsStr::new("runs"), true)?;
    let path = root.0.join("runs").join(id.to_string());
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            directory_at(&runs, OsStr::new(&id.to_string()), false)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(path)
}
pub fn run_path(id: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id).context("Invalid run identity")?;
    run_path_at(&root()?, &id)
}
pub fn create_run() -> Result<PathBuf> {
    let root = root()?;
    let runs = directory_at(&root.1, OsStr::new("runs"), true)?;
    let id = uuid::Uuid::new_v4().to_string();
    let name = CString::new(id.as_bytes())?;
    ensure!(
        unsafe { libc::mkdirat(runs.as_raw_fd(), name.as_ptr(), 0o700) } == 0,
        "Could not exclusively create a private run: {}",
        std::io::Error::last_os_error()
    );
    Ok(root.0.join("runs").join(id))
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(path.parent().context("Missing state directory")?)?.sync_all()?;
    Ok(())
}
pub struct RunLock(File);
impl RunLock {
    pub fn acquire(project: &Path) -> Result<Self> {
        check_storage_boundary(project)?;
        let root = root()?;
        let dir = directory_at(&root.1, OsStr::new("locks"), true)?;
        let key = format!(
            "{:x}",
            Sha256::digest(project.as_os_str().as_encoded_bytes())
        );
        let name = CString::new(key)?;
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(
            fd >= 0,
            "Could not open the private project lock: {}",
            std::io::Error::last_os_error()
        );
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.nlink() == 1
                && metadata.uid() == unsafe { libc::getuid() },
            "Project lock must be an owned regular file without hard links"
        );
        file.try_lock_exclusive()
            .context("Another DeLM task already owns this project")?;
        Ok(Self(file))
    }
}
impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
pub struct Journal(File);
impl Journal {
    pub fn open(run: &Path) -> Result<Self> {
        Ok(Self(
            OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(run.join("events.jsonl"))?,
        ))
    }
    pub fn record(&mut self, kind: &str, value: &impl Serialize) -> Result<()> {
        let value = serde_json::to_value(value)?;
        serde_json::to_writer(
            &mut self.0,
            &json!({"time":now(),"time_ms":now_ms(),"kind":kind,"data":value}),
        )?;
        self.0.write_all(b"\n")?;
        if kind != "native"
            || !value
                .get("method")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|m| m.ends_with("/delta"))
        {
            self.0.sync_data()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn storage_children_and_resumed_run_cannot_alias_the_original() {
        let fixture = tempfile::tempdir().unwrap();
        let original = fixture.path().join("original");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("keep.txt"), "original contents").unwrap();
        let storage = storage_at(&fixture.path().join("DeLM")).unwrap();
        for child in ["locks", "runs"] {
            symlink(&original, storage.0.join(child)).unwrap();
            assert!(directory_at(&storage.1, OsStr::new(child), true).is_err());
            if child == "runs" {
                assert!(run_path_at(&storage, &uuid::Uuid::new_v4()).is_err());
            }
            fs::remove_file(storage.0.join(child)).unwrap();
        }
        let id = uuid::Uuid::new_v4();
        let path = run_path_at(&storage, &id).unwrap();
        symlink(&original, &path).unwrap();
        assert!(run_path_at(&storage, &id).is_err());
        assert_eq!(
            fs::read_to_string(original.join("keep.txt")).unwrap(),
            "original contents"
        );
        assert_eq!(fs::read_dir(&original).unwrap().count(), 1);
    }

    #[test]
    fn storage_root_refuses_links_and_preserves_existing_directory_permissions() {
        let fixture = tempfile::tempdir().unwrap();
        let original = fixture.path().join("original");
        fs::create_dir(&original).unwrap();
        fs::set_permissions(&original, fs::Permissions::from_mode(0o755)).unwrap();
        let before = fs::metadata(&original).unwrap().mode();
        let link = fixture.path().join("DeLM");
        symlink(&original, &link).unwrap();
        assert!(storage_at(&link).is_err());
        assert!(storage_at(&original).is_err());
        assert_eq!(fs::metadata(&original).unwrap().mode(), before);
        assert_eq!(fs::read_dir(&original).unwrap().count(), 0);
    }
}
