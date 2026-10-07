//! The `fm serve` backend: runs `fm serve` as a child process on a Unix socket
//! and sends Chat Completions requests to it.
//!
//! The child starts lazily on the first request, is health-checked, and is
//! replaced if it dies, stops answering, or times out. Requests run one at a time.

use std::{
    collections::VecDeque,
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

use crate::{
    backend::{Backend, BackendError, BoxFuture, ChatRequest, ChatResponse},
    orphans::{SOCKET_DIR_PREFIX, SOCKET_NAME},
};

const DEFAULT_FM_PATH: &str = "/usr/bin/fm";
/// macOS limits Unix socket paths to 104 bytes. A longer path fails silently:
/// `fm serve` keeps running but never creates the socket.
const MAX_SOCKET_PATH: usize = 100;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_POLL: Duration = Duration::from_millis(100);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_GRACE: Duration = Duration::from_secs(2);
const COUNT_TIMEOUT: Duration = Duration::from_secs(30);
/// Give up restarting after this many crashes within `CRASH_WINDOW`.
const MAX_CRASHES: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct FmConfig {
    pub fm_path: PathBuf,
    pub request_timeout: Duration,
}

impl FmConfig {
    /// Reads the environment:
    /// - `FM_MCP_FM_PATH`: the `fm` binary (tests point it at a fake); default `/usr/bin/fm`.
    /// - `FM_MCP_REQUEST_TIMEOUT_SECS`: per-request timeout; default 120.
    pub fn from_env() -> Self {
        let fm_path = std::env::var_os("FM_MCP_FM_PATH")
            .map_or_else(|| PathBuf::from(DEFAULT_FM_PATH), PathBuf::from);
        let request_timeout = parse_timeout(std::env::var("FM_MCP_REQUEST_TIMEOUT_SECS").ok());
        Self {
            fm_path,
            request_timeout,
        }
    }
}

fn parse_timeout(value: Option<String>) -> Duration {
    match value.as_deref().map(str::parse::<u64>) {
        Some(Ok(secs)) if secs > 0 => Duration::from_secs(secs),
        Some(_) => {
            warn!("ignoring invalid FM_MCP_REQUEST_TIMEOUT_SECS; using the default");
            DEFAULT_REQUEST_TIMEOUT
        }
        None => DEFAULT_REQUEST_TIMEOUT,
    }
}

/// Supervises one `fm serve` child. The mutex also serialises requests:
/// the model handles one request at a time anyway (AGENTS.md verified facts).
pub struct FmServe {
    config: FmConfig,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    running: Option<Running>,
    /// When recent children crashed (died or stopped answering), for the restart limit.
    crashes: VecDeque<Instant>,
}

impl State {
    /// Drops the current child (killing it) and records a crash.
    fn discard_crashed(&mut self) {
        self.running = None;
        self.crashes.push_back(Instant::now());
    }

    fn recent_crashes(&mut self) -> usize {
        while self
            .crashes
            .front()
            .is_some_and(|t| t.elapsed() > CRASH_WINDOW)
        {
            self.crashes.pop_front();
        }
        self.crashes.len()
    }
}

/// Fields drop in order: the child is killed first, then its watchdog, then the
/// private socket directory (and the socket in it) is removed.
struct Running {
    child: Child,
    /// Stops `child` if fm-mcp is force-killed (see `orphans`). `None` if it
    /// couldn't be started; fm-mcp still works without it.
    _watchdog: Option<Child>,
    socket: PathBuf,
    _socket_dir: TempDir,
}

impl FmServe {
    pub fn new(config: FmConfig) -> Self {
        Self {
            config,
            state: Mutex::new(State::default()),
        }
    }

    /// Stops the child, if running: SIGTERM, then SIGKILL after a grace period.
    pub async fn shutdown(&self) {
        // Don't wait long for an in-flight request; dropping `Running` kills the child anyway.
        let Ok(mut state) = timeout(STOP_GRACE, self.state.lock()).await else {
            warn!("request still in flight at shutdown; killing fm serve");
            return;
        };
        if let Some(running) = state.running.take() {
            running.stop().await;
        }
    }

    /// Sends one request. If the child has died or the connection fails, the
    /// child is replaced and the request retried once. Model calls have no side
    /// effects, so a retry is safe.
    async fn chat_inner(&self, request: ChatRequest) -> Result<ChatResponse, BackendError> {
        // A tool's own timeout can only shorten the configured one.
        let limit = request.timeout.map_or(self.config.request_timeout, |t| {
            t.min(self.config.request_timeout)
        });
        let body = serde_json::to_vec(&request)
            .map_err(|e| BackendError::BadResponse(format!("could not encode request: {e}")))?;
        let mut state = self.state.lock().await;

        for attempt in 1..=2 {
            let socket = self.ensure_running(&mut state).await?;
            let sent = timeout(
                limit,
                http(&socket, Method::POST, "/v1/chat/completions", body.clone()),
            )
            .await;

            let (status, bytes) = match sent {
                Ok(Ok(response)) => response,
                Ok(Err(BackendError::Connection(e))) => {
                    warn!("fm serve connection failed (attempt {attempt}): {e}");
                    state.discard_crashed();
                    if attempt == 1 {
                        continue;
                    }
                    return Err(BackendError::Connection(e));
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    // fm serve would keep working on the hung request and queue ours
                    // behind it, so replace it. A timeout is not counted as a crash.
                    warn!("fm serve timed out; replacing it");
                    state.running = None;
                    return Err(BackendError::Timeout(limit.as_secs()));
                }
            };

            if !status.is_success() {
                return Err(BackendError::from_http(status.as_u16(), &bytes));
            }
            let response: ChatResponse = serde_json::from_slice(&bytes)
                .map_err(|e| BackendError::BadResponse(e.to_string()))?;
            if let Some(usage) = response.usage {
                debug!(
                    "model used {} prompt + {} completion tokens",
                    usage.prompt_tokens, usage.completion_tokens
                );
            }
            return Ok(response);
        }
        Err(BackendError::Connection("fm serve is not running".into()))
    }

    /// Returns the socket of a healthy child, starting or replacing it if needed.
    async fn ensure_running(&self, state: &mut State) -> Result<PathBuf, BackendError> {
        if state.running.as_mut().is_some_and(Running::has_exited) {
            warn!("fm serve exited unexpectedly");
            state.discard_crashed();
        }
        if let Some(running) = &state.running {
            return Ok(running.socket.clone());
        }
        let crashes = state.recent_crashes();
        if crashes >= MAX_CRASHES {
            return Err(BackendError::CrashLoop(crashes));
        }
        if crashes > 0 {
            info!("restarting fm serve");
        }
        let running = self.start().await?;
        let socket = running.socket.clone();
        state.running = Some(running);
        Ok(socket)
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
                BackendError::StartFailed(format!(
                    "could not run `{} serve`: {e}",
                    self.config.fm_path.display()
                ))
            })?;

        let pid = child.id().unwrap_or_default();
        info!("fm serve started pid={pid} socket={}", socket.display());
        let watchdog = spawn_watchdog(pid, socket_dir.path());

        let mut running = Running {
            child,
            _watchdog: watchdog,
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

    fn count_tokens<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<usize, BackendError>> {
        Box::pin(count_tokens(&self.config.fm_path, text))
    }
}

/// `fm count-tokens -q` with `text` on stdin. Fast (about 0.08 s), and it runs
/// outside `fm serve`, so it doesn't wait behind model requests.
async fn count_tokens(fm_path: &Path, text: &str) -> Result<usize, BackendError> {
    use tokio::io::AsyncWriteExt;

    let failed = |e: &dyn std::fmt::Display| BackendError::CountFailed(e.to_string());
    let mut child = Command::new(fm_path)
        .args(["count-tokens", "-q"])
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        // Same error as `fm serve` failing to run: `fm` itself is missing or broken.
        .map_err(|e| {
            BackendError::StartFailed(format!(
                "could not run `{} count-tokens`: {e}",
                fm_path.display()
            ))
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| failed(&"no stdin"))?;
    let input = text.to_owned();
    // Write in the background so a full pipe can't deadlock with reading stdout.
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let output = timeout(COUNT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| failed(&"timed out"))?
        .map_err(|e| failed(&e))?;
    let _ = writer.await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(failed(&stderr.trim()));
    }
    stdout
        .trim()
        .parse()
        .map_err(|_| failed(&format!("unexpected output {stdout:?}")))
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
                return Err(BackendError::StartFailed(format!(
                    "fm serve exited ({status}) before it was ready"
                )));
            }
            match http(&self.socket, Method::GET, "/health", Vec::new()).await {
                Ok((status, body)) if status.is_success() => {
                    let health: Health = serde_json::from_slice(&body)
                        .map_err(|e| BackendError::BadResponse(format!("/health: {e}")))?;
                    return match health.models.iter().find(|m| m.name == "system") {
                        Some(model) if model.available => Ok(()),
                        _ => Err(BackendError::ModelUnavailable),
                    };
                }
                Ok((status, _)) => debug!("fm serve /health returned {status}"),
                Err(e) => debug!("fm serve not ready yet: {e}"),
            }
            if Instant::now() >= deadline {
                return Err(BackendError::StartFailed(format!(
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
        // The watchdog exits by itself once the child is gone; dropping
        // `self` kills it anyway (`kill_on_drop`).
    }
}

/// Starts `fm-mcp __watch` for the `fm serve` child `child_pid`.
fn spawn_watchdog(child_pid: u32, socket_dir: &Path) -> Option<Child> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            warn!("no watchdog for fm serve: cannot find own executable: {e}");
            return None;
        }
    };
    Command::new(exe)
        .arg("__watch")
        .arg("--parent")
        .arg(std::process::id().to_string())
        .arg("--child")
        .arg(child_pid.to_string())
        .arg("--socket-dir")
        .arg(socket_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .inspect_err(|e| warn!("no watchdog for fm serve: {e}"))
        .ok()
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
            .prefix(SOCKET_DIR_PREFIX)
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
    Err(BackendError::StartFailed(last_error))
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
