//! One bounded signed-JSON codec for all opaque Worker cursors.

use hiroute_application_api::MAX_WORKER_CURSOR_BYTES;
use hiroute_domain::delegation::DelegationErrorV1;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::LocalControlAdapter;

const WORKER_CURSOR_SCHEMA: u8 = 1;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedWorkerCursorV1<T> {
    schema: u8,
    payload: T,
    signature: String,
}

impl LocalControlAdapter {
    pub(super) fn encode_worker_cursor<T: Serialize>(
        &self,
        purpose: &str,
        payload: T,
    ) -> Result<String, DelegationErrorV1> {
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|_| DelegationErrorV1::StorageUnavailable)?;
        let encoded = serde_json::to_string(&SignedWorkerCursorV1 {
            schema: WORKER_CURSOR_SCHEMA,
            signature: self
                .delegation_digest_authority
                .worker_cursor_signature(purpose, &payload_bytes),
            payload,
        })
        .map_err(|_| DelegationErrorV1::StorageUnavailable)?;
        if encoded.len() > MAX_WORKER_CURSOR_BYTES {
            return Err(DelegationErrorV1::StorageUnavailable);
        }
        Ok(encoded)
    }

    pub(super) fn decode_worker_cursor<T>(
        &self,
        purpose: &str,
        encoded: &str,
    ) -> Result<T, DelegationErrorV1>
    where
        T: DeserializeOwned + Serialize,
    {
        if encoded.is_empty() || encoded.len() > MAX_WORKER_CURSOR_BYTES {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        let signed: SignedWorkerCursorV1<T> =
            serde_json::from_str(encoded).map_err(|_| DelegationErrorV1::InvalidArguments)?;
        if signed.schema != WORKER_CURSOR_SCHEMA {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        let payload_bytes =
            serde_json::to_vec(&signed.payload).map_err(|_| DelegationErrorV1::InvalidArguments)?;
        if !self
            .delegation_digest_authority
            .verify_worker_cursor_signature(purpose, &payload_bytes, &signed.signature)
        {
            return Err(DelegationErrorV1::InvalidArguments);
        }
        Ok(signed.payload)
    }
}
