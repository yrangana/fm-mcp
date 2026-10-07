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
    assert_eq!(response["result"]["tools"][0]["name"], json!("summarise"));
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
                "The on-device model server could not start: could not run `/nonexistent/fm serve`"
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
