//! Private saved-state capture and guarded delivery to the selected project.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 1_000_000;
const MAX_DEPTH: usize = 128;
const MAX_ATTRIBUTE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub kind: FileKind,
    pub size: u64,
    pub mode: u32,
    pub sha256: Option<String>,
    pub link_target: Option<String>,
    pub xattrs_sha256: String,
    pub xattrs_bytes: u64,
    pub acl_sha256: String,
    pub flags: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub files: BTreeMap<String, FileEntry>,
    #[serde(default)]
    pub exclusions: BTreeMap<String, String>,
    /// Qualified interpreter dependencies are retained in place, not copied
    /// into source review or silently omitted from result verification.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime_links: BTreeMap<String, RuntimeLink>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PreparedWorkspace {
    pub original: PathBuf,
    pub run_dir: PathBuf,
    pub baseline: PathBuf,
    pub workers: [PathBuf; 2],
    pub baseline_manifest: Manifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mode: u32,
    nlink: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    uid: u32,
    gid: u32,
    flags: u32,
}
impl Identity {
    fn read(file: &File) -> Result<Self> {
        let m = file.metadata()?;
        ensure!(
            m.size() <= i64::MAX as u64,
            "negative or unrepresentable file length"
        );
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mode: m.mode(),
            nlink: m.nlink(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
            uid: m.uid(),
            gid: m.gid(),
            flags: flags(file)?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Inventory {
    entries: BTreeMap<String, (Identity, FileEntry)>,
    bytes: u64,
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(crate::config::PREPARATION_SECONDS)
}
fn within(until: Instant) -> Result<()> {
    ensure!(
        Instant::now() < until,
        "repository preparation exceeded its time bound"
    );
    Ok(())
}
fn cstr(value: &OsStr) -> Result<CString> {
    Ok(CString::new(value.as_bytes())?)
}
fn open_dir(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open directory without following links: {}", path.display()))
}
fn open_at(parent: &File, name: &OsStr, flags: i32) -> Result<File> {
    let name = cstr(name)?;
    #[cfg(target_os = "macos")]
    let nofollow = if flags & libc::O_SYMLINK != 0 {
        0
    } else {
        libc::O_NOFOLLOW
    };
    #[cfg(not(target_os = "macos"))]
    let nofollow = libc::O_NOFOLLOW;
    // SAFETY: the descriptor and terminated name remain valid for this call.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | nofollow | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open contained entry");
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn components(relative: &str, allow_git: bool) -> Result<Vec<&OsStr>> {
    ensure!(
        !relative.is_empty() && !relative.contains('\0'),
        "empty or invalid relative path"
    );
    let path = Path::new(relative);
    let mut result = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => {
                ensure!(
                    allow_git || !name.as_bytes().eq_ignore_ascii_case(b".git"),
                    "Git administration is protected"
                );
                result.push(name);
            }
            _ => bail!("path must contain only ordinary relative components"),
        }
    }
    ensure!(
        !result.is_empty() && result.len() <= MAX_DEPTH,
        "invalid path depth"
    );
    // Components normalizes embedded '.' and duplicate separators. Refuse both.
    ensure!(
        result.iter().map(|p| p.as_bytes().len()).sum::<usize>() + result.len() - 1
            == relative.len(),
        "noncanonical relative path"
    );
    Ok(result)
}
fn open_relative(root: &File, path: &str, directory: bool) -> Result<File> {
    let parts = components(path, true)?;
    let mut parent = root.try_clone()?;
    for (i, part) in parts.iter().enumerate() {
        let flags = if i + 1 < parts.len() || directory {
            libc::O_RDONLY | libc::O_DIRECTORY
        } else {
            libc::O_RDONLY
        };
        parent = open_at(&parent, part, flags)?;
    }
    Ok(parent)
}

/// Validate a board path. Callers doing writes must also pin parent descriptors.
pub fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let parts = components(relative, false)?;
    let mut dir = open_dir(root)?;
    for (i, part) in parts.iter().enumerate() {
        match open_at(
            &dir,
            part,
            libc::O_RDONLY
                | libc::O_NONBLOCK
                | if i + 1 < parts.len() {
                    libc::O_DIRECTORY
                } else {
                    0
                },
        ) {
            Ok(next) => {
                ensure!(
                    next.metadata()?.is_dir() || next.metadata()?.is_file(),
                    "unsupported path type"
                );
                dir = next;
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(root.join(relative))
}

/// Native COW only. A failed clone never falls back to reading and writing bytes.
pub fn clone_file(source: &Path, dest: &Path) -> Result<()> {
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)?;
    let parent = open_dir(dest.parent().context("clone destination has no parent")?)?;
    clone_file_to_dir(
        &source,
        &parent,
        dest.file_name().context("clone destination has no name")?,
    )
}

pub fn clone_file_to_dir(source: &File, destination_parent: &File, name: &OsStr) -> Result<()> {
    ensure!(
        source.metadata()?.is_file(),
        "COW source must be a regular file"
    );
    ensure!(
        !name.as_bytes().contains(&b'/') && name != "." && name != ".." && !name.is_empty(),
        "clone destination must be a filename"
    );
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn fclonefileat(srcfd: i32, dstfd: i32, dst: *const libc::c_char, flags: u32) -> i32;
        }
        let name = cstr(name)?;
        // SAFETY: descriptors are live and destination is one terminated basename.
        if unsafe {
            fclonefileat(
                source.as_raw_fd(),
                destination_parent.as_raw_fd(),
                name.as_ptr(),
                4,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("native COW clone failed; copying fallback is disabled");
        }
        let cloned = open_at(
            destination_parent,
            OsStr::from_bytes(name.as_bytes()),
            libc::O_RDONLY,
        )?;
        ensure!(
            (cloned.metadata()?.dev(), cloned.metadata()?.ino())
                != (source.metadata()?.dev(), source.metadata()?.ino()),
            "clone reused source inode"
        );
        cloned.sync_all()?;
        destination_parent.sync_all()?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = destination_parent;
        bail!("native COW backend requires macOS")
    }
}

fn names(dir: &File) -> Result<Vec<OsString>> {
    // dup shares directory offsets, so open a new description for every scan.
    let scan = open_at(dir, OsStr::new("."), libc::O_RDONLY | libc::O_DIRECTORY)?;
    let fd = std::os::fd::IntoRawFd::into_raw_fd(scan);
    // SAFETY: fd ownership is transferred to the directory stream.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        unsafe { libc::close(fd) };
        return Err(std::io::Error::last_os_error()).context("enumerate directory");
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let _guard = Stream(stream);
    let mut result = Vec::new();
    loop {
        // readdir requires errno=0 to distinguish end from inspection failure.
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            ensure!(error.raw_os_error() == Some(0), "read directory: {error}");
            break;
        }
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            result.push(OsString::from_vec(bytes.to_vec()));
        }
        ensure!(result.len() <= MAX_ENTRIES, "directory exceeds entry bound");
    }
    result.sort();
    Ok(result)
}

#[cfg(target_os = "macos")]
fn attributes(file: &File) -> Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    let fd = file.as_raw_fd();
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
    ensure!(
        size >= 0,
        "list extended attributes: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        size as usize <= MAX_ATTRIBUTE_BYTES,
        "extended attribute names exceed bound"
    );
    let mut list = vec![0u8; size as usize];
    let count = unsafe { libc::flistxattr(fd, list.as_mut_ptr().cast(), list.len(), 0) };
    ensure!(
        count == size,
        "extended attributes changed during inspection"
    );
    let mut result = BTreeMap::new();
    for name in list.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let cname = CString::new(name)?;
        let size = unsafe { libc::fgetxattr(fd, cname.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
        ensure!(
            size >= 0,
            "inspect extended attribute: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            size as usize <= MAX_ATTRIBUTE_BYTES,
            "extended attribute value exceeds supported bound"
        );
        let mut value = vec![0u8; size as usize];
        let count = unsafe {
            libc::fgetxattr(
                fd,
                cname.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        ensure!(
            count == size,
            "extended attribute changed during inspection"
        );
        result.insert(name.to_vec(), value);
    }
    Ok(result)
}
#[cfg(not(target_os = "macos"))]
fn attributes(_file: &File) -> Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    bail!("extended attribute admission requires macOS")
}

fn inspect(
    file: &File,
    kind: FileKind,
    target: Option<String>,
    until: Instant,
) -> Result<(Identity, FileEntry, u64)> {
    let before = Identity::read(file)?;
    let attrs = attributes(file)?;
    let mut attr_hash = Sha256::new();
    let mut bytes = before.size;
    for (name, value) in &attrs {
        bytes = bytes
            .checked_add(value.len() as u64)
            .context("repository size overflow")?;
        attr_hash.update((name.len() as u64).to_le_bytes());
        attr_hash.update(name);
        attr_hash.update((value.len() as u64).to_le_bytes());
        attr_hash.update(value);
    }
    let sha256 = if kind == FileKind::File {
        let mut reader = file.try_clone()?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            within(until)?;
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        Some(format!("{:x}", hash.finalize()))
    } else {
        None
    };
    let entry = FileEntry {
        kind: kind.clone(),
        size: if kind == FileKind::Directory {
            0
        } else {
            before.size
        },
        mode: before.mode & 0o7777,
        sha256,
        link_target: target,
        xattrs_sha256: format!("{:x}", attr_hash.finalize()),
        xattrs_bytes: bytes - before.size,
        acl_sha256: acl_digest(file)?,
        flags: before.flags,
    };
    ensure!(
        before == Identity::read(file)?,
        "source changed during inspection"
    );
    Ok((before, entry, bytes))
}

fn flags(file: &File) -> Result<u32> {
    #[cfg(target_os = "macos")]
    {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        ensure!(
            unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } == 0,
            "inspect file flags: {}",
            std::io::Error::last_os_error()
        );
        let flags = unsafe { stat.assume_init() }.st_flags;
        ensure!(
            flags & 0xC09E_0086 == 0,
            "immutable, protected, dataless or mounted input is unsupported"
        );
        Ok(flags)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        Ok(0)
    }
}
fn acl_digest(file: &File) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn acl_get_fd_np(fd: i32, kind: i32) -> *mut libc::c_void;
            fn acl_to_text(acl: *mut libc::c_void, length: *mut libc::ssize_t)
            -> *mut libc::c_char;
            fn acl_free(pointer: *mut libc::c_void) -> i32;
        }
        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(format!("{:x}", Sha256::digest([])));
            }
            return Err(error).context("inspect access control metadata");
        }
        let mut len = 0;
        let text = unsafe { acl_to_text(acl, &mut len) };
        if text.is_null() {
            unsafe {
                acl_free(acl);
            }
            bail!("cannot inspect access control metadata");
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(unsafe { CStr::from_ptr(text) }.to_bytes())
        );
        unsafe {
            acl_free(text.cast());
            acl_free(acl);
        }
        Ok(digest)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        bail!("access control metadata requires macOS")
    }
}

fn inventory(root: &File, limit: u64, until: Instant, include_git: bool) -> Result<Inventory> {
    inventory_filtered(root, limit, until, include_git, None)
}

fn inventory_filtered(
    root: &File,
    limit: u64,
    until: Instant,
    include_git: bool,
    skip: Option<&dyn Fn(&str) -> bool>,
) -> Result<Inventory> {
    let mut result = Inventory {
        entries: BTreeMap::new(),
        bytes: 0,
    };
    let device = root.metadata()?.dev();
    let (id, entry, bytes) = inspect(root, FileKind::Directory, None, until)?;
    result.bytes = bytes;
    result.entries.insert(String::new(), (id, entry));
    ensure!(
        result.bytes < limit,
        "repository is at least the size limit of {limit} bytes"
    );
    scan_dir(
        root,
        "",
        device,
        limit,
        until,
        include_git,
        skip,
        &mut result,
    )?;
    Ok(result)
}
#[allow(clippy::too_many_arguments)]
fn scan_dir(
    dir: &File,
    prefix: &str,
    device: u64,
    limit: u64,
    until: Instant,
    include_git: bool,
    skip: Option<&dyn Fn(&str) -> bool>,
    result: &mut Inventory,
) -> Result<()> {
    ensure!(
        prefix.split('/').count() <= MAX_DEPTH,
        "repository exceeds supported depth"
    );
    let before = Identity::read(dir)?;
    for name in names(dir)? {
        within(until)?;
        let text = name
            .to_str()
            .context("non-UTF-8 repository paths are unsupported")?;
        if !include_git && prefix.is_empty() && text.eq_ignore_ascii_case(".git") {
            continue;
        }
        let relative = if prefix.is_empty() {
            text.to_owned()
        } else {
            format!("{prefix}/{text}")
        };
        if skip.is_some_and(|filter| filter(&relative)) {
            continue;
        }
        let cname = cstr(&name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        ensure!(
            unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    cname.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } == 0,
            "inspect {relative}: {}",
            std::io::Error::last_os_error()
        );
        let stat = unsafe { stat.assume_init() };
        let kind = match stat.st_mode as u32 & libc::S_IFMT as u32 {
            x if x == libc::S_IFDIR as u32 => FileKind::Directory,
            x if x == libc::S_IFREG as u32 => FileKind::File,
            x if x == libc::S_IFLNK as u32 => FileKind::Symlink,
            _ => bail!("unsupported special file: {relative}"),
        };
        ensure!(
            stat.st_dev as u64 == device,
            "nested mount is unsupported: {relative}"
        );
        ensure!(
            stat.st_size >= 0,
            "invalid negative file length: {relative}"
        );
        ensure!(
            result
                .bytes
                .checked_add(stat.st_size as u64)
                .context("repository size overflow")?
                < limit,
            "repository is at least the size limit of {limit} bytes"
        );
        #[cfg(target_os = "macos")]
        ensure!(
            stat.st_flags & (0x40000000 | 0x80000000) == 0,
            "cloud placeholder or dataless input unsupported: {relative}"
        );
        if kind == FileKind::Symlink {
            ensure!(
                !relative.starts_with(".git/"),
                "Git administration symlink is unsupported: {relative}"
            );
        }
        let flags = match kind {
            FileKind::Directory => libc::O_RDONLY | libc::O_DIRECTORY,
            FileKind::File => libc::O_RDONLY,
            FileKind::Symlink => {
                #[cfg(target_os = "macos")]
                {
                    libc::O_RDONLY | libc::O_SYMLINK
                }
                #[cfg(not(target_os = "macos"))]
                {
                    bail!("symlink admission requires macOS")
                }
            }
        };
        let file = open_at(dir, &name, flags)?;
        ensure!(
            file.metadata()?.ino() == stat.st_ino && file.metadata()?.dev() == device,
            "entry replaced during inspection: {relative}"
        );
        let target = if kind == FileKind::Symlink {
            let mut bytes = vec![0u8; 65536];
            let length = unsafe {
                libc::readlinkat(
                    dir.as_raw_fd(),
                    cname.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            ensure!(
                length >= 0 && (length as usize) < bytes.len(),
                "cannot read symbolic link: {relative}"
            );
            bytes.truncate(length as usize);
            let target = String::from_utf8(bytes).context("non-UTF-8 link target")?;
            Some(target)
        } else {
            None
        };
        let (identity, entry, bytes) = inspect(&file, kind.clone(), target, until)?;
        result.bytes = result
            .bytes
            .checked_add(bytes)
            .context("repository size overflow")?;
        ensure!(
            result.bytes < limit,
            "repository is at least the size limit of {limit} bytes"
        );
        result.entries.insert(relative.clone(), (identity, entry));
        ensure!(
            result.entries.len() <= MAX_ENTRIES,
            "repository exceeds entry bound"
        );
        if kind == FileKind::Directory {
            scan_dir(
                &file,
                &relative,
                device,
                limit,
                until,
                include_git,
                skip,
                result,
            )?;
        }
    }
    ensure!(
        before == Identity::read(dir)?,
        "directory changed during inspection: {prefix}"
    );
    Ok(())
}

/// Exact logical metric, including root, ignored entries, Git and xattr values.
pub fn measure_repository(root: &Path) -> Result<u64> {
    Ok(inventory(&open_dir(root)?, u64::MAX, deadline(), true)?.bytes)
}
pub fn manifest(root: &Path) -> Result<Manifest> {
    let result = manifest_inventory(root)?;
    validate_links(&result.files)?;
    Ok(result)
}

fn manifest_inventory(root: &Path) -> Result<Manifest> {
    let scan = inventory(&open_dir(root)?, u64::MAX, deadline(), false)?;
    Ok(Manifest {
        files: scan
            .entries
            .into_iter()
            .filter(|(p, _)| !p.is_empty())
            .map(|(p, (_, e))| (p, e))
            .collect(),
        exclusions: BTreeMap::new(),
        runtime_links: BTreeMap::new(),
    })
}

fn validate_links(entries: &BTreeMap<String, FileEntry>) -> Result<()> {
    validate_links_except(entries, &BTreeSet::new())
}

fn validate_links_except(
    entries: &BTreeMap<String, FileEntry>,
    qualified_interpreters: &BTreeSet<String>,
) -> Result<()> {
    for (path, entry) in entries {
        if let Some(target) = &entry.link_target {
            if qualified_interpreters.contains(path) {
                continue;
            }
            // Inventory accounts for every link without following it, including
            // ignored development environments. Containment applies only when
            // a link is actually admitted as a source or returned artifact.
            ensure!(
                !Path::new(target).is_absolute(),
                "absolute link unsupported: {path}"
            );
            let mut parts: Vec<String> = path.split('/').map(str::to_owned).collect();
            parts.pop();
            let mut pending: std::collections::VecDeque<String> =
                target.split('/').map(str::to_owned).collect();
            let mut hops = 0;
            while let Some(part) = pending.pop_front() {
                match part.as_str() {
                    "" | "." => continue,
                    ".." => {
                        ensure!(parts.pop().is_some(), "link escapes captured tree: {path}");
                    }
                    _ => parts.push(part),
                }
                let current = parts.join("/");
                ensure!(
                    current.is_empty() || entries.contains_key(&current),
                    "link has a missing target component: {path}"
                );
                if let Some(link) = entries.get(&current).and_then(|e| e.link_target.as_ref()) {
                    ensure!(
                        !Path::new(link).is_absolute(),
                        "link reaches an external target: {path}"
                    );
                    hops += 1;
                    ensure!(hops < 40, "link cycle: {path}");
                    parts.pop();
                    for part in link.split('/').rev() {
                        pending.push_front(part.to_owned());
                    }
                } else if !pending.is_empty() && !current.is_empty() {
                    ensure!(
                        entries[&current].kind == FileKind::Directory,
                        "link traverses a non-directory: {path}"
                    );
                }
            }
            let resolved = parts.join("/");
            ensure!(
                entries.contains_key(&resolved) && !resolved.starts_with(".git/"),
                "link target not captured or escapes: {path}"
            );
        }
    }
    Ok(())
}

mod delivery;
mod git;
mod output;
mod prepare;
mod recovery;
mod result;
pub use delivery::{
    DeliveryReport, RecoveryReport, deliver_accepted_result, deliver_result,
    preserve_partial_and_cleanup, preserve_partial_and_cleanup_with_artifacts,
};
pub use output::{AcceptedResult, ResultSelection};
pub use prepare::{prepare, retain_result, retain_result_with_policy};
pub use recovery::{RecoveryExport, RecoveryInspection, export_recovery, inspect_recovery};
pub use result::{ResultPolicy, RuntimeLink, manifest_for_result};

#[cfg(test)]
mod tests;
