use super::*;

thread_local! { static UNTIL: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) }; }
pub(super) struct DeadlineGuard(Option<Instant>);
impl DeadlineGuard {
    pub(super) fn new(until: Instant) -> Self {
        Self(UNTIL.with(|clock| clock.replace(Some(until))))
    }
}
impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        UNTIL.with(|clock| clock.set(self.0));
    }
}

pub(super) fn execute(
    mut command: Command,
    input: Option<Vec<u8>>,
) -> Result<std::process::Output> {
    let until = UNTIL.with(|clock| clock.get()).unwrap_or_else(deadline);
    within(until)?;
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().expect("configured pipe");
        std::thread::spawn(move || stdin.write_all(&bytes))
    });
    fn collect(mut stream: impl Read) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        stream
            .by_ref()
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut output)?;
        ensure!(
            output.len() <= 64 * 1024 * 1024,
            "Git inspection output exceeds supported bound"
        );
        Ok(output)
    }
    let stdout = child.stdout.take().expect("configured pipe");
    let stderr = child.stderr.take().expect("configured pipe");
    let stdout = std::thread::spawn(move || collect(stdout));
    let stderr = std::thread::spawn(move || collect(stderr));
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            break Err(anyhow::anyhow!(
                "Git inspection exceeded preparation deadline"
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let out = stdout
        .join()
        .map_err(|_| anyhow::anyhow!("Git output reader failed"))?;
    let err = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("Git error reader failed"))?;
    if let Some(writer) = writer {
        let written = writer
            .join()
            .map_err(|_| anyhow::anyhow!("Git input writer failed"))?;
        if status.is_ok() {
            written?;
        }
    }
    Ok(std::process::Output {
        status: status?,
        stdout: out?,
        stderr: err?,
    })
}

pub(super) fn command() -> Command {
    let mut command = Command::new("/usr/bin/git");
    // Keep only the ordinary executable search path. In particular, no ambient
    // GIT_*, injected loaders, askpass program, account token or HOME config.
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("LC_ALL", "C")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("maintenance.auto=false")
        .arg("-c")
        .arg("gc.auto=0");
    command
}

pub(super) fn run(private: &Path, worktree: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = command();
    command
        .arg(format!("--git-dir={}", private.join(".git").display()))
        .arg(format!("--work-tree={}", worktree.display()))
        .args(args);
    let output = execute(command, None)?;
    ensure!(
        output.status.success(),
        "private Git {} failed: {}",
        args.first().unwrap_or(&"operation"),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

pub(super) fn configuration(root: &File, scan: &Inventory) -> Result<(String, String)> {
    let mut source = open_relative(root, ".git/config", false)?;
    ensure!(
        source.metadata()?.len() <= 1024 * 1024,
        "Git configuration exceeds supported bound"
    );
    let mut bytes = Vec::new();
    source.read_to_end(&mut bytes)?;
    let mut command = command();
    command.args(["config", "--no-includes", "--file", "-", "--null", "--list"]);
    let output = execute(command, Some(bytes))?;
    ensure!(
        output.status.success(),
        "cannot parse repository configuration"
    );
    let mut values = BTreeMap::new();
    for record in output.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record = std::str::from_utf8(record).context("non-UTF-8 Git configuration")?;
        let (key, value) = record.split_once('\n').unwrap_or((record, "true"));
        values.insert(key.to_ascii_lowercase(), value.to_owned());
    }
    for key in [
        "core.worktree",
        "core.sparsecheckout",
        "core.sparsecheckoutcone",
        "core.splitindex",
        "extensions.worktreeconfig",
        "extensions.partialclone",
        "extensions.refstorage",
    ] {
        ensure!(
            !values.contains_key(key),
            "unsupported repository configuration: {key}"
        );
    }
    ensure!(
        values.get("core.bare").is_none_or(|s| s == "false"),
        "bare repository is unsupported"
    );
    for (key, value) in &values {
        ensure!(
            !key.starts_with("filter."),
            "content filters require a qualified adapter"
        );
        ensure!(
            !key.starts_with("include.") && !key.starts_with("includeif."),
            "included Git configuration requires a qualified adapter"
        );
        ensure!(
            !(key.starts_with("remote.")
                && (key.ends_with(".promisor") || key.ends_with(".partialclonefilter"))),
            "partial object storage is unsupported"
        );
        if key.starts_with("extensions.") {
            ensure!(
                key == "extensions.objectformat",
                "unsupported Git extension: {key}"
            );
        }
        ensure!(!value.contains('\0'), "invalid Git configuration");
    }
    let format = values
        .get("extensions.objectformat")
        .map(String::as_str)
        .unwrap_or("sha1");
    ensure!(
        format == "sha1" || format == "sha256",
        "unsupported Git object format"
    );
    let version = values
        .get("core.repositoryformatversion")
        .map(String::as_str)
        .unwrap_or("0");
    ensure!(
        version == "0" || version == "1",
        "unsupported Git repository format"
    );
    for path in scan.entries.keys().filter(|p| p.starts_with(".git/")) {
        let name = path.strip_prefix(".git/").unwrap();
        let first = name.split('/').next().unwrap();
        ensure!(!name.ends_with(".lock"), "active Git lock: {name}");
        ensure!(
            ![
                "commondir",
                "worktrees",
                "modules",
                "shallow",
                "reftable",
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "REBASE_HEAD",
                "rebase-apply",
                "rebase-merge",
                "sequencer"
            ]
            .contains(&first)
                && !first.starts_with("BISECT_")
                && !first.starts_with("sharedindex."),
            "unsupported or unfinished Git state: {name}"
        );
        ensure!(
            ![
                "objects/info/alternates",
                "objects/info/http-alternates",
                "info/grafts",
                "info/sparse-checkout"
            ]
            .contains(&name)
                && !name.starts_with("refs/replace/")
                && !name.ends_with(".promisor"),
            "unsupported Git administration: {name}"
        );
    }
    let mut config = format!(
        "[core]\n\trepositoryformatversion = {}\n\tbare = false\n\thooksPath = /dev/null\n\tfsmonitor = false\n\tuntrackedCache = false\n\tattributesFile = /dev/null\n\texcludesFile = /dev/null\n",
        if format == "sha256" { 1 } else { 0 }
    );
    for key in [
        "filemode",
        "ignorecase",
        "precomposeunicode",
        "symlinks",
        "autocrlf",
        "eol",
        "safecrlf",
        "checkroundtripencoding",
    ] {
        if let Some(value) = values.get(&format!("core.{key}")) {
            let valid = match key {
                "autocrlf" => ["true", "false", "input"].contains(&value.as_str()),
                "eol" => ["lf", "crlf", "native"].contains(&value.as_str()),
                "safecrlf" => ["true", "false", "warn"].contains(&value.as_str()),
                "checkroundtripencoding" => {
                    !value.is_empty()
                        && value
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b" ,-".contains(&c))
                }
                _ => ["true", "false"].contains(&value.as_str()),
            };
            ensure!(valid, "unsupported value for core.{key}");
            config.push_str(&format!("\t{key} = {value}\n"));
        }
    }
    config.push_str("[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n\tautoPackLimit = 0\n[credential]\n\thelper =\n");
    if format == "sha256" {
        config.push_str("[extensions]\n\tobjectFormat = sha256\n");
    }
    Ok((format.to_owned(), config))
}

pub(super) fn admitted_admin(path: &str) -> bool {
    [".git/objects", ".git/refs"]
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
        || [
            ".git/HEAD",
            ".git/packed-refs",
            ".git/index",
            ".git/info/exclude",
            ".git/info/attributes",
        ]
        .contains(&path)
}

pub(super) fn initialize(baseline: &Path, format: &str, config: &str) -> Result<()> {
    let mut command = command();
    command
        .args([
            "init",
            "--quiet",
            "--template=",
            &format!("--object-format={format}"),
        ])
        .arg(baseline);
    let output = execute(command, None)?;
    ensure!(
        output.status.success(),
        "private Git initialization failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(baseline.join(".git/config"), config)?;
    // init creates an unborn HEAD. The captured source HEAD replaces it.
    fs::remove_file(baseline.join(".git/HEAD"))?;
    Ok(())
}

pub(super) fn validate(baseline: &Path, original: &Path) -> Result<BTreeSet<String>> {
    let root = run(baseline, original, &["rev-parse", "--show-toplevel"])?;
    ensure!(
        Path::new(std::str::from_utf8(&root)?.trim_end_matches('\n')) == original,
        "Git root does not match explicit project selection"
    );
    let entries = run(
        baseline,
        original,
        &["ls-files", "--stage", "--sparse", "-z"],
    )?;
    for entry in entries.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record = std::str::from_utf8(entry)?;
        let (state, path) = record.split_once('\t').context("invalid Git index entry")?;
        let state: Vec<_> = state.split(' ').collect();
        ensure!(
            state.len() == 3 && state[2] == "0",
            "unmerged index entry: {path}"
        );
        ensure!(
            state[0] != "160000" && state[0] != "040000",
            "submodule or sparse index unsupported: {path}"
        );
        components(path, false)?;
    }
    let shared = run(baseline, original, &["rev-parse", "--shared-index-path"])?;
    ensure!(shared.is_empty(), "split Git index is unsupported");
    if baseline.join(".git/index").exists() {
        run(
            baseline,
            baseline,
            &["update-index", "--no-fsmonitor", "--no-untracked-cache"],
        )?;
        ensure!(
            run(
                baseline,
                original,
                &["ls-files", "--stage", "--sparse", "-z"]
            )? == entries,
            "private cache removal changed staged entries"
        );
    }
    let files = run(
        baseline,
        original,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut selected = BTreeSet::new();
    for path in files.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = std::str::from_utf8(path)?.to_owned();
        components(&path, false)?;
        selected.insert(path);
    }
    let mut input = Vec::new();
    for name in &selected {
        input.extend(name.as_bytes());
        input.push(0);
    }
    let mut command = command();
    command
        .arg(format!("--git-dir={}", baseline.join(".git").display()))
        .arg(format!("--work-tree={}", original.display()))
        .args(["check-attr", "--stdin", "-z", "--all"]);
    let output = execute(command, Some(input))?;
    ensure!(
        output.status.success(),
        "cannot inspect repository attributes"
    );
    let fields: Vec<_> = output
        .stdout
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .collect();
    ensure!(fields.len().is_multiple_of(3), "invalid attribute output");
    for entry in fields.chunks(3) {
        ensure!(
            entry[1] != b"filter"
                || [b"unspecified".as_slice(), b"unset".as_slice()].contains(&entry[2]),
            "active content filter requires a qualified adapter: {}",
            String::from_utf8_lossy(entry[0])
        );
    }
    Ok(selected)
}
