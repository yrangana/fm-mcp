//! The `fm serve` backend: runs `fm serve` as a child process on a Unix socket
//! and sends Chat Completions requests to it.
//!
//! Phase 1 keeps this minimal: lazy start, health check, one request at a time,
//! clean shutdown. Restart limits and the fake server for tests come in Phase 2.

use std::{
    fs::Permissions,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request,
    header::{CONTENT_TYPE, HOST},
};
use hyper_util::rt::TokioIo;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde::Deserialize;
use tempfile::TempDir;
use tokio::{
    net::UnixStream,
    process::{Child, Command},
    sync::Mutex,
    time::{Instant, sleep, timeout},
};
use tracing::{debug, info, warn};

use crate::backend::{Backend, BackendError, BoxFuture, ChatRequest, ChatResponse};

const DEFAULT_FM_PATH: &str = "/usr/bin/fm";
/// macOS limits Unix socket paths to 104 bytes. A longer path fails silently:
/// `fm serve` keeps running but never creates the socket.
const MAX_SOCKET_PATH: usize = 100;
const SOCKET_NAME: &str = "fm.sock";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_POLL: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct FmConfig {
    pub fm_path: PathBuf,
}

impl FmConfig {
    /// Reads `FM_MCP_FM_PATH` (used by tests to point at a fake), else `/usr/bin/fm`.
    pub fn from_env() -> Self {
        let fm_path = std::env::var_os("FM_MCP_FM_PATH")
            .map_or_else(|| PathBuf::from(DEFAULT_FM_PATH), PathBuf::from);
        Self { fm_path }
    }
}

/// Supervises one `fm serve` child. The mutex also serialises requests:
/// the model handles one request at a time anyway (AGENTS.md verified facts).
pub struct FmServe {
    config: FmConfig,
    state: Mutex<Option<Running>>,
}

/// Fields drop in order: the child is killed first, then the private socket
/// directory (and the socket in it) is removed.
struct Running {
    child: Child,
    socket: PathBuf,
    _socket_dir: TempDir,
}

impl FmServe {
    pub fn new(config: FmConfig) -> Self {
        Self {
            config,
            state: Mutex::new(None),
        }
    }

    /// Stops the child, if running: SIGTERM, then SIGKILL after a grace period.
    pub async fn shutdown(&self) {
        // Don't wait long for an in-flight request; dropping `Running` kills the child anyway.
        let Ok(mut state) = timeout(STOP_GRACE, self.state.lock()).await else {
            warn!("request still in flight at shutdown; killing fm serve");
            return;
        };
        if let Some(running) = state.take() {
            running.stop().await;
        }
    }

    async fn chat_inner(&self, request: ChatRequest) -> Result<ChatResponse, BackendError> {
        let mut state = self.state.lock().await;

        let needs_start = match state.as_mut() {
            None => true,
            Some(running) => running.has_exited(),
        };
        if needs_start {
            if state.is_some() {
                warn!("fm serve exited; restarting");
            }
            *state = Some(self.start().await?);
        }
        let Some(running) = state.as_ref() else {
            return Err(BackendError::Connection("fm serve is not running".into()));
        };

        let body = serde_json::to_vec(&request)
            .map_err(|e| BackendError::BadResponse(format!("could not encode request: {e}")))?;
        let (status, bytes) = timeout(
            REQUEST_TIMEOUT,
            http(&running.socket, Method::POST, "/v1/chat/completions", body),
        )
        .await
        .map_err(|_| BackendError::Timeout(REQUEST_TIMEOUT.as_secs()))??;

        if !status.is_success() {
            return Err(BackendError::from_http(status.as_u16(), &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|e| BackendError::BadResponse(e.to_string()))
    }

    async fn start(&self) -> Result<Running, BackendError> {
        let (socket_dir, socket) =
            private_socket_dir(std::env::var_os("TMPDIR").map(PathBuf::from).as_deref())?;

        let child = Command::new(&self.config.fm_path)
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            // stdout carries the MCP protocol; the child must never write to it.
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            // Own process group, so a terminal Ctrl-C reaches fm-mcp first.
            .process_group(0)
            .spawn()
            .map_err(|e| {
                BackendError::Unavailable(format!(
                    "could not start `{} serve`: {e}",
                    self.config.fm_path.display()
                ))
            })?;

        let pid = child.id().unwrap_or_default();
        info!("fm serve started pid={pid} socket={}", socket.display());

        let mut running = Running {
            child,
            socket,
            _socket_dir: socket_dir,
        };
        running.wait_until_ready().await?;
        Ok(running)
    }
}

impl Backend for FmServe {
    fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse, BackendError>> {
        Box::pin(self.chat_inner(request))
    }
}

impl Running {
    fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    async fn wait_until_ready(&mut self) -> Result<(), BackendError> {
        #[derive(Deserialize)]
        struct Health {
            #[serde(default)]
            models: Vec<HealthModel>,
        }
        #[derive(Deserialize)]
        struct HealthModel {
            name: String,
            available: bool,
        }

        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(BackendError::Unavailable(format!(
                    "fm serve exited ({status}) before it was ready"
                )));
            }
            match http(&self.socket, Method::GET, "/health", Vec::new()).await {
                Ok((status, body)) if status.is_success() => {
                    let health: Health = serde_json::from_slice(&body)
                        .map_err(|e| BackendError::BadResponse(format!("/health: {e}")))?;
                    return match health.models.iter().find(|m| m.name == "system") {
                        Some(model) if model.available => Ok(()),
                        _ => Err(BackendError::Unavailable(
                            "fm serve reports the system model is not available".into(),
                        )),
                    };
                }
                Ok((status, _)) => debug!("fm serve /health returned {status}"),
                Err(e) => debug!("fm serve not ready yet: {e}"),
            }
            if Instant::now() >= deadline {
                return Err(BackendError::Unavailable(format!(
                    "fm serve did not become ready within {} s",
                    STARTUP_TIMEOUT.as_secs()
                )));
            }
            sleep(HEALTH_POLL).await;
        }
    }

    async fn stop(mut self) {
        if let Some(pid) = self.child.id().and_then(|id| i32::try_from(id).ok()) {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
        if timeout(STOP_GRACE, self.child.wait()).await.is_err() {
            warn!("fm serve did not stop after SIGTERM; killing it");
            let _ = self.child.kill().await;
        }
        info!("fm serve stopped");
    }
}

/// Creates a fresh private directory for the socket: random name, mode 0700,
/// and creation fails if the name already exists.
///
/// The socket never sits directly in a shared directory such as `/tmp`, so
/// another local user can't pre-create the path and intercept requests.
/// Uses `$TMPDIR` (per-user on macOS) when the socket path fits, else `/tmp`.
fn private_socket_dir(tmpdir: Option<&Path>) -> Result<(TempDir, PathBuf), BackendError> {
    let mut last_error = String::from("no usable temporary directory");
    for base in tmpdir.into_iter().chain([Path::new("/tmp")]) {
        let dir = match tempfile::Builder::new()
            .prefix("fm-mcp-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(base)
        {
            Ok(dir) => dir,
            Err(e) => {
                last_error = format!("could not create a directory in {}: {e}", base.display());
                continue;
            }
        };
        let socket = dir.path().join(SOCKET_NAME);
        if socket.as_os_str().len() <= MAX_SOCKET_PATH {
            return Ok((dir, socket));
        }
        last_error = format!("socket path under {} is too long", base.display());
    }
    Err(BackendError::Unavailable(last_error))
}

/// One HTTP/1.1 request over a fresh Unix socket connection.
async fn http(
    socket: &Path,
    method: Method,
    path: &str,
    body: Vec<u8>,
) -> Result<(hyper::StatusCode, Bytes), BackendError> {
    let connection_error = |e: &dyn std::fmt::Display| BackendError::Connection(e.to_string());

    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| connection_error(&e))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| connection_error(&e))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            debug!("fm serve connection closed with error: {e}");
        }
    });

    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, "localhost")
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| connection_error(&e))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| connection_error(&e))?;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| connection_error(&e))?
        .to_bytes();
    Ok((status, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_dir_should_be_private_to_the_user() {
        let (dir, _) = private_socket_dir(Some(&std::env::temp_dir())).unwrap();
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn socket_dir_should_use_tmpdir_when_path_fits() {
        let base = std::env::temp_dir();
        let (dir, _) = private_socket_dir(Some(&base)).unwrap();
        assert!(dir.path().starts_with(&base));
    }

    #[test]
    fn socket_dir_should_fall_back_to_tmp_when_tmpdir_too_long() {
        let base = tempfile::tempdir().unwrap();
        let long = base.path().join("a".repeat(100));
        std::fs::create_dir(&long).unwrap();
        let (dir, _) = private_socket_dir(Some(&long)).unwrap();
        assert!(dir.path().starts_with("/tmp"));
    }

    #[test]
    fn socket_dir_should_fall_back_to_tmp_when_tmpdir_unset() {
        let (dir, _) = private_socket_dir(None).unwrap();
        assert!(dir.path().starts_with("/tmp"));
    }

    #[test]
    fn socket_path_should_fit_macos_limit() {
        let (_, socket) = private_socket_dir(Some(&std::env::temp_dir())).unwrap();
        assert!(socket.as_os_str().len() <= MAX_SOCKET_PATH);
    }

    #[test]
    fn socket_dir_should_be_removed_on_drop() {
        let (dir, _) = private_socket_dir(None).unwrap();
        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(!path.exists());
    }
}
