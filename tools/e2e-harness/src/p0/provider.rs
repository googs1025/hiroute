use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use super::bindings::RuntimeBindings;
use super::canonical::canonical_json_digest;
use super::types::{Protocol, ProviderBody, ProviderScript};

const MAX_NATIVE_REQUEST_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct NativeProvider {
    addr: SocketAddr,
    observations: Arc<Mutex<Vec<ObservedNativeRequest>>>,
    accepted: Arc<AtomicU64>,
    active: Arc<AtomicU64>,
    parse_failed: Arc<AtomicU64>,
    aborted: Arc<AtomicU64>,
    infrastructure_failures: Arc<Mutex<Vec<String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderSnapshot {
    pub observations: Vec<ObservedNativeRequest>,
    pub accepted: u64,
    pub active: u64,
    pub parse_failed: u64,
    pub aborted: u64,
    pub infrastructure_failures: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ObservedNativeRequest {
    pub sequence: u64,
    pub value: Value,
}

struct ConnectionContext {
    provider_id: String,
    protocol: Protocol,
    expected_authorization: String,
    response_wire: Arc<Vec<u8>>,
    sequence: Arc<AtomicU64>,
    observations: Arc<Mutex<Vec<ObservedNativeRequest>>>,
    capture_body: bool,
}

impl NativeProvider {
    pub async fn start(
        script: &ProviderScript,
        expected_authorization: &str,
        sequence: Arc<AtomicU64>,
        bindings: &RuntimeBindings,
    ) -> Result<Self, std::io::Error> {
        Self::start_with_capture(script, expected_authorization, sequence, bindings, false).await
    }

    pub async fn start_exact(
        script: &ProviderScript,
        expected_authorization: &str,
        sequence: Arc<AtomicU64>,
        bindings: &RuntimeBindings,
    ) -> Result<Self, std::io::Error> {
        Self::start_with_capture(script, expected_authorization, sequence, bindings, true).await
    }

    async fn start_with_capture(
        script: &ProviderScript,
        expected_authorization: &str,
        sequence: Arc<AtomicU64>,
        bindings: &RuntimeBindings,
        capture_body: bool,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let observations = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicU64::new(0));
        let active = Arc::new(AtomicU64::new(0));
        let parse_failed = Arc::new(AtomicU64::new(0));
        let aborted = Arc::new(AtomicU64::new(0));
        let infrastructure_failures = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task_failures = Arc::clone(&infrastructure_failures);
        let context = Arc::new(ConnectionContext {
            provider_id: script.id.clone(),
            protocol: script.protocol,
            expected_authorization: expected_authorization.to_owned(),
            response_wire: Arc::new(response_wire(script, bindings)?),
            sequence,
            observations: Arc::clone(&observations),
            capture_body,
        });
        let task_accepted = Arc::clone(&accepted);
        let task_active = Arc::clone(&active);
        let task_parse_failed = Arc::clone(&parse_failed);
        let task_aborted = Arc::clone(&aborted);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => {
                        while let Ok(Ok((stream, _))) =
                            timeout(Duration::from_millis(1), listener.accept()).await
                        {
                            spawn_connection(
                                stream,
                                Arc::clone(&context),
                                &task_accepted,
                                &task_active,
                                &mut connections,
                            );
                        }
                        break;
                    },
                    completed = connections.join_next(), if !connections.is_empty() => {
                        observe_completion(completed, &task_active, &task_parse_failed, &task_failures).await;
                    },
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _)) => {
                                spawn_connection(
                                    stream,
                                    Arc::clone(&context),
                                    &task_accepted,
                                    &task_active,
                                    &mut connections,
                                );
                            }
                            Err(error) => {
                                task_failures.lock().await.push(error.to_string());
                                break;
                            }
                        }
                    }
                }
            }
            let drain = async {
                while !connections.is_empty() {
                    let completed = connections.join_next().await;
                    observe_completion(completed, &task_active, &task_parse_failed, &task_failures)
                        .await;
                }
            };
            if timeout(Duration::from_millis(250), drain).await.is_err() {
                let remaining = connections.len() as u64;
                task_aborted.fetch_add(remaining, Ordering::AcqRel);
                connections.abort_all();
                while connections.join_next().await.is_some() {
                    task_active.fetch_sub(1, Ordering::AcqRel);
                }
            }
        });
        Ok(Self {
            addr,
            observations,
            accepted,
            active,
            parse_failed,
            aborted,
            infrastructure_failures,
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn seal_and_snapshot(&mut self, bound: Duration) -> ProviderSnapshot {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut task) = self.task.take()
            && timeout(bound, &mut task).await.is_err()
        {
            task.abort();
            let _ = task.await;
            let active = self.active.swap(0, Ordering::AcqRel);
            self.aborted.fetch_add(active, Ordering::AcqRel);
        }
        ProviderSnapshot {
            observations: self.observations.lock().await.clone(),
            accepted: self.accepted.load(Ordering::Acquire),
            active: self.active.load(Ordering::Acquire),
            parse_failed: self.parse_failed.load(Ordering::Acquire),
            aborted: self.aborted.load(Ordering::Acquire),
            infrastructure_failures: self.infrastructure_failures.lock().await.clone(),
        }
    }

    pub async fn shutdown(mut self, bound: Duration) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut task) = self.task.take()
            && timeout(bound, &mut task).await.is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

fn spawn_connection(
    stream: TcpStream,
    context: Arc<ConnectionContext>,
    accepted: &AtomicU64,
    active: &AtomicU64,
    connections: &mut JoinSet<Result<(), std::io::Error>>,
) {
    accepted.fetch_add(1, Ordering::AcqRel);
    active.fetch_add(1, Ordering::AcqRel);
    connections.spawn(async move { handle_connection(stream, &context).await });
}

async fn observe_completion(
    completed: Option<Result<Result<(), std::io::Error>, tokio::task::JoinError>>,
    active: &AtomicU64,
    parse_failed: &AtomicU64,
    failures: &Mutex<Vec<String>>,
) {
    let Some(completed) = completed else {
        return;
    };
    active.fetch_sub(1, Ordering::AcqRel);
    match completed {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            parse_failed.fetch_add(1, Ordering::AcqRel);
            failures.lock().await.push(error.to_string());
        }
        Err(error) if error.is_cancelled() => {}
        Err(error) => failures.lock().await.push(error.to_string()),
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    context: &ConnectionContext,
) -> Result<(), std::io::Error> {
    let request = read_request(&mut stream).await?;
    let body: Value = serde_json::from_slice(&request.body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let authorization = match request.headers.get("authorization") {
        Some(value) if value.as_bytes() == context.expected_authorization.as_bytes() => "exact",
        Some(_) => "wrong",
        None => "missing",
    };
    let mut value = json!({
        "authorization": authorization,
        "body_digest": canonical_json_digest(&body),
        "body_semantics": request_semantics(context.protocol, &body),
        "content_type": request.headers.get("content-type").cloned(),
        "method": request.method,
        "path": request.path,
        "protocol": context.protocol.as_str(),
        "provider_id": context.provider_id,
    });
    if context.capture_body {
        value
            .as_object_mut()
            .expect("native observation is an object")
            .insert("body".into(), body);
    }
    context
        .observations
        .lock()
        .await
        .push(ObservedNativeRequest {
            sequence: context.sequence.fetch_add(1, Ordering::AcqRel),
            value,
        });
    stream.write_all(&context.response_wire).await?;
    stream.flush().await
}

struct NativeRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> Result<NativeRequest, std::io::Error> {
    let mut wire = Vec::new();
    let mut scratch = [0_u8; 16 * 1024];
    let (head_end, content_length, chunked) = loop {
        let count = stream.read(&mut scratch).await?;
        if count == 0 {
            return Err(invalid_data("native request ended before its head"));
        }
        wire.extend_from_slice(&scratch[..count]);
        if wire.len() > MAX_NATIVE_REQUEST_BYTES {
            return Err(invalid_data("native request exceeded capture limit"));
        }
        if let Some(head_end) = find_bytes(&wire, b"\r\n\r\n") {
            let head = std::str::from_utf8(&wire[..head_end])
                .map_err(|_| invalid_data("native request head is not UTF-8"))?;
            let headers = parse_headers(head)?;
            let length = headers
                .get("content-length")
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| invalid_data("invalid native content length"))?;
            let chunked = headers.get("transfer-encoding").is_some_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
            });
            break (head_end, length, chunked);
        }
    };
    let body_offset = head_end + 4;
    loop {
        let complete = if chunked {
            decode_chunked(&wire[body_offset..])?.is_some()
        } else if let Some(length) = content_length {
            wire.len().saturating_sub(body_offset) >= length
        } else {
            false
        };
        if complete {
            break;
        }
        let count = stream.read(&mut scratch).await?;
        if count == 0 {
            return Err(invalid_data("native request body was truncated"));
        }
        wire.extend_from_slice(&scratch[..count]);
        if wire.len() > MAX_NATIVE_REQUEST_BYTES {
            return Err(invalid_data("native request exceeded capture limit"));
        }
    }
    let head = std::str::from_utf8(&wire[..head_end])
        .map_err(|_| invalid_data("native request head is not UTF-8"))?;
    let mut lines = head.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| invalid_data("native request line is missing"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| invalid_data("native method is missing"))?
        .to_owned();
    let path = parts
        .next()
        .ok_or_else(|| invalid_data("native path is missing"))?
        .to_owned();
    let headers = parse_headers(head)?;
    let body = if chunked {
        decode_chunked(&wire[body_offset..])?
            .ok_or_else(|| invalid_data("native chunked body is incomplete"))?
    } else {
        let length = content_length.ok_or_else(|| invalid_data("native body framing missing"))?;
        wire[body_offset..body_offset + length].to_vec()
    };
    Ok(NativeRequest {
        method,
        path,
        headers,
        body,
    })
}

fn parse_headers(head: &str) -> Result<BTreeMap<String, String>, std::io::Error> {
    let mut headers = BTreeMap::new();
    for line in head.lines().skip(1) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid_data("native header has no colon"))?;
        if headers
            .insert(name.trim().to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err(invalid_data("duplicate native header"));
        }
    }
    Ok(headers)
}

fn decode_chunked(wire: &[u8]) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut cursor = 0;
    let mut decoded = Vec::new();
    loop {
        let Some(relative_end) = find_bytes(&wire[cursor..], b"\r\n") else {
            return Ok(None);
        };
        let size_end = cursor + relative_end;
        let size_text = std::str::from_utf8(&wire[cursor..size_end])
            .map_err(|_| invalid_data("native chunk size is not UTF-8"))?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or_default(), 16)
            .map_err(|_| invalid_data("invalid native chunk size"))?;
        cursor = size_end + 2;
        if size == 0 {
            return (wire.len() >= cursor + 2)
                .then_some(decoded)
                .map_or(Ok(None), |value| Ok(Some(value)));
        }
        if wire.len() < cursor + size + 2 {
            return Ok(None);
        }
        decoded.extend_from_slice(&wire[cursor..cursor + size]);
        cursor += size;
        if wire.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err(invalid_data("native chunk has no terminator"));
        }
        cursor += 2;
    }
}

fn request_semantics(protocol: Protocol, body: &Value) -> Value {
    let (instructions, content, reasoning) = match protocol {
        Protocol::Responses => (
            body.get("instructions"),
            body.get("input"),
            body.get("reasoning"),
        ),
        Protocol::ChatCompletions => (None, body.get("messages"), body.get("reasoning_effort")),
        Protocol::Messages => (
            body.get("system"),
            body.get("messages"),
            body.get("thinking").or_else(|| body.get("output_config")),
        ),
    };
    json!({
        "content_digest": digest_optional(content),
        "instructions_digest": digest_optional(instructions),
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "reasoning_digest": digest_optional(reasoning),
        "stream": body.get("stream").cloned().unwrap_or(Value::Bool(false)),
        "tool_choice_digest": digest_optional(body.get("tool_choice")),
        "tools_digest": digest_optional(body.get("tools"))
    })
}

fn digest_optional(value: Option<&Value>) -> Value {
    value
        .map(canonical_json_digest)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

#[cfg(test)]
pub(crate) fn response_payload_digest(script: &ProviderScript) -> String {
    let value = match &script.response.body {
        ProviderBody::Json { value } => value.clone(),
        ProviderBody::Sse { events } => Value::Array(
            events
                .iter()
                .map(|event| json!({"event": event.event, "data": event.data}))
                .collect(),
        ),
    };
    canonical_json_digest(&value)
}

pub(crate) fn expected_native_value(script: &ProviderScript, ordinal: usize) -> Value {
    let body = &script.expected_request.body;
    json!({
        "authorization": "exact",
        "body_digest": canonical_json_digest(body),
        "body_semantics": request_semantics(script.protocol, body),
        "content_type": "application/json",
        "method": "POST",
        "ordinal": ordinal,
        "path": script.expected_request.path,
        "protocol": script.protocol.as_str(),
        "provider_id": script.id
    })
}

fn response_wire(
    script: &ProviderScript,
    bindings: &RuntimeBindings,
) -> Result<Vec<u8>, std::io::Error> {
    let body = match &script.response.body {
        ProviderBody::Json { value } => serde_json::to_vec(&bindings.materialize_global(value))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
        ProviderBody::Sse { events } => {
            let mut body = Vec::new();
            for event in events {
                if let Some(name) = &event.event {
                    body.extend_from_slice(b"event: ");
                    body.extend_from_slice(name.as_bytes());
                    body.push(b'\n');
                }
                body.extend_from_slice(b"data: ");
                if event.data.as_str() == Some("[DONE]") {
                    body.extend_from_slice(b"[DONE]");
                } else {
                    body.extend_from_slice(&super::canonical::canonical_json_bytes(
                        &bindings.materialize_global(&event.data),
                    ));
                }
                body.extend_from_slice(b"\n\n");
            }
            body
        }
    };
    let reason = match script.response.status {
        200 => "OK",
        400 => "Bad Request",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => return Err(invalid_data("unsupported native response status")),
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        script.response.status,
        script.response.content_type,
        body.len()
    );
    let mut wire = head.into_bytes();
    wire.extend_from_slice(&body);
    Ok(wire)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|item| item == needle)
}

fn invalid_data(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::p0::canonical::sha256_hex;
    use crate::p0::types::{NativeRequestExpectation, ProviderResponse, ProviderScript};

    #[test]
    fn response_payload_digest_changes_with_wrong_provider_payload() {
        let mut script = ProviderScript {
            id: "provider".into(),
            protocol: Protocol::Responses,
            expected_calls: 1,
            expected_request: NativeRequestExpectation {
                path: "/v1/responses".into(),
                body: json!({"model": "physical"}),
            },
            response: ProviderResponse {
                status: 200,
                content_type: "application/json".into(),
                body: ProviderBody::Json {
                    value: json!({"model": "native-a"}),
                },
            },
        };
        let expected = response_payload_digest(&script);
        script.response.body = ProviderBody::Json {
            value: json!({"model": "native-b"}),
        };
        assert_ne!(expected, response_payload_digest(&script));
        assert!(expected.starts_with("sha256:"));
        assert_ne!(sha256_hex(b"native-a"), sha256_hex(b"native-b"));
    }
}
