//! A fake `fm` for tests. `fake-fm serve --socket <path>` speaks the same
//! endpoints as the real `fm serve`, so CI can test fm-mcp without Apple
//! Intelligence. Built only with `--features fake-fm`.
//!
//! Startup behaviour comes from environment variables, which fm-mcp passes on:
//! - `FAKE_FM_LOG`: append `start pid=<N>` and `request <json>` lines to this file.
//! - `FAKE_FM_AVAILABLE=false`: `/health` and `available` report the model as unavailable.
//! - `FAKE_FM_LICENSE=false`: `license --status` reports the licence as not agreed.
//! - `FAKE_FM_START_DELAY_MS`: wait before listening (a slow start).
//!
//! Per-request behaviour comes from a magic word in the last message:
//! `FAKE_OVERFLOW`, `FAKE_GUARDRAIL`, `FAKE_BAD_REQUEST` (error responses),
//! `FAKE_HANG` (never answer), `FAKE_CRASH` (exit mid-request), and
//! `FAKE_CRASH_ONCE` (exit mid-request only the first time; needs `FAKE_FM_LOG`),
//! `FAKE_REPEAT` (every array in a structured answer lists its items twice,
//! as the real model sometimes repeats `multi` labels), and `FAKE_RUN_ON` (the
//! answer reports using its whole `max_completion_tokens`, as a runaway does).
//! Like the real server, it streams unless the request has `"stream": false`.
//! Structured-output requests get a value that fits the schema (`null` for
//! nullable fields), and a message with an image gets `Fake text read from
//! an image.`. `count-tokens -q` (4 chars per token), `license --status` and
//! `available` are also faked.

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
        [_, cmd, ..] if cmd == "count-tokens" => count_tokens(),
        [_, cmd, flag] if cmd == "license" && flag == "--status" => license_status(),
        [_, cmd] if cmd == "available" => available(),
        _ => {
            eprintln!(
                "usage: fake-fm serve --socket <path> | count-tokens -q | license --status | available"
            );
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

    // Structured output: answer with a value that fits the schema.
    let schema = &request["response_format"]["json_schema"]["schema"];
    let has_image = request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .any(|part| part["type"] == "image_url");
    let content = if has_image {
        "Fake text read from an image.".to_owned()
    } else if schema.is_null() {
        format!("Fake summary of {} characters.", last.chars().count())
    } else if last.contains("FAKE_REPEAT") {
        repeat_arrays(fake_instance(schema)).to_string()
    } else {
        fake_instance(schema).to_string()
    };
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
    let completion_tokens = if last.contains("FAKE_RUN_ON") {
        request["max_completion_tokens"].as_u64().unwrap_or(8)
    } else {
        8
    };
    reply(
        StatusCode::OK,
        &json!({
            "object": "chat.completion",
            "model": "system",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": content, "refusal": null}}],
            "usage": {"prompt_tokens": 60, "completion_tokens": completion_tokens, "total_tokens": 60 + completion_tokens}
        }),
    )
}

/// A small value that satisfies `schema`: the first `enum` option, `null` for
/// nullable fields (as the real model does for missing data), and simple
/// placeholders otherwise.
fn fake_instance(schema: &Value) -> Value {
    if let Some(first) = schema["enum"].as_array().and_then(|e| e.first()) {
        return first.clone();
    }
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(options) = schema["anyOf"].as_array() {
        return if options.iter().any(|o| o["type"] == "null") {
            Value::Null
        } else {
            options.first().map(fake_instance).unwrap_or(Value::Null)
        };
    }
    match schema["type"].as_str() {
        Some("object") => {
            let mut object = serde_json::Map::new();
            if let Some(properties) = schema["properties"].as_object() {
                for (name, property) in properties {
                    object.insert(name.clone(), fake_instance(property));
                }
            }
            Value::Object(object)
        }
        Some("array") => {
            let count = schema["minItems"].as_u64().unwrap_or(1).max(1);
            Value::Array(
                (0..count)
                    .map(|_| fake_instance(&schema["items"]))
                    .collect(),
            )
        }
        Some("string") => json!("fake"),
        Some("integer") => json!(1),
        Some("number") => json!(1.5),
        Some("boolean") => json!(false),
        _ => Value::Null,
    }
}

fn repeat_arrays(value: Value) -> Value {
    match value {
        Value::Array(items) => {
            let items: Vec<Value> = items.into_iter().map(repeat_arrays).collect();
            Value::Array(items.iter().chain(&items).cloned().collect())
        }
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(k, v)| (k, repeat_arrays(v)))
                .collect(),
        ),
        other => other,
    }
}

/// `fake-fm count-tokens -q`: four characters per token, read from stdin.
/// Like the real `fm`, empty input is an error.
fn count_tokens() -> ! {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).unwrap();
    if input.is_empty() {
        eprintln!("Error: Missing prompt. Provide a positional prompt, --text, or --image option.");
        std::process::exit(1);
    }
    println!("{}", input.chars().count().div_ceil(4));
    std::process::exit(0);
}

/// Real output when agreed (2026-10-06). The not-agreed text is made up:
/// the real one is untested.
fn license_status() -> ! {
    if std::env::var("FAKE_FM_LICENSE").as_deref() == Ok("false") {
        println!("Not agreed.");
        std::process::exit(1);
    }
    println!("Agreed to license FM1 version 1.0 on 6 Oct 2026 at 15:34.");
    std::process::exit(0);
}

/// Real output when ready (2026-10-06). The unavailable text is made up.
fn available() -> ! {
    if std::env::var("FAKE_FM_AVAILABLE").as_deref() == Ok("false") {
        println!("System model unavailable.");
        std::process::exit(1);
    }
    println!("System model available");
    std::process::exit(0);
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
