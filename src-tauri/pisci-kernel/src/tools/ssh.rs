use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::store::Settings;
use async_trait::async_trait;
use russh::client::{self, AuthResult, Handle};
use russh::keys::{decode_secret_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::info;

// ── SSH client handler ────────────────────────────────────────────────────────

struct ClientHandler;

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        // Accept any host key — user is responsible for verifying out-of-band.
        std::future::ready(Ok(true))
    }
}

// ── Connection record ─────────────────────────────────────────────────────────

struct SshConnection {
    handle: Handle<ClientHandler>,
    host: String,
    username: String,
}

// ── Tool ──────────────────────────────────────────────────────────────────────

pub struct SshTool {
    connections: Arc<Mutex<HashMap<String, SshConnection>>>,
    /// Optional Settings reference for looking up pre-configured servers
    settings: Option<Arc<Mutex<Settings>>>,
}

impl SshTool {
    pub fn new(settings: Option<Arc<Mutex<Settings>>>) -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            settings,
        }
    }
}

impl Default for SshTool {
    fn default() -> Self {
        Self::new(None)
    }
}

#[async_trait]
impl Tool for SshTool {
    fn name(&self) -> &str {
        "ssh"
    }

    fn description(&self) -> &str {
        "SSH client tool. Connect to remote servers, execute shell commands, and manage connections. \
         Actions: connect, exec, disconnect, list_connections. \
         For pre-configured servers (set up in Settings > SSH Servers), pass only connection_id to connect — \
         credentials are loaded automatically. For ad-hoc connections, provide host/username/password or private_key."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["connect", "exec", "disconnect", "list_connections"],
                    "description": "Action to perform"
                },
                "connection_id": {
                    "type": "string",
                    "description": "Identifier for the connection. For pre-configured servers, this is the server alias from Settings. For ad-hoc connections, any unique name; defaults to host."
                },
                "host": {
                    "type": "string",
                    "description": "Hostname or IP address. Required for ad-hoc connect; not needed for pre-configured servers."
                },
                "port": {
                    "type": "integer",
                    "description": "SSH port (default: 22). Not needed for pre-configured servers."
                },
                "username": {
                    "type": "string",
                    "description": "SSH username. Required for ad-hoc connect."
                },
                "password": {
                    "type": "string",
                    "description": "SSH password. Used for ad-hoc connect (mutually exclusive with private_key)."
                },
                "private_key": {
                    "type": "string",
                    "description": "PEM-encoded private key content. Used for ad-hoc connect."
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to execute. Required for exec."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Command execution timeout in seconds (default: 30)."
                }
            },
            "required": ["action"]
        })
    }

    fn needs_confirmation(&self, input: &Value) -> bool {
        matches!(input["action"].as_str(), Some("connect") | Some("exec"))
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        match input["action"].as_str() {
            Some("connect") => self.connect(&input).await,
            Some("exec") => self.exec(&input).await,
            Some("disconnect") => self.disconnect(&input).await,
            Some("list_connections") => self.list_connections().await,
            Some(other) => Ok(ToolResult::err(format!(
                "Unknown action '{}'. Valid: connect, exec, disconnect, list_connections",
                other
            ))),
            None => Ok(ToolResult::err("'action' is required")),
        }
    }
}

impl SshTool {
    async fn connect(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let conn_id_input = input["connection_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // Try to look up a pre-configured server by connection_id
        let preset =
            if let (Some(ref id), Some(ref settings_arc)) = (&conn_id_input, &self.settings) {
                let s = settings_arc.lock().await;
                s.ssh_servers.iter().find(|srv| srv.id == *id).cloned()
            } else {
                None
            };

        // Merge: preset values are defaults, input values override
        let host = input["host"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| preset.as_ref().map(|p| p.host.clone()))
            .unwrap_or_default();
        if host.is_empty() {
            // Show helpful message listing available servers
            let available = if let Some(ref settings_arc) = self.settings {
                let s = settings_arc.lock().await;
                if s.ssh_servers.is_empty() {
                    "No pre-configured servers. Go to Settings > SSH Servers to add one."
                        .to_string()
                } else {
                    let names: Vec<String> = s
                        .ssh_servers
                        .iter()
                        .map(|srv| {
                            format!("'{}' ({}@{}:{})", srv.id, srv.username, srv.host, srv.port)
                        })
                        .collect();
                    format!("Available pre-configured servers: {}", names.join(", "))
                }
            } else {
                String::new()
            };
            return Ok(ToolResult::err(format!(
                "'host' is required for ad-hoc connect, or provide a valid connection_id for a pre-configured server. {}",
                available
            )));
        }

        let username = input["username"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| preset.as_ref().map(|p| p.username.clone()))
            .unwrap_or_default();
        if username.is_empty() {
            return Ok(ToolResult::err("'username' is required for connect"));
        }

        let port = input["port"]
            .as_u64()
            .map(|p| p as u16)
            .or_else(|| preset.as_ref().map(|p| p.port))
            .unwrap_or(22);

        let conn_id = conn_id_input.unwrap_or_else(|| host.clone());

        // Credential resolution: input > preset
        let password_opt = input["password"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                preset
                    .as_ref()
                    .filter(|p| !p.password.is_empty())
                    .map(|p| p.password.clone())
            });
        let key_opt = input["private_key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                preset
                    .as_ref()
                    .filter(|p| !p.private_key.is_empty())
                    .map(|p| p.private_key.clone())
            });

        // Block loopback connections
        let blocked = ["localhost", "127.0.0.1", "::1", "0.0.0.0"];
        for b in blocked {
            if host == b {
                return Ok(ToolResult::err(format!(
                    "Blocked: cannot SSH to '{}' (loopback address)",
                    host
                )));
            }
        }

        let config = Arc::new(client::Config {
            inactivity_timeout: Some(Duration::from_secs(60)),
            ..<_>::default()
        });

        let addr = format!("{}:{}", host, port);
        info!("SSH connect: {} as {}", addr, username);

        let mut handle = match timeout(
            Duration::from_secs(15),
            client::connect(config, &addr, ClientHandler),
        )
        .await
        {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => return Ok(ToolResult::err(format!("Connection failed: {}", e))),
            Err(_) => return Ok(ToolResult::err(format!("Connection timed out to {}", addr))),
        };

        // Authenticate
        let auth_result: AuthResult = if let Some(password) = password_opt {
            match timeout(
                Duration::from_secs(10),
                handle.authenticate_password(username.clone(), password),
            )
            .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return Ok(ToolResult::err(format!("Authentication error: {}", e))),
                Err(_) => return Ok(ToolResult::err("Authentication timed out")),
            }
        } else if let Some(key_pem) = key_opt {
            let private_key = match decode_secret_key(&key_pem, None) {
                Ok(k) => Arc::new(k),
                Err(e) => {
                    return Ok(ToolResult::err(format!(
                        "Failed to parse private key: {}",
                        e
                    )))
                }
            };
            // best_supported_rsa_hash returns Result<Option<Option<HashAlg>>, _>
            let hash_alg: Option<russh::keys::HashAlg> = handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            let key_with_alg = PrivateKeyWithHashAlg::new(private_key, hash_alg);
            match timeout(
                Duration::from_secs(10),
                handle.authenticate_publickey(username.clone(), key_with_alg),
            )
            .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return Ok(ToolResult::err(format!("Authentication error: {}", e))),
                Err(_) => return Ok(ToolResult::err("Authentication timed out")),
            }
        } else {
            return Ok(ToolResult::err(
                "No credentials available. Provide 'password' or 'private_key', or configure this server in Settings > SSH Servers.",
            ));
        };

        if !auth_result.success() {
            return Ok(ToolResult::err(
                "Authentication failed: credentials rejected by server",
            ));
        }

        let mut pool = self.connections.lock().await;
        pool.insert(
            conn_id.clone(),
            SshConnection {
                handle,
                host: host.clone(),
                username: username.clone(),
            },
        );

        info!(
            "SSH connected: id={} host={} user={}",
            conn_id, host, username
        );
        Ok(ToolResult::ok(format!(
            "Connected to {}@{} (connection_id: '{}'). Use exec to run commands.",
            username, addr, conn_id
        )))
    }

    async fn exec(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let conn_id = match input["connection_id"].as_str().filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::err("'connection_id' is required for exec")),
        };
        let command = match input["command"].as_str().filter(|s| !s.is_empty()) {
            Some(c) => c.to_string(),
            None => return Ok(ToolResult::err("'command' is required for exec")),
        };
        let timeout_secs = input["timeout_secs"].as_u64().unwrap_or(30);

        let mut pool = self.connections.lock().await;
        let conn = match pool.get_mut(&conn_id) {
            Some(c) => c,
            None => {
                let active: Vec<String> = pool.keys().cloned().collect();
                return Ok(ToolResult::err(format!(
                    "No connection '{}'. Use connect first. Active: [{}]",
                    conn_id,
                    active.join(", ")
                )));
            }
        };

        info!("SSH exec [{}]: {}", conn_id, command);

        let channel = match conn.handle.channel_open_session().await {
            Ok(ch) => ch,
            Err(e) => return Ok(ToolResult::err(format!("Failed to open channel: {}", e))),
        };

        let mut channel = channel;
        if let Err(e) = channel.exec(true, command.as_str()).await {
            return Ok(ToolResult::err(format!("Failed to exec command: {}", e)));
        }

        let result = timeout(Duration::from_secs(timeout_secs), async {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut exit_code: Option<u32> = None;

            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Data { data }) => {
                        stdout.extend_from_slice(&data);
                        if stdout.len() > 200 * 1024 {
                            stdout.truncate(200 * 1024);
                            stdout.extend_from_slice(b"\n[output truncated at 200 KB]");
                            break;
                        }
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        stderr.extend_from_slice(&data);
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        exit_code = Some(exit_status);
                    }
                    Some(ChannelMsg::Eof) | None => break,
                    _ => {}
                }
            }
            (stdout, stderr, exit_code)
        })
        .await;

        match result {
            Err(_) => Ok(ToolResult::err(format!(
                "Command timed out after {}s: {}",
                timeout_secs, command
            ))),
            Ok((stdout, stderr, exit_code)) => {
                let stdout_str = String::from_utf8_lossy(&stdout).into_owned();
                let stderr_str = String::from_utf8_lossy(&stderr).into_owned();
                let code = exit_code.unwrap_or(0);

                let mut output = format!("exit_code: {}\n", code);
                if !stdout_str.is_empty() {
                    output.push_str(&format!("stdout:\n{}\n", stdout_str.trim_end()));
                }
                if !stderr_str.is_empty() {
                    output.push_str(&format!("stderr:\n{}\n", stderr_str.trim_end()));
                }
                if stdout_str.is_empty() && stderr_str.is_empty() {
                    output.push_str("(no output)");
                }

                if code != 0 {
                    Ok(ToolResult::err(output))
                } else {
                    Ok(ToolResult::ok(output))
                }
            }
        }
    }

    async fn disconnect(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let conn_id = match input["connection_id"].as_str().filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => {
                return Ok(ToolResult::err(
                    "'connection_id' is required for disconnect",
                ))
            }
        };

        let mut pool = self.connections.lock().await;
        match pool.remove(&conn_id) {
            Some(conn) => {
                let _ = conn
                    .handle
                    .disconnect(Disconnect::ByApplication, "bye", "en")
                    .await;
                info!("SSH disconnected: id={}", conn_id);
                Ok(ToolResult::ok(format!(
                    "Disconnected '{}' ({}@{})",
                    conn_id, conn.username, conn.host
                )))
            }
            None => {
                let active: Vec<String> = pool.keys().cloned().collect();
                Ok(ToolResult::err(format!(
                    "No active connection '{}'. Active: [{}]",
                    conn_id,
                    active.join(", ")
                )))
            }
        }
    }

    async fn list_connections(&self) -> anyhow::Result<ToolResult> {
        let pool = self.connections.lock().await;
        if pool.is_empty() {
            return Ok(ToolResult::ok("No active SSH connections."));
        }
        let lines: Vec<String> = pool
            .iter()
            .map(|(id, c)| format!("- '{}': {}@{}", id, c.username, c.host))
            .collect();
        Ok(ToolResult::ok(format!(
            "{} active connection(s):\n{}",
            pool.len(),
            lines.join("\n")
        )))
    }
}
