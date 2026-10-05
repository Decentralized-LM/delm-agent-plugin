//! Native Claude plugin transport. The plugin observes native identities; the
//! MCP sidecar executes board calls only after Claude's own tool permission path.
mod controller;
mod view;

use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

const LIMIT: usize = 2 * 1024 * 1024;

#[derive(Subcommand)]
pub enum Command {
    /// Internal passive display observer. Never sends a host control request.
    #[command(hide = true)]
    View(view::Arguments),
    /// Internal native plugin bridge. Its startup request is supplied on stdin.
    #[command(hide = true)]
    Serve,
    /// Internal host control request; credentials are supplied on stdin.
    #[command(hide = true)]
    Request {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Official Claude plugin MCP sidecar; loading it starts no workers.
    #[command(hide = true)]
    Mcp,
    /// Recover a native session's stopped workers after a plugin reload.
    #[command(hide = true)]
    Recover {
        #[arg(long)]
        run_id: String,
    },
}

#[derive(Deserialize)]
struct Start {
    project: PathBuf,
    session_id: String,
    task: String,
    host_version: Option<String>,
    package_root: Option<PathBuf>,
}

async fn line(input: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let n = input
        .take((LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(n <= LIMIT, "Claude bridge message exceeds 2 MiB");
    Ok((n > 0).then_some(bytes))
}

async fn print(value: &Value) -> Result<()> {
    let mut output = tokio::io::stdout();
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

pub async fn execute(command: Command) -> Result<()> {
    match command {
        Command::View(arguments) => view::execute(arguments).await,
        Command::Serve => serve().await,
        Command::Mcp => mcp().await,
        Command::Recover { run_id } => {
            let bytes = line(&mut BufReader::new(tokio::io::stdin()))
                .await?
                .context("Missing recovery request")?;
            let request: Value = serde_json::from_slice(&bytes)?;
            print(&controller::recover(&run_id, &request)?).await
        }
        Command::Request { socket } => {
            let bytes = line(&mut BufReader::new(tokio::io::stdin()))
                .await?
                .context("Missing bridge request")?;
            let value: Value = serde_json::from_slice(&bytes)?;
            print(&request(&socket, &value).await?).await
        }
    }
}

fn socket_root() -> Result<PathBuf> {
    let root = Path::new("/tmp")
        .canonicalize()?
        .join(format!("delm-{}", unsafe { libc::getuid() }));
    match fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let meta = fs::symlink_metadata(&root)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::getuid() } && meta.mode() & 0o077 == 0,
        "Claude bridge storage must be a private owned directory"
    );
    Ok(root)
}

struct Endpoint {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| {
            m.file_type().is_socket() && m.dev() == self.device && m.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

async fn serve() -> Result<()> {
    let bytes = line(&mut BufReader::new(tokio::io::stdin()))
        .await?
        .context("Missing Claude launch request")?;
    let start: Start = serde_json::from_slice(&bytes)?;
    ensure!(start.project.is_absolute(), "Project path must be absolute");
    let native_host =
        crate::supervisor::ProcessIdentity::capture(unsafe { libc::getppid() } as u32)?;
    let executable = crate::package::retained_executable()?;
    let token = uuid::Uuid::new_v4().to_string();
    let socket = socket_root()?.join(format!("{}-claude.sock", uuid::Uuid::new_v4()));
    let listener = UnixListener::bind(&socket)?;
    let meta = fs::symlink_metadata(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let _endpoint = Endpoint {
        path: socket.clone(),
        device: meta.dev(),
        inode: meta.ino(),
    };
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut controller =
        controller::Controller::new(&start.project, start.session_id, start.task, native_host)?;
    if let Err(error) = controller.handle(
        json!({"op":"transport","token_digest":format!("{:x}",Sha256::digest(token.as_bytes())),"host_version":start.host_version,"package_root":start.package_root}),
    ) {
        controller.abort_before_handoff("Could not establish native control ownership")?;
        return Err(error);
    }
    let mut ready = controller.ready();
    ready["type"] = json!("ready");
    ready["socket"] = json!(socket);
    ready["token"] = json!(token);
    ready["executable"] = json!(executable);
    if let Err(error) = print(&ready).await {
        // The reader may have received the launch contract before its stream
        // closed. Preserve until native stop can be established by recovery.
        let _ = controller.handle(
            json!({"op":"cancel","reason":"Native launch output closed before acknowledgment"}),
        );
        return Err(error);
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(crate::config::DEFAULT_RUN_SECONDS);
    let mut deadline_sent = false;
    loop {
        tokio::select! {
            _ = termination.recv() => {
                let _ = controller.handle(json!({"op":"interrupt","reason":"Claude unloaded its DeLM bridge; native worker shutdown is unconfirmed"}));
                break;
            }
            _ = heartbeat.tick() => {
                if !native_host.is_running()? {
                    let _ = controller.handle(json!({"op":"interrupt","reason":"The native Claude host exited"}));
                    break;
                }
                if !deadline_sent && tokio::time::Instant::now() >= deadline {
                    deadline_sent = true;
                    // An execution deadline ends unfinished work. A selected
                    // result is already finishing, so retain its delivery intent
                    // while native shutdown is reconciled.
                    let result = controller.handle(json!({"op":"interrupt","reason":"DeLM's execution allowance expired"}))?;
                    let _ = print(&json!({"type":"action","result":result})).await;
                }
            }
            connection = listener.accept() => {
                let (mut stream, _) = connection?;
                // Local clients are short-lived, bounded requests. State transitions
                // stay serialized so claims, revisions and one-use tickets are atomic.
                let result = tokio::time::timeout(Duration::from_secs(3), async {
                    let bytes = line(&mut BufReader::new(&mut stream)).await?.context("Missing control message")?;
                    let mut req:Value = serde_json::from_slice(&bytes)?;
                    let consume = req["op"] == "consume";
                    ensure!(consume || req["token"].as_str() == Some(&token), "Unbound Claude control request");
                    if let Some(object) = req.as_object_mut() { object.remove("token"); }
                    controller.handle(req)
                }).await;
                let reply = match result {
                    Ok(Ok(value)) => json!({"ok":true,"result":value}),
                    Ok(Err(error)) => json!({"ok":false,"error":format!("{error:#}")}),
                    Err(_) => json!({"ok":false,"error":"Bridge request timed out"}),
                };
                let mut bytes = serde_json::to_vec(&reply)?;
                bytes.push(b'\n');
                let _ = tokio::time::timeout(Duration::from_secs(3), stream.write_all(&bytes)).await;
                if reply["result"]["actions"].as_array().is_some_and(|actions| !actions.is_empty()) {
                    let _ = print(&json!({"type":"action","result":reply["result"]})).await;
                }
                if controller.finished() { break; }
            }
        }
    }
    Ok(())
}

async fn request(socket: &Path, request: &Value) -> Result<Value> {
    let root = socket_root()?;
    ensure!(
        socket.parent() == Some(root.as_path()),
        "Unrecognized Claude bridge endpoint"
    );
    let meta = fs::symlink_metadata(socket).context("DeLM is no longer running")?;
    ensure!(
        meta.file_type().is_socket()
            && meta.uid() == unsafe { libc::getuid() }
            && meta.mode() & 0o077 == 0,
        "Untrusted Claude bridge endpoint"
    );
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut stream = UnixStream::connect(socket).await?;
        let mut bytes = serde_json::to_vec(request)?;
        ensure!(bytes.len() < LIMIT, "Claude request is too large");
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        let bytes = line(&mut BufReader::new(stream))
            .await?
            .context("Claude bridge closed without a result")?;
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await
    .context("Claude bridge did not respond")?
}

fn definitions() -> Vec<Value> {
    crate::board::tool_definitions().into_iter().chain(crate::services::tool_definitions()).map(|mut tool| {
        tool.as_object_mut().unwrap().remove("type");
        tool.as_object_mut().unwrap().remove("deferLoading");
        tool["inputSchema"]["properties"]["_delm"] = json!({"type":"object","description":"Reserved native transport binding. Do not supply this field.",
            "properties":{"socket":{"type":"string"},"ticket":{"type":"string"}},"required":["socket","ticket"],"additionalProperties":false});
        tool
    }).collect()
}

async fn mcp() -> Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    while let Some(bytes) = line(&mut input).await? {
        let message: Value = serde_json::from_slice(&bytes)?;
        let Some(id) = message.get("id") else {
            continue;
        };
        let result = match message["method"].as_str() {
            Some("initialize") => {
                json!({"protocolVersion":protocol_version(&message["params"]["protocolVersion"]),
                "capabilities":{"tools":{}},"serverInfo":{"name":"delm","version":env!("CARGO_PKG_VERSION")}})
            }
            Some("ping") => json!({}),
            Some("tools/list") => json!({"tools":definitions()}),
            Some("tools/call") => {
                let result = mcp_call(&message["params"]).await;
                match result {
                    Ok(value) => {
                        json!({"content":[{"type":"text","text":serde_json::to_string(&value)?}],"isError":value.get("error").is_some()})
                    }
                    Err(error) => {
                        json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true})
                    }
                }
            }
            _ => {
                print(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})).await?;
                continue;
            }
        };
        print(&json!({"jsonrpc":"2.0","id":id,"result":result})).await?;
    }
    Ok(())
}

fn protocol_version(requested: &Value) -> &str {
    match requested.as_str() {
        Some(version @ ("2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25")) => version,
        _ => "2025-11-25",
    }
}

async fn mcp_call(params: &Value) -> Result<Value> {
    let name = params["name"].as_str().context("Missing tool name")?;
    ensure!(
        definitions().iter().any(|t| t["name"] == name),
        "Unknown DeLM tool"
    );
    let mut arguments = params["arguments"].clone();
    let binding = arguments
        .as_object_mut()
        .context("Tool arguments must be an object")?
        .remove("_delm")
        .context("Use /delm:run in Claude Code; this tool has no native worker binding")?;
    let socket = Path::new(
        binding["socket"]
            .as_str()
            .context("Missing native endpoint")?,
    );
    let ticket = binding["ticket"]
        .as_str()
        .context("Missing native invocation ticket")?;
    let response = request(
        socket,
        &json!({"op":"consume","ticket":ticket,"tool":name,"arguments":arguments}),
    )
    .await?;
    ensure!(
        response["ok"] == true,
        "{}",
        response["error"]
            .as_str()
            .unwrap_or("DeLM refused the native tool call")
    );
    Ok(response["result"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mcp_schema_is_shared_and_binding_is_not_model_authority() {
        let tools = definitions();
        assert!(tools.iter().any(|tool| tool["name"] == "delm_complete"));
        assert!(tools.iter().any(|tool| tool["name"] == "delm_service"));
        assert!(tools.iter().all(|tool| tool.get("deferLoading").is_none()
            && tool["inputSchema"]["properties"]["_delm"]["additionalProperties"] == false));
    }
    #[test]
    fn protocol_negotiation_does_not_claim_unknown_future_versions() {
        assert_eq!(protocol_version(&json!("2025-03-26")), "2025-03-26");
        assert_eq!(protocol_version(&json!("future-version")), "2025-11-25");
    }
    #[tokio::test]
    async fn direct_unbound_mcp_call_cannot_reach_the_board() {
        assert!(
            mcp_call(&json!({"name":"delm_status","arguments":{}}))
                .await
                .is_err()
        );
        assert!(
            mcp_call(&json!({"name":"arbitrary","arguments":{}}))
                .await
                .is_err()
        );
    }
}
