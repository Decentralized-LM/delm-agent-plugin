//! Explicit user-selected inputs, captured before workers can read them.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_FILES: usize = 16;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u64,
    files: Vec<SelectedFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedFile {
    path: PathBuf,
    kind: Kind,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Image,
    File,
}

pub fn parse_manifest(text: &str) -> Result<Vec<Value>> {
    let manifest: Manifest =
        serde_json::from_str(text).context("Invalid selected-input manifest")?;
    ensure!(
        manifest.version == 1,
        "Unsupported selected-input manifest version"
    );
    ensure!(
        manifest.files.len() <= MAX_FILES,
        "Select at most 16 input files"
    );
    manifest.files.into_iter().map(|file| {
        ensure!(file.path.is_absolute(), "Selected inputs need absolute paths");
        Ok(json!({"type":match file.kind {Kind::Image => "localImage", Kind::File => "file"}, "path":file.path}))
    }).collect()
}

fn read_selected(path: &Path) -> Result<(fs::Metadata, Vec<u8>)> {
    ensure!(path.is_absolute(), "Selected inputs need absolute paths");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_FILE_BYTES,
        "Select a regular input file no larger than 32 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == metadata.len(),
        "Selected input changed while being read"
    );
    let name = path
        .file_name()
        .context("Selected input has no filename")?
        .to_string_lossy()
        .to_ascii_lowercase();
    ensure!(
        !(name == ".env"
            || name.starts_with(".env.")
            || [
                "auth.json",
                "credentials.json",
                ".netrc",
                "id_rsa",
                "id_ed25519",
                "id_ecdsa"
            ]
            .contains(&name.as_str())),
        "Account credentials cannot be supplied as task inputs"
    );
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
    ensure!(
        !(prefix.contains("-----BEGIN") && prefix.contains("PRIVATE KEY-----")),
        "Private keys cannot be supplied as task inputs"
    );
    ensure!(
        !(name == ".npmrc" && (prefix.contains("_auth") || prefix.contains("password"))),
        "Package-manager credentials cannot be supplied as task inputs"
    );
    Ok((metadata, bytes))
}

pub struct StagedInputs {
    directory: Option<PathBuf>,
    destination: PathBuf,
    inputs: Vec<Value>,
}
impl StagedInputs {
    pub fn publish(mut self) -> Result<Vec<Value>> {
        if let Some(directory) = &self.directory {
            fs::create_dir_all(
                self.destination
                    .parent()
                    .context("Input destination has no parent")?,
            )?;
            fs::rename(directory, &self.destination)?;
            self.directory = None;
        }
        Ok(std::mem::take(&mut self.inputs))
    }
}
impl Drop for StagedInputs {
    fn drop(&mut self) {
        if let Some(directory) = &self.directory {
            // Only this newly created, runtime-private scratch directory is removed.
            let _ = fs::remove_dir_all(directory);
        }
    }
}

pub fn capture(selected: &[Value], run: &Path) -> Result<Vec<Value>> {
    stage(selected, run)?.publish()
}

pub fn stage(selected: &[Value], run: &Path) -> Result<StagedInputs> {
    ensure!(selected.len() <= MAX_FILES, "Select at most 16 input files");
    if selected.is_empty() {
        return Ok(StagedInputs {
            directory: None,
            destination: PathBuf::new(),
            inputs: Vec::new(),
        });
    }
    // Validate the entire batch before creating its immutable private copy.
    let mut files = Vec::new();
    let mut total = 0;
    for item in selected {
        let kind = item["type"].as_str().unwrap_or("");
        ensure!(
            matches!(kind, "localImage" | "local_image" | "file"),
            "Supply a local image or file; remote session attachment IDs are not portable"
        );
        let path = PathBuf::from(
            item["path"]
                .as_str()
                .context("Selected input path is missing")?,
        );
        let (metadata, bytes) = read_selected(&path)?;
        total += metadata.len();
        ensure!(total <= MAX_BATCH_BYTES, "Selected inputs exceed 128 MiB");
        files.push((path, kind, metadata, bytes));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let directory = run.join(format!("input-capture-{id}"));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut staged = StagedInputs {
        directory: Some(directory.clone()),
        destination: run.join("attachments").join(id),
        inputs: Vec::new(),
    };
    let mut records = Vec::new();
    for (index, (source, kind, before, bytes)) in files.into_iter().enumerate() {
        let name = source
            .file_name()
            .context("Selected input has no filename")?;
        let filename = format!("{index}-{}", name.to_string_lossy());
        let destination = directory.join(&filename);
        let published = staged.destination.join(&filename);
        crate::workspace::clone_file(&source, &destination)
            .context("Selected input must support copy-on-write capture on this volume")?;
        let after = fs::symlink_metadata(&source)?;
        ensure!(
            after.is_file()
                && before.dev() == after.dev()
                && before.ino() == after.ino()
                && before.len() == after.len()
                && before.mtime() == after.mtime()
                && before.mtime_nsec() == after.mtime_nsec()
                && fs::read(&destination)? == bytes,
            "Selected input changed during capture; no update was sent"
        );
        let digest = format!("{:x}", Sha256::digest(&bytes));
        records.push(json!({"name":name.to_string_lossy(),"path":published,"sha256":digest,"bytes":bytes.len(),"kind":kind}));
        match kind {
            "localImage" | "local_image" => staged.inputs.push(json!({"type":"localImage","path":published})),
            "file" => staged.inputs.push(crate::workers::text_input(&format!(
                "The user selected this reference file. Read its private captured copy as needed: {}", serde_json::to_string(&published)?))),
            _ => bail!("Unsupported selected input"),
        }
    }
    crate::run::state::atomic_json(&directory.join("manifest.json"), &records)?;
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_inputs_are_explicit_and_bounded() {
        assert!(parse_manifest(r#"{"version":2,"files":[]}"#).is_err());
        assert!(
            parse_manifest(r#"{"version":1,"files":[{"kind":"file","path":"relative"}]}"#).is_err()
        );
        assert!(
            capture(
                &[json!({"type":"image","fileId":"parent-only"})],
                Path::new("/tmp")
            )
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        fs::write(&path, "KEY=private").unwrap();
        assert!(read_selected(&path).is_err());
        let normal = dir.path().join("spec.txt");
        fs::write(&normal, "user reference").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&normal, &link).unwrap();
        assert!(read_selected(&link).is_err());
        assert!(read_selected(dir.path()).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn captured_inputs_keep_names_bytes_and_independent_update_versions() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("reference.md");
        fs::write(&source, "first").unwrap();
        let selected = vec![json!({"type":"file","path":source})];
        let first = capture(&selected, dir.path()).unwrap();
        fs::write(&source, "second").unwrap();
        let second = capture(&selected, dir.path()).unwrap();
        assert_ne!(first, second);
        let batches = fs::read_dir(dir.path().join("attachments"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(batches.len(), 2);
        let contents = batches
            .into_iter()
            .map(|entry| fs::read_to_string(entry.unwrap().path().join("0-reference.md")).unwrap())
            .collect::<Vec<_>>();
        assert!(contents.contains(&"first".into()) && contents.contains(&"second".into()));
    }
}
