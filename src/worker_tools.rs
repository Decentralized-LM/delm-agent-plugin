//! A small MCP transport for coordination tools on native forked Codex threads.
//! The runtime binds each connection to one worker; arguments cannot select it.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const LIMIT: usize = 2 * 1024 * 1024;

pub struct Call {
    pub worker: usize,
    pub tool: String,
    pub arguments: Value,
    pub call_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub reply: oneshot::Sender<Value>,
}

#[derive(Serialize, Deserialize)]
struct WireCall {
    token: String,
    tool: String,
    arguments: Value,
    call_id: Option<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
}

pub struct Gateway {
    pub calls: mpsc::Receiver<Call>,
    paths: Vec<PathBuf>,
    tokens: Vec<String>,
    listeners: Vec<JoinHandle<()>>,
    bound: Vec<(PathBuf, u64, u64)>,
}

impl Gateway {
    pub fn start(run_id: &str) -> Result<Self> {
        Self::for_worker_count(run_id, crate::config::DEFAULT_WORKER_COUNT)
    }

    pub fn for_worker_count(run_id: &str, count: usize) -> Result<Self> {
        crate::config::validate_worker_count(count)?;
        let id = uuid::Uuid::parse_str(run_id)?;
        let root = Path::new("/tmp")
            .canonicalize()?
            .join(format!("delm-{}", unsafe { libc::getuid() }));
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let meta = fs::symlink_metadata(&root)?;
        ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::getuid() } && meta.mode() & 0o077 == 0,
            "Worker tool storage is not private"
        );
        let paths = (1..=count)
            .map(|worker| root.join(format!("{id}-{worker}.mcp")))
            .collect();
        let tokens = (0..count)
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        let (sender, calls) = mpsc::channel(32);
        let mut gateway = Self {
            calls,
            paths,
            tokens,
            listeners: Vec::new(),
            bound: Vec::new(),
        };
        for index in 0..count {
            let listener = UnixListener::bind(&gateway.paths[index])?;
            let identity = fs::symlink_metadata(&gateway.paths[index])?;
            ensure!(
                identity.file_type().is_socket(),
                "Bound tool endpoint is not a socket"
            );
            gateway
                .bound
                .push((gateway.paths[index].clone(), identity.dev(), identity.ino()));
            fs::set_permissions(&gateway.paths[index], fs::Permissions::from_mode(0o600))?;
            let token = gateway.tokens[index].clone();
            let sender = sender.clone();
            gateway.listeners.push(tokio::spawn(async move {
                let slots = Arc::new(tokio::sync::Semaphore::new(8));
                let mut clients = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        connection = listener.accept() => {
                            let Ok((stream, _)) = connection else { break; };
                            let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                            let token = token.clone(); let sender = sender.clone();
                            clients.spawn(async move {
                                let _permit = permit;
                                let _ = receive(stream, index, &token, sender).await;
                            });
                        },
                        _ = clients.join_next(), if !clients.is_empty() => {},
                    }
                }
            }));
        }
        Ok(gateway)
    }

    pub fn config(&self, index: usize) -> Result<Value> {
        ensure!(index < self.paths.len(), "Unknown worker");
        // Invoking DeLM authorizes its private board protocol. This grant is
        // limited to our exact tools on this authenticated, per-run endpoint;
        // native execution and the user's other MCP servers keep their policy.
        let tools: serde_json::Map<String, Value> = crate::board::tool_definitions()
            .into_iter()
            .chain(crate::services::tool_definitions())
            .map(|tool| {
                (
                    tool["name"].as_str().unwrap().to_owned(),
                    json!({"approval_mode":"approve"}),
                )
            })
            .collect();
        Ok(
            json!({"command":std::env::current_exe()?,"args":["worker-mcp","--socket",self.paths[index]],
            "env":{"DELM_COORDINATION_TOKEN":self.tokens[index]},"enabled":true,"required":true,"tool_timeout_sec":120,"tools":tools}),
        )
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        for task in &self.listeners {
            task.abort();
        }
        for (path, device, inode) in &self.bound {
            if fs::symlink_metadata(path).is_ok_and(|meta| {
                meta.file_type().is_socket() && meta.dev() == *device && meta.ino() == *inode
            }) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

async fn line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let count = reader
        .take((LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await?;
    ensure!(count <= LIMIT, "Worker tool message exceeds 2 MiB");
    Ok((count > 0).then_some(bytes))
}

async fn receive(
    stream: UnixStream,
    worker: usize,
    token: &str,
    sender: mpsc::Sender<Call>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let bytes = tokio::time::timeout(Duration::from_secs(10), line(&mut BufReader::new(read)))
        .await??
        .context("Missing worker request")?;
    let request: WireCall = serde_json::from_slice(&bytes)?;
    ensure!(request.token == token, "Unbound worker request");
    let (reply, receiver) = oneshot::channel();
    sender
        .send(Call {
            worker,
            tool: request.tool,
            arguments: request.arguments,
            call_id: request.call_id,
            thread_id: request.thread_id,
            turn_id: request.turn_id,
            reply,
        })
        .await?;
    let response = receiver.await.context("DeLM run has ended")?;
    let mut encoded = serde_json::to_vec(&response)?;
    encoded.push(b'\n');
    write.write_all(&encoded).await?;
    Ok(())
}

fn local_response(request: &Value) -> Option<Value> {
    match request["method"].as_str()? {
        "initialize" => {
            let requested = request["params"]["protocolVersion"].as_str().unwrap_or("");
            let version =
                if ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"].contains(&requested) {
                    requested
                } else {
                    "2025-11-25"
                };
            Some(
                json!({"protocolVersion":version,"capabilities":{"tools":{}},
                "serverInfo":{"name":"delm-coordination","version":env!("CARGO_PKG_VERSION")}}),
            )
        }
        "ping" => Some(json!({})),
        "tools/list" => {
            let mut tools = crate::board::tool_definitions();
            tools.extend(crate::services::tool_definitions());
            for tool in &mut tools {
                if let Some(tool) = tool.as_object_mut() {
                    tool.remove("type");
                    tool.remove("deferLoading");
                }
            }
            Some(json!({"tools":tools}))
        }
        _ => None,
    }
}

/// Native model tool calls carry these IDs outside the model's tool arguments.
/// A direct administrative MCP call can omit callId; the runtime then refuses
/// coordination mutations because it has no matching observed native turn.
fn native_id(request: &Value, key: &str) -> Result<Option<String>> {
    let Some(value) = request["params"]["_meta"].get(key) else {
        return Ok(None);
    };
    let id = value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .with_context(|| format!("Invalid native MCP {key}"))?;
    Ok(Some(id.into()))
}

fn native_turn_id(request: &Value) -> Result<Option<String>> {
    let Some(value) = request["params"]["_meta"]["x-codex-turn-metadata"].get("turn_id") else {
        return Ok(None);
    };
    let id = value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .context("Invalid native MCP turn_id")?;
    Ok(Some(id.into()))
}

/// No stdout logging: every line belongs to the MCP protocol.
pub async fn serve_stdio(socket: &Path) -> Result<()> {
    let token = std::env::var("DELM_COORDINATION_TOKEN").context("Missing worker binding")?;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    while let Some(bytes) = line(&mut input).await? {
        let request: Value = serde_json::from_slice(&bytes)?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let response = if let Some(result) = local_response(&request) {
            json!({"jsonrpc":"2.0","id":id,"result":result})
        } else if request["method"] == "tools/call" {
            let reply = async {
                let mut stream = UnixStream::connect(socket)
                    .await
                    .context("The DeLM run is no longer available")?;
                let wire = WireCall {
                    token: token.clone(),
                    tool: request["params"]["name"]
                        .as_str()
                        .context("Missing tool name")?
                        .into(),
                    arguments: request["params"]["arguments"].clone(),
                    call_id: native_id(&request, "callId")?,
                    thread_id: native_id(&request, "threadId")?,
                    turn_id: native_turn_id(&request)?,
                };
                let mut bytes = serde_json::to_vec(&wire)?;
                bytes.push(b'\n');
                stream.write_all(&bytes).await?;
                let bytes = line(&mut BufReader::new(stream))
                    .await?
                    .context("DeLM closed the tool request")?;
                Ok::<Value, anyhow::Error>(serde_json::from_slice(&bytes)?)
            }
            .await;
            let result = match reply {
                Ok(value) => value,
                Err(e) => json!({"isError":true,"content":[{"type":"text","text":e.to_string()}]}),
            };
            json!({"jsonrpc":"2.0","id":id,"result":result})
        } else {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})
        };
        let mut encoded = serde_json::to_vec(&response)?;
        encoded.push(b'\n');
        output.write_all(&encoded).await?;
        output.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_metadata_is_separate_from_untrusted_tool_arguments() {
        let request = json!({"params":{"arguments":{"callId":"forged","threadId":"forged"},"_meta":{"callId":"native-call","threadId":"native-thread"}}});
        assert_eq!(
            native_id(&request, "callId").unwrap().as_deref(),
            Some("native-call")
        );
        assert_eq!(
            native_id(&request, "threadId").unwrap().as_deref(),
            Some("native-thread")
        );
        assert!(
            native_id(
                &json!({"params":{"arguments":{"callId":"forged"}}}),
                "callId"
            )
            .unwrap()
            .is_none()
        );
        assert!(native_id(&json!({"params":{"_meta":{"callId":[]}}}), "callId").is_err());
    }

    #[test]
    fn native_turn_identity_cannot_come_from_tool_arguments() {
        let request = json!({"params":{"arguments":{"turn_id":"forged"},"_meta":{"x-codex-turn-metadata":{"turn_id":"native-turn"}}}});
        assert_eq!(
            native_turn_id(&request).unwrap().as_deref(),
            Some("native-turn")
        );
        assert!(
            native_turn_id(&json!({"params":{"arguments":{"turn_id":"forged"}}}))
                .unwrap()
                .is_none()
        );
        assert!(
            native_turn_id(&json!({"params":{"_meta":{"x-codex-turn-metadata":{"turn_id":12}}}}))
                .is_err()
        );
    }

    #[test]
    fn mcp_lists_tools_and_preserves_protocol_envelope() {
        let result = local_response(&json!({"method":"tools/list"})).unwrap();
        assert!(
            result["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "delm_status")
        );
        assert!(local_response(&json!({"method":"unknown"})).is_none());
        assert!(
            result["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t.get("type").is_none() && t.get("deferLoading").is_none())
        );
        for version in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
            assert_eq!(
                local_response(
                    &json!({"method":"initialize","params":{"protocolVersion":version}})
                )
                .unwrap()["protocolVersion"],
                version
            );
        }
    }
    #[tokio::test]
    async fn coordination_grants_are_limited_to_the_owned_tools_and_endpoint() {
        let gateway = Gateway::for_worker_count(&uuid::Uuid::new_v4().to_string(), 4).unwrap();
        let listed = local_response(&json!({"method":"tools/list"})).unwrap();
        for index in 0..4 {
            let config = gateway.config(index).unwrap();
            assert!(config.get("default_tools_approval_mode").is_none());
            assert!(config.get("approval_policy").is_none());
            assert_eq!(
                config["tools"].as_object().unwrap().len(),
                listed["tools"].as_array().unwrap().len()
            );
            for tool in listed["tools"].as_array().unwrap() {
                assert_eq!(
                    config["tools"][tool["name"].as_str().unwrap()]["approval_mode"],
                    "approve"
                );
            }
            assert_eq!(config["args"][2], json!(gateway.paths[index]));
            assert_eq!(
                config["env"]["DELM_COORDINATION_TOKEN"],
                gateway.tokens[index]
            );
        }
        assert!(gateway.config(4).is_err());
    }

    #[tokio::test]
    async fn gateway_binds_requests_and_returns_runtime_response() {
        let mut gateway = Gateway::start(&uuid::Uuid::new_v4().to_string()).unwrap();
        let path = gateway.paths[1].clone();
        let token = gateway.tokens[1].clone();
        let client = tokio::spawn(async move {
            let mut stream = UnixStream::connect(path).await.unwrap();
            let mut bytes = serde_json::to_vec(&WireCall {
                token,
                tool: "delm_read".into(),
                arguments: json!({}),
                call_id: Some("native-call-1".into()),
                thread_id: Some("native-thread-2".into()),
                turn_id: Some("native-turn-3".into()),
            })
            .unwrap();
            bytes.push(b'\n');
            stream.write_all(&bytes).await.unwrap();
            line(&mut BufReader::new(stream)).await.unwrap().unwrap()
        });
        let call = gateway.calls.recv().await.unwrap();
        assert_eq!(call.worker, 1);
        assert_eq!(call.call_id.as_deref(), Some("native-call-1"));
        assert_eq!(call.thread_id.as_deref(), Some("native-thread-2"));
        assert_eq!(call.turn_id.as_deref(), Some("native-turn-3"));
        call.reply.send(json!({"content":[]})).unwrap();
        let response: Value = serde_json::from_slice(&client.await.unwrap()).unwrap();
        assert!(response["content"].is_array());
    }
    #[tokio::test]
    async fn duplicate_gateway_cannot_remove_live_owner_sockets() {
        let id = uuid::Uuid::new_v4().to_string();
        let gateway = Gateway::start(&id).unwrap();
        assert!(Gateway::start(&id).is_err());
        assert!(gateway.paths.iter().all(|path| path.exists()));
        let paths = gateway.paths.clone();
        drop(gateway);
        assert!(paths.iter().all(|path| !path.exists()));
    }
    #[tokio::test]
    async fn gateway_does_not_remove_replacement_endpoint() {
        let gateway = Gateway::start(&uuid::Uuid::new_v4().to_string()).unwrap();
        let path = gateway.paths[0].clone();
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"replacement data").unwrap();
        drop(gateway);
        assert_eq!(fs::read(&path).unwrap(), b"replacement data");
        fs::remove_file(path).unwrap();
    }
}
