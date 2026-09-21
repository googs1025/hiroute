use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use thiserror::Error;

use crate::config::SecretText;

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

pub(crate) struct LoopbackRequest<'a> {
    pub(crate) address: SocketAddr,
    pub(crate) method: &'static str,
    pub(crate) path: &'a str,
    pub(crate) authorization: Option<&'a SecretText>,
    pub(crate) body: &'a [u8],
    pub(crate) timeout: Duration,
}

pub(crate) struct LoopbackResponse {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl LoopbackResponse {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

pub(crate) fn request(input: LoopbackRequest<'_>) -> Result<LoopbackResponse, LoopbackHttpError> {
    if !matches!(input.method, "GET" | "HEAD" | "PATCH" | "POST")
        || !input.path.starts_with('/')
        || input.path.contains(['\r', '\n', '\0'])
        || input.body.len() > MAX_RESPONSE_BYTES
    {
        return Err(LoopbackHttpError::InvalidRequest);
    }
    if !input.address.ip().is_loopback() {
        return Err(LoopbackHttpError::NonLoopback);
    }
    let mut stream =
        TcpStream::connect_timeout(&input.address, input.timeout).map_err(LoopbackHttpError::Io)?;
    stream
        .set_read_timeout(Some(input.timeout))
        .map_err(LoopbackHttpError::Io)?;
    stream
        .set_write_timeout(Some(input.timeout))
        .map_err(LoopbackHttpError::Io)?;

    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\n",
        input.method, input.path, input.address
    );
    if let Some(secret) = input.authorization {
        head.push_str("Authorization: Bearer ");
        head.push_str(secret.expose());
        head.push_str("\r\n");
    }
    if !input.body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", input.body.len()));
    stream
        .write_all(head.as_bytes())
        .map_err(LoopbackHttpError::Io)?;
    stream
        .write_all(input.body)
        .map_err(LoopbackHttpError::Io)?;
    stream.flush().map_err(LoopbackHttpError::Io)?;

    let mut raw = Vec::new();
    stream
        .take(u64::try_from(MAX_RESPONSE_BYTES + 1).expect("response bound fits u64"))
        .read_to_end(&mut raw)
        .map_err(LoopbackHttpError::Io)?;
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(LoopbackHttpError::ResponseTooLarge);
    }
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<LoopbackResponse, LoopbackHttpError> {
    let header_end = find_bytes(raw, b"\r\n\r\n").ok_or(LoopbackHttpError::MalformedResponse)?;
    if header_end > MAX_HEADER_BYTES {
        return Err(LoopbackHttpError::MalformedResponse);
    }
    let header = std::str::from_utf8(&raw[..header_end])
        .map_err(|_| LoopbackHttpError::MalformedResponse)?;
    let mut lines = header.split("\r\n");
    let status_line = lines.next().ok_or(LoopbackHttpError::MalformedResponse)?;
    let mut status_parts = status_line.split_ascii_whitespace();
    if status_parts.next() != Some("HTTP/1.1") {
        return Err(LoopbackHttpError::MalformedResponse);
    }
    let status = status_parts
        .next()
        .ok_or(LoopbackHttpError::MalformedResponse)?
        .parse::<u16>()
        .map_err(|_| LoopbackHttpError::MalformedResponse)?;
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or(LoopbackHttpError::MalformedResponse)?;
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || headers.insert(name, value.trim().to_owned()).is_some()
        {
            return Err(LoopbackHttpError::MalformedResponse);
        }
    }
    let encoded_body = &raw[header_end + 4..];
    let transfer_encoding = headers.get("transfer-encoding");
    if transfer_encoding.is_some() && headers.contains_key("content-length") {
        return Err(LoopbackHttpError::MalformedResponse);
    }
    let body = if let Some(encoding) = transfer_encoding {
        if !encoding.eq_ignore_ascii_case("chunked") {
            return Err(LoopbackHttpError::MalformedResponse);
        }
        decode_chunked(encoded_body)?
    } else if let Some(length) = headers.get("content-length") {
        let expected = length
            .parse::<usize>()
            .map_err(|_| LoopbackHttpError::MalformedResponse)?;
        if expected != encoded_body.len() {
            return Err(LoopbackHttpError::MalformedResponse);
        }
        encoded_body.to_vec()
    } else {
        encoded_body.to_vec()
    };
    Ok(LoopbackResponse {
        status,
        headers,
        body,
    })
}

fn decode_chunked(raw: &[u8]) -> Result<Vec<u8>, LoopbackHttpError> {
    let mut cursor = 0_usize;
    let mut body = Vec::new();
    loop {
        let line_end = find_bytes(&raw[cursor..], b"\r\n")
            .ok_or(LoopbackHttpError::MalformedResponse)?
            + cursor;
        let line = std::str::from_utf8(&raw[cursor..line_end])
            .map_err(|_| LoopbackHttpError::MalformedResponse)?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or_default().trim(), 16)
            .map_err(|_| LoopbackHttpError::MalformedResponse)?;
        cursor = line_end + 2;
        if size == 0 {
            if raw.get(cursor..cursor + 2) != Some(b"\r\n") || cursor + 2 != raw.len() {
                return Err(LoopbackHttpError::MalformedResponse);
            }
            break;
        }
        let end = cursor
            .checked_add(size)
            .ok_or(LoopbackHttpError::MalformedResponse)?;
        if end > raw.len()
            || raw.get(end..end + 2) != Some(b"\r\n")
            || body.len().saturating_add(size) > MAX_RESPONSE_BYTES
        {
            return Err(LoopbackHttpError::MalformedResponse);
        }
        body.extend_from_slice(&raw[cursor..end]);
        cursor = end + 2;
    }
    Ok(body)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(crate) fn percent_encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

#[derive(Debug, Error)]
pub(crate) enum LoopbackHttpError {
    #[error("CPA loopback request is malformed")]
    InvalidRequest,
    #[error("CPA transport destination is not loopback")]
    NonLoopback,
    #[error("CPA loopback I/O failed: {0}")]
    Io(std::io::Error),
    #[error("CPA loopback response exceeded its bound")]
    ResponseTooLarge,
    #[error("CPA loopback response is malformed")]
    MalformedResponse,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_content_length_response() {
        let response = parse_response(
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 2\r\nRetry-After: 7\r\n\r\n{}",
        )
        .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(response.header("retry-after"), Some("7"));
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn parses_chunked_response_without_accepting_trailing_ambiguity() {
        let response = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(response.body, b"{}");
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}").is_err());
        assert!(
            parse_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n2\r\n{}\r\n0\r\n\r\n"
            )
            .is_err()
        );
        assert!(
            parse_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\ntrailing"
            )
            .is_err()
        );
    }

    #[test]
    fn query_encoding_does_not_create_a_second_parameter() {
        assert_eq!(percent_encode_query("a&name=b.json"), "a%26name%3Db.json");
    }
}
