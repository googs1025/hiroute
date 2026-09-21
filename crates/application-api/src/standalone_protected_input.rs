use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub const STANDALONE_PROTECTED_INPUT_REQUEST_SCHEMA_V1: &str =
    "hiroute.standalone-protected-input-request/v1";
pub const STANDALONE_PROTECTED_INPUT_RESPONSE_SCHEMA_V1: &str =
    "hiroute.standalone-protected-input-response/v1";
pub const STANDALONE_PROTECTED_INPUT_MAX_FRAME_BYTES_V1: u64 = 64 * 1024;

/// Same-UID request accepted only by the standalone daemon's private protected-input socket.
/// The optional secret is zeroized on every success and failure path.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandaloneProtectedInputRequestV1 {
    pub schema: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

impl StandaloneProtectedInputRequestV1 {
    pub fn register(candidate_ref: impl Into<String>, secret: String) -> Self {
        Self {
            schema: STANDALONE_PROTECTED_INPUT_REQUEST_SCHEMA_V1.into(),
            action: "register".into(),
            candidate_ref: Some(candidate_ref.into()),
            candidate_revision: Some(1),
            secret: Some(secret),
        }
    }

    pub fn release(candidate_ref: impl Into<String>) -> Self {
        Self {
            schema: STANDALONE_PROTECTED_INPUT_REQUEST_SCHEMA_V1.into(),
            action: "release".into(),
            candidate_ref: Some(candidate_ref.into()),
            candidate_revision: None,
            secret: None,
        }
    }
}

impl Drop for StandaloneProtectedInputRequestV1 {
    fn drop(&mut self) {
        if let Some(secret) = &mut self.secret {
            secret.zeroize();
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandaloneProtectedInputResponseV1 {
    pub schema: String,
    pub action: String,
    pub registered: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_input_contract_is_strict_and_has_no_apply_authority_fields() {
        let request: Result<StandaloneProtectedInputRequestV1, _> = serde_json::from_slice(
            br#"{"schema":"hiroute.standalone-protected-input-request/v1","action":"register","candidate_ref":"candidate/manual","candidate_revision":1,"secret":"x","operation_kind":"ApplyAgentPlanChange"}"#,
        );
        assert!(request.is_err());

        let request = StandaloneProtectedInputRequestV1::register("candidate/manual", "x".into());
        let value = serde_json::to_value(&request).unwrap();
        assert!(value.get("capability").is_none());
        assert!(value.get("operation_kind").is_none());
        assert!(value.get("accepted_digest").is_none());
        assert!(value.get("expected_revisions").is_none());
    }
}
