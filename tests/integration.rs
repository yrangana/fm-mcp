//! End-to-end tests: the real `fm-mcp` binary over stdio, against the fake `fm`.
//! Run with `cargo test --features fake-fm`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::time::Duration;

use common::{Server, socket_dir, wait_until_gone};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};

const EXIT_LIMIT: Duration = Duration::from_secs(5);

// --- Success path -----------------------------------------------------------

#[test]
fn summarise_should_return_the_model_text() {
    let mut server = Server::start(&[]);
    let result = server.summarise("Twelve chars");
    assert!(
        !result.is_error
            && result.text.starts_with("Fake summary of ")
            && result.text.ends_with(" characters."),
        "{}",
        result.text
    );
}

#[test]
fn every_request_should_send_stream_false() {
    // The fake streams unless told not to, which would make the call fail;
    // this also checks the request bodies directly.
    let mut server = Server::start(&[]);
    server.summarise("first");
    server.summarise("second");
    let requests: Vec<Value> = server
        .fake_log()
        .iter()
        .filter_map(|l| l.strip_prefix("request "))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        requests.len() == 2 && requests.iter().all(|r| r["stream"] == json!(false)),
        "{requests:?}"
    );
}

#[test]
fn server_should_start_fm_serve_only_on_first_tool_call() {
    let mut server = Server::start(&[]);
    server.request("tools/list", json!({}));
    assert_eq!(server.fake_starts(), 0);
}

#[test]
fn server_should_answer_tools_list_even_when_fm_is_missing() {
    let mut server = Server::start(&[("FM_MCP_FM_PATH", "/nonexistent/fm")]);
    let response = server.request("tools/list", json!({}));
    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(names.len(), 4, "{names:?}");
}

#[test]
fn slow_fm_serve_start_should_still_work() {
    let mut server = Server::start(&[("FAKE_FM_START_DELAY_MS", "500")]);
    assert!(!server.summarise("hello").is_error);
}

#[test]
fn long_tmpdir_should_fall_back_to_private_dir_in_tmp() {
    let base = tempfile::tempdir().unwrap();
    let long = base
        .path()
        .join("a".repeat(150 - base.path().as_os_str().len()));
    std::fs::create_dir(&long).unwrap();
    let mut server = Server::start(&[("TMPDIR", long.to_str().unwrap())]);

    let result = server.summarise("hello");
    let socket = server.current_fm_serve().socket;
    assert!(
        !result.is_error && socket.starts_with("/tmp") && socket.as_os_str().len() <= 100,
        "{socket:?}: {}",
        result.text
    );
}

// --- Error mapping: one test per row of the plan's error table ---------------

fn assert_tool_error(result: &common::ToolResult, expected: &str) {
    assert_eq!((result.is_error, result.text.as_str()), (true, expected));
}

#[test]
fn context_overflow_should_become_a_tool_error() {
    let mut server = Server::start(&[]);
    assert_tool_error(
        &server.summarise("FAKE_OVERFLOW"),
        "Input is too long for the on-device model (about 8K tokens including the reply). \
         Send a shorter excerpt, or split it and call this tool once per part.",
    );
}

#[test]
fn guardrail_should_become_a_tool_error() {
    let mut server = Server::start(&[]);
    assert_tool_error(
        &server.summarise("FAKE_GUARDRAIL"),
        "The on-device model's safety filter refused this input. This often happens with \
         harmless text. Do this task yourself instead of retrying.",
    );
}

#[test]
fn bad_request_should_become_a_tool_error_with_the_server_message() {
    let mut server = Server::start(&[]);
    assert_tool_error(
        &server.summarise("FAKE_BAD_REQUEST"),
        "fm-mcp sent a request the model server rejected (HTTP 400: Invalid JSON: The data \
         couldn't be read because it is missing.). This is a bug in fm-mcp; please report it.",
    );
}

#[test]
fn crash_on_every_request_should_become_a_tool_error_after_one_retry() {
    let mut server = Server::start(&[]);
    let result = server.summarise("FAKE_CRASH");
    assert_tool_error(
        &result,
        "The on-device model server stopped responding. Run `fm-mcp doctor`. \
         Do this task yourself for now.",
    );
    assert_eq!(server.fake_starts(), 2, "expected one retry");
}

#[test]
fn repeated_crashes_should_stop_restarts_with_a_tool_error() {
    let mut server = Server::start(&[]);
    server.summarise("FAKE_CRASH"); // crashes 1 and 2
    assert_tool_error(
        &server.summarise("FAKE_CRASH"), // crash 3, then the limit
        "The on-device model server keeps crashing (3 times in the last minute), so fm-mcp \
         has stopped restarting it for now. Run `fm-mcp doctor`. Do this task yourself.",
    );
}

#[test]
fn timeout_should_become_a_tool_error() {
    let mut server = Server::start(&[("FM_MCP_REQUEST_TIMEOUT_SECS", "1")]);
    assert_tool_error(
        &server.summarise("FAKE_HANG"),
        "The on-device model took longer than 1 s. Other sessions may be using it; \
         try a shorter input.",
    );
}

#[test]
fn call_after_timeout_should_use_a_fresh_fm_serve() {
    let mut server = Server::start(&[("FM_MCP_REQUEST_TIMEOUT_SECS", "1")]);
    server.summarise("FAKE_HANG");
    let result = server.summarise("hello");
    assert_eq!((result.is_error, server.fake_starts()), (false, 2));
}

#[test]
fn unavailable_model_should_become_a_tool_error() {
    let mut server = Server::start(&[("FAKE_FM_AVAILABLE", "false")]);
    assert_tool_error(
        &server.summarise("hello"),
        "Apple Intelligence is not available on this Mac (it may be turned off or still \
         downloading). Run `fm-mcp doctor` for details. Do this task yourself for now.",
    );
}

#[test]
fn missing_fm_binary_should_become_a_tool_error() {
    let mut server = Server::start(&[("FM_MCP_FM_PATH", "/nonexistent/fm")]);
    let result = server.summarise("hello");
    assert!(
        result.is_error
            && result.text.starts_with(
                "The on-device model server could not start: could not run `/nonexistent/fm"
            ),
        "{}",
        result.text
    );
}

#[test]
fn empty_text_should_become_a_tool_error_without_calling_the_model() {
    let mut server = Server::start(&[]);
    assert_tool_error(
        &server.summarise("   "),
        "`text` is empty; there is nothing to summarise.",
    );
    assert_eq!(server.fake_starts(), 0);
}

// --- Restarts ---------------------------------------------------------------

#[test]
fn killed_fm_serve_should_be_restarted_on_next_call() {
    let mut server = Server::start(&[]);
    server.summarise("hello");
    let first = server.current_fm_serve();
    kill(Pid::from_raw(first.pid), Signal::SIGKILL).unwrap();

    let result = server.summarise("hello again");
    assert_eq!((result.is_error, server.fake_starts()), (false, 2));
}

#[test]
fn crash_during_a_request_should_be_retried_once() {
    let mut server = Server::start(&[]);
    let result = server.summarise("FAKE_CRASH_ONCE");
    assert_eq!((result.is_error, server.fake_starts()), (false, 2));
}

// --- Clean shutdown -----------------------------------------------------------

fn assert_clean_exit(stop: impl FnOnce(&mut Server)) {
    let mut server = Server::start(&[]);
    server.summarise("hello");
    let child = server.current_fm_serve();
    let dir = socket_dir(&child).to_path_buf();

    stop(&mut server);
    let status = server.wait_for_exit(EXIT_LIMIT);

    assert!(status.success(), "exit status {status}");
    assert!(
        wait_until_gone(child.pid, EXIT_LIMIT),
        "fm serve pid {} still running",
        child.pid
    );
    assert!(!dir.exists(), "socket dir {dir:?} left behind");
}

#[test]
fn closing_stdin_should_stop_fm_serve_and_remove_the_socket() {
    assert_clean_exit(Server::close_stdin);
}

#[test]
fn sigterm_should_stop_fm_serve_and_remove_the_socket() {
    assert_clean_exit(|server| server.signal(Signal::SIGTERM));
}

#[test]
fn sigint_should_stop_fm_serve_and_remove_the_socket() {
    assert_clean_exit(|server| server.signal(Signal::SIGINT));
}

// --- Orphan protection (plan R13, D7) ----------------------------------------

#[test]
fn sigkill_of_fm_mcp_should_let_the_watchdog_stop_fm_serve() {
    let mut server = Server::start(&[]);
    server.summarise("hello");
    let child = server.current_fm_serve();
    let dir = socket_dir(&child).to_path_buf();

    server.signal(Signal::SIGKILL);
    server.wait_for_exit(EXIT_LIMIT);

    assert!(
        wait_until_gone(child.pid, EXIT_LIMIT),
        "fm serve pid {} outlived a force-killed fm-mcp",
        child.pid
    );
    assert!(
        wait_until(EXIT_LIMIT, || !dir.exists()),
        "socket dir {dir:?} left behind"
    );
}

#[test]
fn start_should_stop_fm_serve_orphaned_by_an_earlier_session() {
    // An orphan: a fake `fm serve` on an fm-mcp-style socket whose parent has
    // exited, so launchd owns it. `sh` backgrounds it and exits at once.
    // Under /tmp so the socket path stays short whatever the runner's TMPDIR is.
    let base = tempfile::Builder::new()
        .prefix("orphan-test-")
        .tempdir_in("/tmp")
        .unwrap();
    let dir = base.path().join("fm-mcp-orphantest");
    std::fs::create_dir(&dir).unwrap();
    let socket = dir.join("fm.sock");
    let log = base.path().join("orphan.log");
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#""$0" serve --socket "$1" >/dev/null 2>&1 & echo $!"#)
        .arg(common::fake_fm_path())
        .arg(&socket)
        .env("FAKE_FM_LOG", &log)
        .output()
        .unwrap();
    let pid: i32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap();
    let explain = || {
        format!(
            "orphan pid {pid}: alive={}, log={:?}, dir exists={}",
            common::process_alive(pid),
            std::fs::read_to_string(&log).unwrap_or_default(),
            dir.exists()
        )
    };
    // Proof the orphan really started. Don't wait for its socket: tests run in
    // parallel, and any test's fm-mcp may clean the orphan up first. That's fine.
    assert!(
        wait_until(EXIT_LIMIT, || log.exists()),
        "orphan never started; {}",
        explain()
    );

    let _server = Server::start(&[]);

    assert!(
        wait_until_gone(pid, EXIT_LIMIT),
        "orphan was not stopped; {}",
        explain()
    );
    assert!(
        wait_until(EXIT_LIMIT, || !dir.exists()),
        "orphan's socket dir left behind; {}",
        explain()
    );
}

fn wait_until(limit: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    condition()
}

// --- Phase 3 tools -------------------------------------------------------------

#[test]
fn long_summarise_should_split_combine_and_report_progress() {
    // The fake counts 4 characters per token: ~40K characters is ~10K tokens.
    let mut server = Server::start(&[]);
    let text = "The cache was cold after the deploy, so requests were slow. ".repeat(700);
    let result = server.call_tool_with_params(json!({
        "name": "summarise",
        "arguments": {"text": text},
        "_meta": {"progressToken": "p1"}
    }));
    let progress = server
        .notifications
        .iter()
        .filter(|n| n["method"] == json!("notifications/progress"))
        .count();
    assert!(
        !result.is_error && result.text.contains("summarised in 2 parts") && progress == 3,
        "progress={progress}: {}",
        result.text
    );
}

#[test]
fn summarise_over_the_cap_should_be_refused_quickly_without_the_model() {
    let mut server = Server::start(&[]);
    let text = "x".repeat(200_000); // 50K tokens at 4 chars per token
    let started = std::time::Instant::now();
    let result = server.summarise(&text);
    assert!(
        result.is_error
            && result
                .text
                .starts_with("Input is too long to summarise (50000 tokens")
            && server.fake_starts() == 0
            && started.elapsed() < Duration::from_secs(1),
        "{}",
        result.text
    );
}

#[test]
fn extract_should_return_json_matching_the_schema() {
    let mut server = Server::start(&[]);
    let result = server.call_tool(
        "extract",
        json!({"text": "Invoice INV-7 total $5", "schema": {"type": "object", "properties": {
            "invoice": {"type": "string"}, "status": {"enum": ["paid", "unpaid"]}}}}),
    );
    // The fake answers null for nullable fields and the first enum value otherwise.
    assert_eq!(
        (result.is_error, result.structured),
        (false, Some(json!({"invoice": null, "status": "paid"})))
    );
}

#[test]
fn extract_should_send_a_nullable_schema_with_unique_titles() {
    let mut server = Server::start(&[]);
    server.call_tool(
        "extract",
        json!({"text": "x", "schema": {"type": "object", "properties": {"total": {"type": "number"}}}}),
    );
    let request: Value = server
        .fake_log()
        .iter()
        .find_map(|l| l.strip_prefix("request "))
        .map(|l| serde_json::from_str(l).unwrap())
        .unwrap();
    assert_eq!(
        request["response_format"]["json_schema"]["schema"]["properties"]["total"],
        json!({"title": "Total", "anyOf": [{"type": "number"}, {"type": "null"}]})
    );
}

#[test]
fn extract_should_reject_an_unsupported_schema_without_the_model() {
    let mut server = Server::start(&[]);
    let result = server.call_tool(
        "extract",
        json!({"text": "x", "schema": {"type": "object", "properties": {"id": {"type": "string", "pattern": "^A"}}}}),
    );
    assert!(
        result.is_error
            && result
                .text
                .starts_with("Unusable schema: schema field `id`: `pattern` can't be used")
            && server.fake_starts() == 0,
        "{}",
        result.text
    );
}

#[test]
fn extract_should_refuse_input_over_one_call() {
    let mut server = Server::start(&[]);
    // 7,000 tokens at 4 chars per token: under the length pre-check, over the counted budget.
    let text = "word ".repeat(5600);
    let result = server.call_tool(
        "extract",
        json!({"text": text, "schema": {"type": "object", "properties": {"a": {"type": "string"}}}}),
    );
    assert!(
        result.is_error
            && result
                .text
                .starts_with("Input is too long for the on-device model (7000 tokens"),
        "{}",
        result.text
    );
}

#[test]
fn extract_runaway_should_become_a_stuck_error() {
    let mut server = Server::start(&[("FM_MCP_REQUEST_TIMEOUT_SECS", "1")]);
    let result = server.call_tool(
        "extract",
        json!({"text": "FAKE_HANG", "schema": {"type": "object", "properties": {"a": {"type": "string"}}}}),
    );
    assert!(
        result.is_error
            && result
                .text
                .starts_with("The on-device model got stuck and was stopped after 1 s."),
        "{}",
        result.text
    );
}

#[test]
fn classify_should_return_one_of_the_labels() {
    let mut server = Server::start(&[]);
    let result = server.call_tool(
        "classify",
        json!({"text": "The app crashes on launch", "labels": ["bug", "feature", "question"]}),
    );
    assert_eq!(result.structured, Some(json!({"label": "bug"})));
}

#[test]
fn classify_multi_should_return_a_list_of_labels() {
    let mut server = Server::start(&[]);
    let result = server.call_tool(
        "classify",
        json!({"text": "Crash and a feature idea", "labels": ["bug", "feature"], "multi": true}),
    );
    assert_eq!(result.structured, Some(json!({"labels": ["bug"]})));
}

#[test]
fn classify_should_reject_bad_labels_without_the_model() {
    let mut server = Server::start(&[]);
    let result = server.call_tool("classify", json!({"text": "x", "labels": ["only one"]}));
    assert_eq!(
        (result.is_error, result.text.as_str(), server.fake_starts()),
        (true, "Give between 2 and 50 labels (got 1).", 0)
    );
}

#[test]
fn ocr_should_read_an_image_through_fm_serve() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("shot.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfake").unwrap();
    let mut server = Server::start(&[]);
    let result = server.call_tool("ocr", json!({"path": image}));
    assert_eq!(
        (result.is_error, result.text.as_str()),
        (false, "Fake text read from an image.")
    );
}

#[test]
fn ocr_should_reject_a_missing_file_without_the_model() {
    let mut server = Server::start(&[]);
    let result = server.call_tool("ocr", json!({"path": "/nonexistent/shot.png"}));
    assert!(
        result.is_error
            && result
                .text
                .starts_with("Cannot read `/nonexistent/shot.png`")
            && server.fake_starts() == 0,
        "{}",
        result.text
    );
}

#[test]
fn ocr_should_refuse_pdfs_with_advice() {
    let mut server = Server::start(&[]);
    let result = server.call_tool("ocr", json!({"path": "/tmp/page.pdf"}));
    assert!(
        result.is_error && result.text.starts_with("PDFs are not supported"),
        "{}",
        result.text
    );
}
