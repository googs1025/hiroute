use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::http::{HttpError, post_json};

use super::canonical::canonical_json_bytes;
use super::types::{AuthorizationFixture, CorpusCase, Protocol};

pub(crate) async fn send(
    addr: SocketAddr,
    case: &CorpusCase,
    valid_authorization: &str,
    request_id: &str,
    deadline: Duration,
) -> Result<Value, ClientError> {
    let authorization = match case.ingress.authorization {
        AuthorizationFixture::Missing => None,
        AuthorizationFixture::Invalid => Some(agent_grant_header(
            case.ingress.protocol,
            "Bearer invalid-e2e-capability",
        )?),
        AuthorizationFixture::Valid => Some(agent_grant_header(
            case.ingress.protocol,
            valid_authorization,
        )?),
    };
    let mut headers = Vec::new();
    if let Some(header) = authorization {
        headers.push(header);
    }
    headers.push(("X-HiRoute-Oracle-Request", request_id));
    let response = post_json(
        addr,
        &case.ingress.path,
        &headers,
        &canonical_json_bytes(&case.ingress.body),
        deadline,
    )
    .await?;
    let content_type =
        response
            .header("content-type")
            .ok_or_else(|| ClientError::MissingContentType {
                status: response.status,
                headers: format!("{:?}", response.headers),
                body: String::from_utf8_lossy(&response.body).into_owned(),
            })?;
    let media_type = content_type
        .split_once(';')
        .map_or(content_type, |(value, _)| value)
        .trim();
    let body = match media_type {
        "application/json" => serde_json::from_slice(&response.body)?,
        "text/event-stream" => json!({"events": parse_sse(&response.body)?}),
        _ => return Err(ClientError::UnsupportedContentType(content_type.to_owned())),
    };
    Ok(json!({
        "body": body,
        "content_type": media_type,
        "status": response.status,
        "transport": "http1"
    }))
}

fn agent_grant_header(
    protocol: Protocol,
    authorization: &str,
) -> Result<(&'static str, &str), ClientError> {
    if protocol == Protocol::Responses {
        let token = authorization
            .strip_prefix("Bearer ")
            .filter(|token| !token.is_empty())
            .ok_or(ClientError::InvalidAgentGrantHeader)?;
        Ok(("X-HiRoute-Token", token))
    } else {
        Ok(("Authorization", authorization))
    }
}

fn parse_sse(bytes: &[u8]) -> Result<Vec<Value>, ClientError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ClientError::InvalidSse("non-UTF-8"))?;
    let normalized = text.replace("\r\n", "\n");
    let mut events = Vec::new();
    for block in normalized.split("\n\n").filter(|block| !block.is_empty()) {
        let mut name = None;
        let mut data = Vec::new();
        for line in block.lines() {
            if line.starts_with(':') {
                continue;
            }
            if let Some(value) = line.strip_prefix("event:") {
                if name.replace(value.trim_start().to_owned()).is_some() {
                    return Err(ClientError::InvalidSse("duplicate event field"));
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                data.push(value.trim_start());
            } else if !line.is_empty() {
                return Err(ClientError::InvalidSse("unsupported SSE field"));
            }
        }
        if data.is_empty() {
            return Err(ClientError::InvalidSse("event has no data"));
        }
        let joined = data.join("\n");
        let data = if joined == "[DONE]" {
            Value::String(joined)
        } else {
            serde_json::from_str(&joined)?
        };
        let mut value = Map::new();
        value.insert("data".into(), data);
        value.insert(
            "event".into(),
            name.map(Value::String).unwrap_or(Value::Null),
        );
        events.push(Value::Object(value));
    }
    if events.is_empty() {
        return Err(ClientError::InvalidSse("stream has no events"));
    }
    Ok(events)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(
        "client response has no content type (status {status}, headers {headers}, body {body})"
    )]
    MissingContentType {
        status: u16,
        headers: String,
        body: String,
    },
    #[error("client response content type is not frozen: {0}")]
    UnsupportedContentType(String),
    #[error("client response JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("client SSE is invalid: {0}")]
    InvalidSse(&'static str),
    #[error("Responses agent grant must be a non-empty Bearer value")]
    InvalidAgentGrantHeader,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_preserves_typed_event_order() {
        let events =
            parse_sse(b"event: delta\ndata: {\"text\":\"a\"}\n\ndata: [DONE]\n\n").unwrap();
        assert_eq!(
            events,
            vec![
                json!({"event": "delta", "data": {"text": "a"}}),
                json!({"event": null, "data": "[DONE]"})
            ]
        );
    }

    #[test]
    fn responses_uses_the_independent_agent_grant_header() {
        assert_eq!(
            agent_grant_header(Protocol::Responses, "Bearer model-grant").unwrap(),
            ("X-HiRoute-Token", "model-grant")
        );
        assert_eq!(
            agent_grant_header(Protocol::Messages, "Bearer model-grant").unwrap(),
            ("Authorization", "Bearer model-grant")
        );
        assert!(agent_grant_header(Protocol::Responses, "model-grant").is_err());
        assert!(agent_grant_header(Protocol::Responses, "Bearer ").is_err());
    }
}
