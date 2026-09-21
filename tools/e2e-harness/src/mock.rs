use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::contract::{ClientProtocol, ResponseHeadStatus, Source};
use crate::fixtures::{CLAUDE_NATIVE_KEY, CODEX_NATIVE_KEY};

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct NativeLedgerEntry {
    pub sequence: u64,
    pub source: Source,
    pub path: String,
    pub protocol: ClientProtocol,
    pub model: Option<String>,
    pub response_head_status: u16,
}

pub(crate) struct NativeMock {
    source: Source,
    addr: SocketAddr,
    ledger: Arc<Mutex<Vec<NativeLedgerEntry>>>,
    response_heads: Arc<Mutex<VecDeque<u16>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl NativeMock {
    pub async fn start(source: Source, sequence: Arc<AtomicU64>) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let ledger = Arc::new(Mutex::new(Vec::new()));
        let task_ledger = Arc::clone(&ledger);
        let response_heads = Arc::new(Mutex::new(VecDeque::new()));
        let task_response_heads = Arc::clone(&response_heads);
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let _ = timeout(
                            Duration::from_secs(10),
                            handle_connection(
                                source,
                                stream,
                                Arc::clone(&task_ledger),
                                Arc::clone(&task_response_heads),
                                Arc::clone(&sequence),
                            ),
                        ).await;
                    }
                }
            }
        });
        Ok(Self {
            source,
            addr,
            ledger,
            response_heads,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn snapshot(&self) -> Vec<NativeLedgerEntry> {
        self.ledger.lock().await.clone()
    }

    pub async fn arm_response_heads(&self, statuses: &[ResponseHeadStatus]) {
        let mut response_heads = self.response_heads.lock().await;
        response_heads.clear();
        response_heads.extend(statuses.iter().map(|status| status.as_u16()));
    }

    pub async fn pending_response_heads(&self) -> usize {
        self.response_heads.lock().await.len()
    }

    pub async fn shutdown(mut self, bound: Duration) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if timeout(bound, &mut self.task).await.is_err() {
            self.task.abort();
        }
    }

    pub fn source(&self) -> Source {
        self.source
    }
}

async fn handle_connection(
    source: Source,
    mut stream: TcpStream,
    ledger: Arc<Mutex<Vec<NativeLedgerEntry>>>,
    response_heads: Arc<Mutex<VecDeque<u16>>>,
    sequence: Arc<AtomicU64>,
) -> Result<(), std::io::Error> {
    let request = read_request(&mut stream).await?;
    let expected_path = source.native_path();
    let expected_authorization = match source {
        Source::ClaudeCompatible => format!("Bearer {CLAUDE_NATIVE_KEY}"),
        Source::CodexChatgpt => format!("Bearer {CODEX_NATIVE_KEY}"),
    };
    let parsed = serde_json::from_slice::<Value>(&request.body).ok();
    let protocol_valid = match (source, parsed.as_ref()) {
        (Source::ClaudeCompatible, Some(value)) => {
            value.get("messages").is_some_and(Value::is_array)
        }
        (Source::CodexChatgpt, Some(value)) => value
            .get("input")
            .is_some_and(|input| input.is_array() || input.is_string()),
        _ => false,
    };
    let authentication_valid = request
        .headers
        .get("authorization")
        .is_some_and(|value| value == &expected_authorization);
    if request.method != "POST"
        || request.path != expected_path
        || !protocol_valid
        || !authentication_valid
    {
        return write_response(
            &mut stream,
            404,
            "application/json",
            source,
            br#"{"error":"native contract mismatch"}"#,
        )
        .await;
    }
    let response_head_status = response_heads.lock().await.pop_front().unwrap_or(500);
    ledger.lock().await.push(NativeLedgerEntry {
        sequence: sequence.fetch_add(1, Ordering::Relaxed),
        source,
        path: request.path,
        protocol: source.native_protocol(),
        model: parsed
            .as_ref()
            .and_then(|value| value.get("model"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        response_head_status,
    });
    if response_head_status != 200 {
        return write_response(
            &mut stream,
            response_head_status,
            "application/json",
            source,
            br#"{"error":"scripted response-head failure"}"#,
        )
        .await;
    }
    let body = match source {
        Source::ClaudeCompatible => claude_sse(),
        Source::CodexChatgpt => codex_sse(),
    };
    write_response(
        &mut stream,
        response_head_status,
        "text/event-stream",
        source,
        body.as_bytes(),
    )
    .await
}

struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, std::io::Error> {
    let mut wire = Vec::new();
    let mut scratch = [0_u8; 8192];
    let (head_end, content_length) = loop {
        let count = stream.read(&mut scratch).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request ended before head",
            ));
        }
        wire.extend_from_slice(&scratch[..count]);
        if wire.len() > MAX_REQUEST_BYTES {
            return Err(std::io::Error::other("request exceeds E2E mock limit"));
        }
        if let Some(head_end) = find_bytes(&wire, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&wire[..head_end]);
            let content_length = head
                .lines()
                .skip(1)
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            break (head_end, content_length);
        }
    };
    let body_offset = head_end + 4;
    while wire.len() < body_offset + content_length {
        let count = stream.read(&mut scratch).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request body is truncated",
            ));
        }
        wire.extend_from_slice(&scratch[..count]);
        if wire.len() > MAX_REQUEST_BYTES {
            return Err(std::io::Error::other("request exceeds E2E mock limit"));
        }
    }
    let head = String::from_utf8_lossy(&wire[..head_end]);
    let mut request_line = head.lines().next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let target = request_line.next().unwrap_or_default();
    let path = target.split('?').next().unwrap_or(target).to_owned();
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Request {
        method,
        path,
        headers,
        body: wire[body_offset..body_offset + content_length].to_vec(),
    })
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    source: Source,
    body: &[u8],
) -> Result<(), std::io::Error> {
    let reason = match status {
        200 => "OK",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        529 => "Overloaded",
        _ => "Contract Mismatch",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nCache-Control: no-cache\r\nX-E2E-Native-Source: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        source.as_str(),
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

fn claude_sse() -> String {
    [
        (
            "message_start",
            serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": "msg_hiroute_e2e",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-e2e-native",
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 1, "output_tokens": 0}
                }
            }),
        ),
        (
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
        ),
        (
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "claude-native-ok"}
            }),
        ),
        (
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            "message_delta",
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"output_tokens": 1}
            }),
        ),
        ("message_stop", serde_json::json!({"type": "message_stop"})),
    ]
    .into_iter()
    .map(|(event, data)| sse_event(event, &data))
    .collect()
}

fn codex_sse() -> String {
    let message = serde_json::json!({
        "id": "msg_hiroute_e2e",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "codex-native-ok", "annotations": []}]
    });
    [
        (
            "response.created",
            serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": "resp_hiroute_e2e",
                    "object": "response",
                    "created_at": 1,
                    "status": "in_progress",
                    "model": "gpt-e2e-native",
                    "output": []
                }
            }),
        ),
        (
            "response.output_item.added",
            serde_json::json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": "msg_hiroute_e2e",
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": []
                }
            }),
        ),
        (
            "response.content_part.added",
            serde_json::json!({
                "type": "response.content_part.added",
                "item_id": "msg_hiroute_e2e",
                "output_index": 0,
                "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []}
            }),
        ),
        (
            "response.output_text.delta",
            serde_json::json!({
                "type": "response.output_text.delta",
                "item_id": "msg_hiroute_e2e",
                "output_index": 0,
                "content_index": 0,
                "delta": "codex-native-ok"
            }),
        ),
        (
            "response.output_text.done",
            serde_json::json!({
                "type": "response.output_text.done",
                "item_id": "msg_hiroute_e2e",
                "output_index": 0,
                "content_index": 0,
                "text": "codex-native-ok"
            }),
        ),
        (
            "response.content_part.done",
            serde_json::json!({
                "type": "response.content_part.done",
                "item_id": "msg_hiroute_e2e",
                "output_index": 0,
                "content_index": 0,
                "part": {"type": "output_text", "text": "codex-native-ok", "annotations": []}
            }),
        ),
        (
            "response.output_item.done",
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": message.clone()
            }),
        ),
        (
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_hiroute_e2e",
                    "object": "response",
                    "created_at": 1,
                    "status": "completed",
                    "model": "gpt-e2e-native",
                    "output": [message],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                }
            }),
        ),
    ]
    .into_iter()
    .map(|(event, data)| sse_event(event, &data))
    .collect()
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_streams_expose_terminal_events() {
        assert!(claude_sse().contains("message_stop"));
        assert!(codex_sse().contains("response.completed"));
    }
}
