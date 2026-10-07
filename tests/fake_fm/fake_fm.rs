//! A fake `fm` for tests. `fake-fm serve --socket <path>` speaks the same
//! endpoints as the real `fm serve`, so CI can test fm-mcp without Apple
//! Intelligence. Built only with `--features fake-fm`.
//!
//! Startup behaviour comes from environment variables, which fm-mcp passes on:
//! - `FAKE_FM_LOG`: append `start pid=<N>` and `request <json>` lines to this file.
//! - `FAKE_FM_AVAILABLE=false`: `/health` reports the model as unavailable.
//! - `FAKE_FM_START_DELAY_MS`: wait before listening (a slow start).
//!
//! Per-request behaviour comes from a magic word in the last message:
//! `FAKE_OVERFLOW`, `FAKE_GUARDRAIL`, `FAKE_BAD_REQUEST` (error responses),
//! `FAKE_HANG` (never answer), `FAKE_CRASH` (exit mid-request), and
//! `FAKE_CRASH_ONCE` (exit mid-request only the first time; needs `FAKE_FM_LOG`).
//! Like the real server, it streams unless the request has `"stream": false`.

// A test helper: failing loudly is the right behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{convert::Infallible, io::Write, path::PathBuf, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, Response, StatusCode, body::Incoming, header::CONTENT_TYPE,
    server::conn::http1, service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::{
    net::UnixListener,
    signal::unix::{SignalKind, signal},
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let socket = match args.as_slice() {
        [_, cmd, flag, path] if cmd == "serve" && flag == "--socket" => PathBuf::from(path),
        _ => {
            eprintln!("usage: fake-fm serve --socket <path>");
            std::process::exit(2);
        }
    };

    log(&format!("start pid={}", std::process::id()));
    if let Some(ms) = std::env::var("FAKE_FM_START_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    let listener = UnixListener::bind(&socket).expect("bind socket");
    let mut terminate = signal(SignalKind::terminate()).expect("listen for SIGTERM");
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.expect("accept");
                tokio::spawn(async move {
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service_fn(handle))
                        .await;
                });
            }
            _ = terminate.recv() => {
                // The real `fm serve` removes its socket on SIGTERM.
                let _ = std::fs::remove_file(&socket);
                std::process::exit(0);
            }
        }
    }
}

async fn handle(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(match (request.method(), request.uri().path()) {
        (&Method::GET, "/health") => {
            let available = std::env::var("FAKE_FM_AVAILABLE").as_deref() != Ok("false");
            reply(
                StatusCode::OK,
                &json!({
                    "models": [{"available": available, "name": "system"}],
                    "status": "fm serve is running"
                }),
            )
        }
        (&Method::POST, "/v1/chat/completions") => {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            chat(&body).await
        }
        _ => reply(StatusCode::NOT_FOUND, &json!({"error": "not found"})),
    })
}

async fn chat(body: &[u8]) -> Response<Full<Bytes>> {
    let request: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    log(&format!("request {request}"));

    let last = request["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_owned();

    if last.contains("FAKE_CRASH_ONCE") {
        if !log_contains("crashed-once") {
            log("crashed-once");
            std::process::exit(3);
        }
    } else if last.contains("FAKE_CRASH") {
        std::process::exit(3);
    }
    if last.contains("FAKE_HANG") {
        std::future::pending::<()>().await;
    }
    if last.contains("FAKE_OVERFLOW") {
        return error(
            500,
            "The session's transcript exceeded the model's context size.",
        );
    }
    if last.contains("FAKE_GUARDRAIL") {
        return error(500, "The model's safety guardrails were triggered.");
    }
    if last.contains("FAKE_BAD_REQUEST") {
        return error(
            400,
            "Invalid JSON: The data couldn't be read because it is missing.",
        );
    }

    let content = format!("Fake summary of {} characters.", last.chars().count());
    if request.get("stream") != Some(&Value::Bool(false)) {
        // The real server streams by default.
        let chunk = json!({"object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"content": content}}]});
        let sse = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        return Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/event-stream")
            .body(Full::new(Bytes::from(sse)))
            .unwrap();
    }
    reply(
        StatusCode::OK,
        &json!({
            "object": "chat.completion",
            "model": "system",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": content, "refusal": null}}],
            "usage": {"prompt_tokens": 60, "completion_tokens": 8, "total_tokens": 68}
        }),
    )
}

fn error(status: u16, message: &str) -> Response<Full<Bytes>> {
    let kind = if status == 400 {
        "invalid_request_error"
    } else {
        "server_error"
    };
    reply(
        StatusCode::from_u16(status).unwrap(),
        &json!({"error": {"code": status.to_string(), "message": message, "type": kind}}),
    )
}

fn reply(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

fn log(line: &str) {
    if let Ok(path) = std::env::var("FAKE_FM_LOG") {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }
}

fn log_contains(needle: &str) -> bool {
    std::env::var("FAKE_FM_LOG")
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|text| text.lines().any(|l| l == needle))
}
