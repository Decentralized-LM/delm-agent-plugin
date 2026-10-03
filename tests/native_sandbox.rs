//! Explicit qualification against the installed stock Codex host. This
//! invokes the real macOS sandbox and never starts a model turn.
#![cfg(target_os = "macos")]

use delm::{
    protocol::StartRequest,
    workers::{RpcClient, verify_thread_response, worker_config},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    time::Duration,
};

struct ProbeSocket(PathBuf);
impl Drop for ProbeSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn toml_value(value: &Value) -> String {
    match value {
        Value::String(_) | Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(toml_value).collect::<Vec<_>>().join(",")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{}={}", json!(key), toml_value(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Null => panic!("native worker config cannot contain TOML null"),
    }
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, path: &Path, output: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, output);
            } else {
                output.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

#[tokio::test]
#[ignore = "requires DELM_TEST_HOST pointing at installed stock Codex and real macOS sandbox authority"]
async fn native_named_worker_profile_enforces_private_filesystem_authority() {
    let restricted = qualify(false, 1).await;
    let first = qualify(true, 1).await;
    let second = qualify(true, 2).await;
    let evidence =
        json!({"network_disabled":restricted,"network_enabled":[first,second],"model_turns":0});
    if let Some(path) = std::env::var_os("DELM_TEST_EVIDENCE") {
        fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
    println!("{evidence}");
}

async fn qualify(network_enabled: bool, worker: usize) -> Value {
    let host =
        std::env::var_os("DELM_TEST_HOST").expect("set DELM_TEST_HOST to installed stock Codex");
    let host = Path::new(&host).canonicalize().unwrap();
    let version = delm::compatibility::host_version(&host).await.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let original = base.join("original");
    let run = base.join("run");
    let project = run.join(format!("workspace/worker-{worker}"));
    let baseline = run.join("workspace/baseline");
    let peer = run.join(format!("workspace/worker-{}", 3 - worker));
    let auth = base.join("auth");
    for directory in [&original, &project, &baseline, &peer, &auth] {
        fs::create_dir_all(directory.join(".git")).unwrap();
        fs::write(directory.join("canary.txt"), "unchanged\n").unwrap();
        fs::write(directory.join(".git/canary"), "git unchanged\n").unwrap();
    }
    for root in [&original, &project] {
        for name in ["readonly", "denied"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(
                root.join(name).join("canary.txt"),
                "inherited restriction\n",
            )
            .unwrap();
        }
    }
    let before = tree(&original);
    let peer_before = tree(&peer);
    let control_root = Path::new("/tmp")
        .canonicalize()
        .unwrap()
        .join(format!("delm-{}", unsafe { libc::getuid() }));
    match fs::DirBuilder::new().mode(0o700).create(&control_root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("cannot create private socket-probe directory: {error}"),
    }
    let metadata = fs::symlink_metadata(&control_root).unwrap();
    assert!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0
    );
    let control_socket =
        ProbeSocket(control_root.join(format!("test-{}.sock", uuid::Uuid::new_v4())));
    let _listener = UnixListener::bind(&control_socket.0).unwrap();
    fs::set_permissions(&control_socket.0, fs::Permissions::from_mode(0o600)).unwrap();
    let request: StartRequest = serde_json::from_value(json!({
        "project":original,"task":"Qualify the native worker sandbox without a model turn",
        "model":"gpt-6-astra","model_provider":"openai","auth_home":auth,"host_executable":host,
        "policy":{"approval_policy":"never","sandbox":{"type":"workspace-write","network_access":network_enabled},
            "file_system":{"kind":"restricted","entries":[
                {"path":{"type":"special","value":{"kind":"root"}},"access":"read"},
                {"path":{"type":"path","path":original},"access":"write"},
                {"path":{"type":"path","path":original.join("readonly")},"access":"read"},
                {"path":{"type":"path","path":original.join("denied")},"access":"deny"}]},
            "network":if network_enabled {"enabled"} else {"restricted"},"network_proxy_active":false}
    })).unwrap();
    delm::compatibility::qualify(&request).await.unwrap();
    let config = worker_config(&request, &run, &project, worker).unwrap();
    let browser = if network_enabled {
        std::env::var_os("DELM_TEST_BROWSER_BUNDLE").map(|bundle| {
            // Qualification only: clone an already installed headless bundle into
            // this disposable private project. Never open a user's browser/profile.
            let bundle = PathBuf::from(bundle).canonicalize().unwrap();
            assert!(bundle.join("chrome-headless-shell").is_file());
            let target = project.join("browser-runtime");
            assert!(
                std::process::Command::new("/bin/cp")
                    .arg("-cR")
                    .arg(bundle)
                    .arg(&target)
                    .status()
                    .unwrap()
                    .success()
            );
            target.join("chrome-headless-shell")
        })
    } else {
        None
    };
    let profile = format!("delm_worker_{worker}");
    let control_token = run.join("control-token");
    fs::write(&control_token, "private control-token fixture\n").unwrap();
    fs::set_permissions(&control_token, fs::Permissions::from_mode(0o600)).unwrap();
    let config_text = config
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| format!("{}={}\n", json!(key), toml_value(value)))
        .collect::<String>();
    let mut rpc = RpcClient::spawn(&request, &run).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), rpc.initialize())
        .await
        .unwrap()
        .unwrap_or_else(|error| {
            panic!(
                "initialize failed: {error}; {}",
                fs::read_to_string(rpc.launch_dir.join("worker-host.log")).unwrap_or_default()
            )
        });
    let response = tokio::time::timeout(
        Duration::from_secs(20),
        rpc.request(
            "thread/start",
            json!({
                "cwd":project,"model":request.model,"modelProvider":"openai","permissions":profile,
                    "config":config,"ephemeral":true
            }),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    verify_thread_response(&request, &run, &project, worker, &response).unwrap();
    rpc.shutdown(&[]).await.unwrap();
    // command/exec selects a process-level named profile rather than a thread.
    // Use a fresh host with the identical worker profile in this isolated
    // CODEX_HOME; the first host above exercises production thread preflight.
    fs::write(auth.join("config.toml"), config_text).unwrap();
    let mut rpc = RpcClient::spawn(&request, &run).await.unwrap();
    rpc.initialize().await.unwrap();

    let probes = json!({
        "private":project.join("canary.txt"),"original":original.join("canary.txt"),
        "original_git":original.join(".git/canary"),"auth":auth.join("canary.txt"),
        "baseline":baseline.join("canary.txt"),"peer":peer.join("canary.txt"),
        "readonly":project.join("readonly/canary.txt"),"denied":project.join("denied/canary.txt"),
        "denied_alias":project.join("DENIED/CANARY.TXT"),
        "readonly_alias":project.join("READONLY/CANARY.TXT"),
        "home":config["shell_environment_policy"]["set"]["HOME"],
        "tmpdir":config["shell_environment_policy"]["set"]["TMPDIR"],
        "control_socket":control_socket.0,"control_token":control_token
    });
    let script = r#"
import json, os, pathlib, socket, sys
paths = json.loads(sys.argv[1])
results = {}
for label in ['private', 'original', 'original_git', 'auth', 'baseline', 'peer', 'readonly', 'denied', 'denied_alias', 'readonly_alias', 'control_token']:
    for access in ['read', 'write']:
        try:
            with open(paths[label], 'r' if access == 'read' else 'a') as stream:
                stream.read() if access == 'read' else stream.write('probe write\n')
            allowed = True
        except PermissionError:
            allowed = False
        expected = label == 'private' or (label in ['readonly', 'readonly_alias'] and access == 'read')
        results[label + '_' + access] = {'allowed': allowed, 'expected': expected}
for key, variable in [('home', 'HOME'), ('tmpdir', 'TMPDIR')]:
    value = os.environ.get(variable)
    results[variable] = {'allowed': value == paths[key], 'expected': True}
    if value == paths[key]:
        pathlib.Path(value, 'native-allowed.txt').write_text('private environment write\n')
try:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(2)
        connection.connect(paths['control_socket'])
    connected = True
except PermissionError:
    connected = False
# Reachability is recorded separately: authentication must remain effective even
# if platform socket rules change. The inaccessible control token is asserted above.
results['control_socket_connect'] = {'reachable': connected, 'requires_authentication': True}
print(json.dumps(results, sort_keys=True))
sys.exit(0 if all(value['allowed'] == value['expected'] for value in results.values() if 'expected' in value) else 1)
"#;
    let executed = tokio::time::timeout(
        Duration::from_secs(20),
        rpc.request(
            "command/exec",
            json!({
                "command":["/usr/bin/python3","-c",script,probes.to_string()],"cwd":project,
                "permissionProfile":profile,"timeoutMs":10000,"outputBytesCap":16384
            }),
        ),
    )
    .await;
    let development = if network_enabled {
        let network_check = if std::env::var("DELM_TEST_PUBLIC_NETWORK").as_deref() == Ok("1") {
            "public"
        } else {
            "local"
        };
        Some(
            tokio::time::timeout(
                Duration::from_secs(60),
                rpc.request(
                    "command/exec",
                    json!({
                        "command":["/usr/bin/python3","-c",DEVELOPMENT_PROBE,network_check],"cwd":project,
                        "permissionProfile":profile,"timeoutMs":50000,"outputBytesCap":16384
                    }),
                ),
            )
            .await,
        )
    } else {
        None
    };
    let browser = if let Some(browser) = browser {
        Some(
            rpc.request(
                "command/exec",
                json!({
                    "command":["/usr/bin/python3","-c",BROWSER_PROBE,browser],"cwd":project,
                    "permissionProfile":profile,"timeoutMs":20000,"outputBytesCap":8192
                }),
            )
            .await,
        )
    } else {
        None
    };
    let shutdown = rpc.shutdown(&[]).await;
    assert_eq!(
        tree(&original),
        before,
        "native sandbox allowed original or Git writes"
    );
    assert_eq!(
        tree(&peer),
        peer_before,
        "native sandbox allowed peer writes"
    );
    shutdown.unwrap();
    let executed = executed.unwrap().unwrap();
    assert_eq!(
        executed["exitCode"], 0,
        "native sandbox probe failed: {executed}"
    );
    let evidence: Value = serde_json::from_str(executed["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(
        evidence.as_object().unwrap().len(),
        25,
        "incomplete probe: {evidence}"
    );
    let development = development.map(|result| {
        let executed = result.unwrap().unwrap();
        assert_eq!(
            executed["exitCode"], 0,
            "native development probe failed: {executed}"
        );
        let evidence: Value = serde_json::from_str(executed["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(evidence["npm_import"], "private npm dependency");
        assert_eq!(evidence["python_import"], "private Python dependency");
        assert_eq!(evidence["npm_private_cache_written"], true);
        assert_eq!(evidence["downloaded_packages"], 2);
        if std::env::var("DELM_TEST_PUBLIC_NETWORK").as_deref() == Ok("1") {
            assert_eq!(evidence["public_registry_import"], "true");
        }
        evidence
    });
    let browser = browser.map(|result| {
        let executed = result.unwrap();
        assert_eq!(
            executed["exitCode"], 0,
            "private browser probe failed: {executed}"
        );
        serde_json::from_str::<Value>(executed["stdout"].as_str().unwrap()).unwrap()
    });
    json!({"host":host,"host_version":version,"worker":worker,"thread_response":response,"probes":evidence,
        "development":development,"browser":browser,"original_unchanged":true,"peer_unchanged":true,"model_turns":0})
}

const BROWSER_PROBE: &str = r##"
import json, os, pathlib, struct, subprocess, sys
project = pathlib.Path.cwd()
page = project / 'browser-probe.html'
page.write_text('<!doctype html><html><body style="margin:0;background:#123456"><canvas id="game" width="400" height="300"></canvas><script>const c=document.querySelector("canvas").getContext("2d"); c.fillStyle="#00ffff"; c.fillRect(20,30,100,70);</script></body></html>')
image = project / 'browser-probe.png'
profile = pathlib.Path(os.environ['HOME']) / 'browser-probe-profile'
# Single-process mode avoids macOS Mach registration denied by the native
# worker sandbox. The outer Codex filesystem policy remains in force.
result = subprocess.run([sys.argv[1], '--headless', '--no-sandbox', '--disable-gpu', '--single-process', '--no-zygote',
    '--no-first-run', '--no-default-browser-check', '--disable-background-networking',
    '--user-data-dir=' + str(profile), '--window-size=400,300',
    '--screenshot=' + str(image), page.as_uri()], capture_output=True, timeout=15)
assert result.returncode == 0, result.stderr.decode(errors='replace')
data = image.read_bytes()
assert data[:8] == b'\x89PNG\r\n\x1a\n'
assert struct.unpack('>II', data[16:24]) == (400, 300)
assert profile.is_dir()
print(json.dumps({'headless':True,'single_process':True,'private_profile':True,'screenshot_readable':True,
    'width':400,'height':300,'native_image_feature_enabled':True,
    'model_image_interpretation_tested':False}))
"##;

// Exercise actual dependency downloads, installs, cache writes, executable
// resolution, and local servers without depending on an external registry.
const DEVELOPMENT_PROBE: &str = r#"
import http.server, io, json, os, pathlib, shutil, subprocess, sys, tarfile, threading, zipfile

def command(args):
    result = subprocess.run(args, text=True, capture_output=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f'{args[0]} failed: {result.stdout}\n{result.stderr}')
    return result.stdout.strip()

home = pathlib.Path(os.environ['HOME'])
for variable in ['XDG_CACHE_HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
                 'NPM_CONFIG_CACHE', 'NPM_CONFIG_PREFIX', 'NPM_CONFIG_USERCONFIG',
                 'NPM_CONFIG_GLOBALCONFIG', 'PIP_CACHE_DIR', 'PYTHONUSERBASE', 'CARGO_HOME',
                 'PLAYWRIGHT_BROWSERS_PATH']:
    assert pathlib.Path(os.environ[variable]).is_relative_to(home), variable
assert os.environ['PIP_CONFIG_FILE'] == '/dev/null'
assert 'PIP_USER' not in os.environ

npm_archive = io.BytesIO()
with tarfile.open(fileobj=npm_archive, mode='w:gz') as archive:
    for name, contents in {
        'package/package.json': json.dumps({'name': 'delm-private-probe', 'version': '1.0.0', 'main': 'index.js'}),
        'package/index.js': "module.exports = 'private npm dependency';\n",
    }.items():
        contents = contents.encode()
        entry = tarfile.TarInfo(name)
        entry.size = len(contents)
        archive.addfile(entry, io.BytesIO(contents))

wheel = io.BytesIO()
with zipfile.ZipFile(wheel, 'w') as archive:
    for name, contents in {
        'delm_private_probe.py': "value = 'private Python dependency'\n",
        'delm_private_probe-1.0.dist-info/METADATA': 'Metadata-Version: 2.1\nName: delm-private-probe\nVersion: 1.0\n',
        'delm_private_probe-1.0.dist-info/WHEEL': 'Wheel-Version: 1.0\nGenerator: delm-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
        'delm_private_probe-1.0.dist-info/RECORD': '',
    }.items():
        archive.writestr(name, contents)

packages = {'/probe.tgz': npm_archive.getvalue(), '/delm_private_probe-1.0-py3-none-any.whl': wheel.getvalue()}
downloaded = set()
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = packages.get(self.path)
        if body is None:
            self.send_error(404)
            return
        downloaded.add(self.path)
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'public, max-age=3600')
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    origin = f'http://127.0.0.1:{server.server_port}'
    npm = shutil.which('npm')
    node = shutil.which('node')
    assert npm and node, 'Native development qualification requires installed Node and npm'
    command([npm, 'install', '--ignore-scripts', '--no-audit', '--no-fund', '--package-lock=false', origin + '/probe.tgz'])
    npm_import = command([node, '-e', "console.log(require('delm-private-probe'))"])
    assert command([npm, 'prefix', '-g']) == os.environ['NPM_CONFIG_PREFIX']
    assert command([npm, 'config', 'get', 'cache']) == os.environ['NPM_CONFIG_CACHE']
    venv = pathlib.Path.cwd() / '.probe-venv'
    command([sys.executable, '-m', 'venv', str(venv)])
    python = str(venv / 'bin/python')
    command([python, '-m', 'pip', 'install', '--no-index', '--disable-pip-version-check', origin + '/delm_private_probe-1.0-py3-none-any.whl'])
    python_import = command([python, '-c', 'import delm_private_probe; print(delm_private_probe.value)'])
    public_registry_import = None
    if sys.argv[1] == 'public':
        command([npm, 'install', '--ignore-scripts', '--no-audit', '--no-fund', '--package-lock=false',
                 '--registry=https://registry.npmjs.org', '--fetch-retries=0', '--fetch-timeout=15000', 'is-number@7.0.0'])
        public_registry_import = command([node, '-e', "console.log(require('is-number')(42))"])
    evidence = {
        'npm_import': npm_import,
        'python_import': python_import,
        'npm_private_cache_written': any(pathlib.Path(os.environ['NPM_CONFIG_CACHE']).rglob('*')),
        'downloaded_packages': len(downloaded),
        'local_server': True,
        'virtualenv': str(venv),
        'public_registry_import': public_registry_import,
    }
finally:
    server.shutdown()
    server.server_close()
    thread.join()
print(json.dumps(evidence, sort_keys=True))
"#;
