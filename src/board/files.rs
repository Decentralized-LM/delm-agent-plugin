use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct FileVersion {
    pub sha256: String,
    pub bytes: u64,
    pub executable: u32,
}

pub(super) struct Root {
    pub path: PathBuf,
    directory: File,
    device: u64,
    inode: u64,
}

impl Root {
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "root cannot be a symbolic link"
        );
        let path = fs::canonicalize(path)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let metadata = directory.metadata()?;
        Ok(Self {
            path,
            directory,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub fn verify(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
                && fs::canonicalize(&self.path)? == self.path,
            "private root identity changed"
        );
        Ok(())
    }

    /// Every component is opened relative to an already pinned directory. A
    /// symlink replacement cannot redirect traversal into another tree.
    fn parent(&self, relative: &str, create: bool) -> Result<Option<(File, CString)>> {
        validate_relative(relative)?;
        self.verify()?;
        let mut components = relative.split('/').peekable();
        let mut parent = self.directory.try_clone()?;
        while let Some(component) = components.next() {
            let name = CString::new(component)?;
            if components.peek().is_none() {
                return Ok(Some((parent, name)));
            }
            let mut fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error).context("unsafe or inaccessible path component");
                }
                if !create {
                    return Ok(None);
                }
                let made = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
                if made < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error).context("create import parent directory");
                    }
                }
                fd = unsafe {
                    libc::openat(
                        parent.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("open import parent directory");
                }
                parent.sync_all()?;
            }
            parent = unsafe { File::from_raw_fd(fd) };
        }
        bail!("empty project path")
    }

    fn open_file(&self, relative: &str) -> Result<Option<File>> {
        let Some((parent, name)) = self.parent(relative, false)? else {
            return Ok(None);
        };
        open_regular(&parent, &name)
    }

    pub fn version(&self, relative: &str) -> Result<Option<FileVersion>> {
        let Some(mut file) = self.open_file(relative)? else {
            return Ok(None);
        };
        let result = version(&mut file)?;
        self.verify()?;
        Ok(Some(result))
    }

    pub fn freeze(
        &self,
        relative: &str,
        objects: &Root,
        object: &str,
        expected: &FileVersion,
    ) -> Result<FileVersion> {
        let mut source = self
            .open_file(relative)?
            .context("publication source disappeared")?;
        ensure!(
            version(&mut source)? == *expected,
            "publication source changed before freezing"
        );
        objects.verify()?;
        crate::workspace::clone_file_to_dir(&source, &objects.directory, OsStr::new(object))?;
        let captured = objects
            .open_file(object)?
            .context("captured object is missing")?;
        let captured_raw = captured.as_raw_fd();
        ensure!(
            unsafe { libc::fchmod(captured_raw, 0o400) } == 0,
            "make captured object read only: {}",
            std::io::Error::last_os_error()
        );
        captured.sync_all()?;
        objects.directory.sync_all()?;
        let mut result = objects
            .version(object)?
            .context("captured object disappeared")?;
        ensure!(
            result.sha256 == expected.sha256 && result.bytes == expected.bytes,
            "publication source changed during COW capture"
        );
        ensure!(
            self.version(relative)?.as_ref() == Some(expected),
            "publication source changed during freezing"
        );
        result.executable = expected.executable;
        Ok(result)
    }

    pub fn verify_object(&self, object: &str, expected: &FileVersion) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(object).is_ok(),
            "invalid object reference"
        );
        let actual = self
            .version(object)?
            .context("publication object is missing")?;
        ensure!(
            actual.sha256 == expected.sha256 && actual.bytes == expected.bytes,
            "immutable publication failed integrity verification"
        );
        Ok(())
    }

    pub fn read_range(
        &self,
        object: &str,
        expected: &FileVersion,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        self.verify_object(object, expected)?;
        let mut file = self
            .open_file(object)?
            .context("publication object disappeared")?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(length).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn install(
        &self,
        relative: &str,
        objects: &Root,
        object: Option<&str>,
        expected: Option<&FileVersion>,
        authorized_before: Option<&FileVersion>,
    ) -> Result<()> {
        let before = self.version(relative)?;
        if before.as_ref() == expected {
            return Ok(());
        }
        ensure!(
            before.as_ref() == authorized_before,
            "local file changed after import compatibility check"
        );
        let Some((parent, name)) = self.parent(relative, expected.is_some())? else {
            ensure!(expected.is_none(), "missing import parent");
            return Ok(());
        };
        match (object, expected) {
            (Some(object), Some(expected)) => {
                objects.verify_object(object, expected)?;
                let source = objects
                    .open_file(object)?
                    .context("publication object disappeared")?;
                let stage = format!(".delm-import-{}", uuid::Uuid::new_v4());
                crate::workspace::clone_file_to_dir(&source, &parent, OsStr::new(&stage))?;
                let stage = CString::new(stage)?;
                let staged = open_regular(&parent, &stage)?.context("staged import disappeared")?;
                let result = (|| -> Result<()> {
                    ensure!(
                        unsafe {
                            libc::fchmod(
                                staged.as_raw_fd(),
                                (0o644 | expected.executable) as libc::mode_t,
                            )
                        } == 0,
                        "restore publication executable bits: {}",
                        std::io::Error::last_os_error()
                    );
                    staged.sync_all()?;
                    self.verify_parent(relative, &parent)?;
                    ensure!(
                        self.version(relative)? == before,
                        "local file changed during import preparation"
                    );
                    ensure!(
                        unsafe {
                            libc::renameat(
                                parent.as_raw_fd(),
                                stage.as_ptr(),
                                parent.as_raw_fd(),
                                name.as_ptr(),
                            )
                        } == 0,
                        "install publication: {}",
                        std::io::Error::last_os_error()
                    );
                    parent.sync_all()?;
                    ensure!(
                        self.version(relative)?.as_ref() == Some(expected),
                        "imported manifest differs from publication"
                    );
                    Ok(())
                })();
                if result.is_err() {
                    // Only this unique staging entry is disposable. The actual
                    // destination is never removed to recover from an error.
                    unsafe { libc::unlinkat(parent.as_raw_fd(), stage.as_ptr(), 0) };
                }
                result?;
            }
            (None, None) => {
                self.verify_parent(relative, &parent)?;
                ensure!(
                    self.version(relative)? == before,
                    "local file changed before deletion import"
                );
                if before.is_some() {
                    ensure!(
                        unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0,
                        "import deletion: {}",
                        std::io::Error::last_os_error()
                    );
                    parent.sync_all()?;
                }
                ensure!(
                    self.version(relative)?.is_none(),
                    "deleted path reappeared during import"
                );
            }
            _ => bail!("invalid publication object manifest"),
        }
        Ok(())
    }

    fn verify_parent(&self, relative: &str, pinned: &File) -> Result<()> {
        let (current, _) = self
            .parent(relative, false)?
            .context("import parent disappeared")?;
        let current = current.metadata()?;
        let pinned = pinned.metadata()?;
        ensure!(
            current.dev() == pinned.dev() && current.ino() == pinned.ino(),
            "import parent changed during operation"
        );
        Ok(())
    }
}

fn open_regular(parent: &File, name: &CString) -> Result<Option<File>> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error).context("open publication path without following links");
    }
    let file = unsafe { File::from_raw_fd(fd) };
    ensure!(
        file.metadata()?.is_file(),
        "only regular files and deletions may be published or imported"
    );
    Ok(Some(file))
}

fn version(file: &mut File) -> Result<FileVersion> {
    const MAX_VERSION_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    let before = file.metadata()?;
    ensure!(before.is_file(), "unsupported file type");
    ensure!(
        before.len() <= MAX_VERSION_BYTES,
        "file exceeds the 4 GiB board transfer limit"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes = bytes
            .checked_add(count as u64)
            .context("file size overflow")?;
        ensure!(
            bytes <= MAX_VERSION_BYTES,
            "file grew beyond the 4 GiB board transfer limit"
        );
    }
    let after = file.metadata()?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == bytes
            && after.len() == bytes
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "file changed during manifest hashing"
    );
    Ok(FileVersion {
        sha256: format!("{:x}", hash.finalize()),
        bytes,
        executable: after.mode() & 0o111,
    })
}

pub(super) fn validate_relative(relative: &str) -> Result<()> {
    ensure!(
        !relative.is_empty() && relative.len() <= 4096 && !relative.contains('\0'),
        "invalid project-relative path"
    );
    ensure!(
        !relative.starts_with('/') && !relative.ends_with('/') && !relative.contains("//"),
        "path must be a normalized project-relative file"
    );
    for part in relative.split('/') {
        ensure!(
            !matches!(part, "." | "..") && !part.eq_ignore_ascii_case(".git"),
            "path traversal and Git administration are not allowed"
        );
    }
    ensure!(
        Path::new(relative)
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "invalid path component"
    );
    Ok(())
}

/// Literal grants remain literal. Fail closed on alternate spellings of a
/// restrictive scope, including not-yet-created paths. `canonicalize` does not
/// normalize filename case on APFS, and inode comparison cannot protect future
/// import destinations. This can restrict additional names on case-sensitive
/// volumes; it never creates a new read or write grant.
pub(super) fn restrictive_alias(path: &Path, scope: &Path) -> Result<bool> {
    if path.starts_with(scope) {
        return Ok(false);
    }
    let mut parts = path.components();
    for scope_part in scope.components() {
        let Some(part) = parts.next() else {
            return Ok(false);
        };
        if !equivalent_component(part.as_os_str(), scope_part.as_os_str())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Preserve rules for missing leaves while resolving aliases in existing
/// ancestors, such as macOS /var -> /private/var. A failed full canonicalize
/// must not silently leave such a deny rule outside the worker's real root.
pub(super) fn normalize_rule_path(path: &Path) -> Result<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    let mut normalized = loop {
        match fs::canonicalize(ancestor) {
            Ok(path) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .context("permission path has no existing ancestor")?
                        .to_owned(),
                );
                ancestor = ancestor.parent().context("permission path has no parent")?;
            }
            Err(error) => return Err(error).context("resolve permission path"),
        }
    };
    for component in suffix.into_iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

#[cfg(target_os = "macos")]
fn equivalent_component(left: &OsStr, right: &OsStr) -> Result<bool> {
    use std::ffi::c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> *const c_void;
        fn CFStringCompare(left: *const c_void, right: *const c_void, flags: usize) -> isize;
        fn CFRelease(value: *const c_void);
    }
    struct StringRef(*const c_void);
    impl Drop for StringRef {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }
    fn string(value: &OsStr) -> Result<StringRef> {
        let bytes = value
            .to_str()
            .context("non-UTF-8 permission path")?
            .as_bytes();
        // kCFStringEncodingUTF8, with no external representation marker.
        let value = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                0x08000100,
                0,
            )
        };
        ensure!(!value.is_null(), "could not normalize permission path");
        Ok(StringRef(value))
    }
    if left == right {
        return Ok(true);
    }
    let left = string(left)?;
    let right = string(right)?;
    // kCFCompareCaseInsensitive | kCFCompareNonliteral. Nonliteral comparison
    // includes composed/decomposed Unicode equivalents, not accent removal.
    Ok(unsafe { CFStringCompare(left.0, right.0, 1 | 16) } == 0)
}

#[cfg(not(target_os = "macos"))]
fn equivalent_component(left: &OsStr, right: &OsStr) -> Result<bool> {
    Ok(left == right)
}

pub(super) fn open_private_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1 && metadata.uid() == unsafe { libc::geteuid() },
        "unsafe private board file"
    );
    Ok(file)
}
