//! Test harness: runs the real `fm-mcp` binary over stdio against the fake `fm`.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The fake `fm` binary. Without the feature, Cargo may still point at a stale
/// build from an earlier run, so require the feature explicitly.
pub fn fake_fm_path() -> &'static str {
    match option_env!("CARGO_BIN_EXE_fake-fm") {
        Some(path) if cfg!(feature = "fake-fm") => path,
        _ => panic!("integration tests need the fake fm: run `cargo test --features fake-fm`"),
    }
}

/// One running `fm-mcp` process, already through the MCP handshake.
pub struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
    dir: TempDir,
}

/// The `fm serve` child that fm-mcp reported starting.
#[derive(Debug, Clone)]
pub struct FmServeChild {
    pub pid: i32,
    pub socket: PathBuf,
}

pub struct ToolResult {
    pub is_error: bool,
    pub text: String,
}

impl Server {
    /// Starts fm-mcp with the fake `fm` and the given extra environment.
    pub fn start(env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_fm-mcp"));
        command
            .env("FM_MCP_FM_PATH", fake_fm_path())
            .env("FAKE_FM_LOG", dir.path().join("fake.log"))
            .env("FM_MCP_LOG", "info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn fm-mcp");

        let stderr = Arc::new(Mutex::new(String::new()));
        let mut pipe = child.stderr.take().unwrap();
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buffer[..n]));
            }
        });

        let mut server = Self {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            stderr,
            next_id: 1,
            dir,
        };
        server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "fm-mcp-tests", "version": "0"}
            }),
        );
        server.notify("notifications/initialized");
        server
    }

    pub fn pid(&self) -> i32 {
        i32::try_from(self.child.id()).unwrap()
    }

    /// Sends a request and waits for its response.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "fm-mcp closed stdout; stderr:\n{}", self.stderr());
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    pub fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    pub fn summarise(&mut self, text: &str) -> ToolResult {
        self.call_tool("summarise", json!({"text": text}))
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> ToolResult {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &response["result"];
        assert!(!result.is_null(), "protocol error: {response}");
        ToolResult {
            is_error: result["isError"].as_bool().unwrap_or(false),
            text: result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        }
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Every `fm serve` child fm-mcp has reported starting, oldest first.
    pub fn fm_serve_children(&self) -> Vec<FmServeChild> {
        self.stderr()
            .lines()
            .filter_map(|line| {
                let rest = line.split("fm serve started pid=").nth(1)?;
                let (pid, socket) = rest.split_once(" socket=")?;
                Some(FmServeChild {
                    pid: pid.parse().ok()?,
                    socket: PathBuf::from(socket.trim()),
                })
            })
            .collect()
    }

    pub fn current_fm_serve(&self) -> FmServeChild {
        self.fm_serve_children()
            .pop()
            .unwrap_or_else(|| panic!("no fm serve started; stderr:\n{}", self.stderr()))
    }

    /// Lines the fake `fm` wrote to its log.
    pub fn fake_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("fake.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn fake_starts(&self) -> usize {
        self.fake_log()
            .iter()
            .filter(|l| l.starts_with("start "))
            .count()
    }

    /// Closes stdin, as an MCP client does when it quits.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    pub fn signal(&self, signal: Signal) {
        kill(Pid::from_raw(self.pid()), signal).unwrap();
    }

    /// Waits for fm-mcp to exit; panics after `limit`.
    pub fn wait_for_exit(&mut self, limit: Duration) -> ExitStatus {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "fm-mcp did not exit within {limit:?}; stderr:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    /// Shuts down like a real client (close stdin), so fm-mcp stops its `fm serve`.
    /// Only if that fails, force-kill fm-mcp and every child it reported, so no
    /// test leaves processes behind (a SIGKILLed fm-mcp can't clean up).
    fn drop(&mut self) {
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        for child in self.fm_serve_children() {
            let _ = kill(Pid::from_raw(child.pid), Signal::SIGKILL);
        }
    }
}

pub fn process_alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok()
}

/// Waits until `pid` has gone; returns false if it is still alive after `limit`.
pub fn wait_until_gone(pid: i32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    !process_alive(pid)
}

pub fn socket_dir(child: &FmServeChild) -> &Path {
    child.socket.parent().unwrap()
}
