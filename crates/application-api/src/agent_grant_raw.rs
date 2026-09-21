use thiserror::Error;

pub const AGENT_GRANT_RAW_MAGIC_V1: &[u8; 8] = b"HIRAGENT";
pub const AGENT_GRANT_RAW_VERSION_V1: u16 = 1;
pub const MAX_AGENT_GRANT_CONNECTION_ID_BYTES_V1: usize = 256;
pub const MAX_AGENT_GRANT_TOKEN_BYTES_V1: usize = 512;

#[derive(Clone, Eq, PartialEq)]
pub struct AgentGrantRawRequestV1 {
    connection_id: String,
}

impl AgentGrantRawRequestV1 {
    pub fn new(connection_id: impl Into<String>) -> Result<Self, AgentGrantRawProtocolError> {
        let connection_id = connection_id.into();
        validate_connection_id(&connection_id)?;
        Ok(Self { connection_id })
    }

    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub fn encode(&self) -> Vec<u8> {
        let bytes = self.connection_id.as_bytes();
        let mut frame = Vec::with_capacity(12 + bytes.len());
        frame.extend_from_slice(AGENT_GRANT_RAW_MAGIC_V1);
        frame.extend_from_slice(&AGENT_GRANT_RAW_VERSION_V1.to_be_bytes());
        frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        frame.extend_from_slice(bytes);
        frame
    }

    pub fn decode(frame: &[u8]) -> Result<Self, AgentGrantRawProtocolError> {
        if frame.len() < 12
            || &frame[..8] != AGENT_GRANT_RAW_MAGIC_V1
            || u16::from_be_bytes([frame[8], frame[9]]) != AGENT_GRANT_RAW_VERSION_V1
        {
            return Err(AgentGrantRawProtocolError::InvalidFrame);
        }
        let length = usize::from(u16::from_be_bytes([frame[10], frame[11]]));
        if length == 0
            || length > MAX_AGENT_GRANT_CONNECTION_ID_BYTES_V1
            || frame.len() != 12 + length
        {
            return Err(AgentGrantRawProtocolError::InvalidFrame);
        }
        let connection_id = std::str::from_utf8(&frame[12..])
            .map_err(|_| AgentGrantRawProtocolError::InvalidFrame)?;
        Self::new(connection_id)
    }
}

fn validate_connection_id(value: &str) -> Result<(), AgentGrantRawProtocolError> {
    let valid = value
        .strip_prefix("agent-connection/")
        .is_some_and(|suffix| !suffix.is_empty())
        && value.len() <= MAX_AGENT_GRANT_CONNECTION_ID_BYTES_V1
        && !value.contains("..")
        && !value.contains("//")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        });
    if valid {
        Ok(())
    } else {
        Err(AgentGrantRawProtocolError::InvalidConnectionId)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AgentGrantRawProtocolError {
    #[error("agent-grant raw request connection id is invalid")]
    InvalidConnectionId,
    #[error("agent-grant raw request frame is invalid")]
    InvalidFrame,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_request_is_versioned_bounded_and_single_connection_only() {
        let request = AgentGrantRawRequestV1::new("agent-connection/claude-default").unwrap();
        assert!(AgentGrantRawRequestV1::decode(&request.encode()).unwrap() == request);
        let settings_request = AgentGrantRawRequestV1::new(
            "agent-connection/agent-context/codex/sha256:0123456789abcdef",
        )
        .unwrap();
        assert!(
            AgentGrantRawRequestV1::decode(&settings_request.encode()).unwrap() == settings_request
        );
        for invalid in [
            "",
            "*",
            "connection/claude",
            "agent-connection/",
            "agent-connection/../other",
            "agent-connection//other",
        ] {
            assert!(AgentGrantRawRequestV1::new(invalid).is_err());
        }
        let mut wrong_version = request.encode();
        wrong_version[9] = 2;
        assert!(matches!(
            AgentGrantRawRequestV1::decode(&wrong_version),
            Err(AgentGrantRawProtocolError::InvalidFrame)
        ));
    }
}
