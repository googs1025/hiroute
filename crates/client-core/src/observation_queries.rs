//! Thin typed shared-transport facade. The native caller supplies the protected
//! grant; WebView payloads can express only a finite observation intent.
use crate::{Client, ClientFailure, FailureCode};
use hiroute_application_api::*;
use serde::de::DeserializeOwned;
impl Client {
    pub async fn observation_read<R: DeserializeOwned>(
        &self,
        request_id: &str,
        query: ObservationReadRequestV2,
        grant: ProtectedClientGrantV2,
    ) -> Result<MachineEnvelopeV2<R>, ClientFailure> {
        self.call_typed(LocalControlWireRequestV2 {
            schema_version: LOCAL_CONTROL_SCHEMA_V2,
            request_id: request_id.into(),
            operation_id: query.operation().into(),
            payload: serde_json::to_value(query)
                .map_err(|_| ClientFailure::before_send(FailureCode::FrameInvalid))?,
            protected_grant: Some(grant),
        })
        .await
    }
}

impl Client {
    pub async fn observation_delete_preview(
        &self,
        request_id: &str,
        query: ObservationDeletePreviewRequestV2,
        grant: ProtectedClientGrantV2,
    ) -> Result<MachineEnvelopeV2<SessionDeletionPreviewV2>, ClientFailure> {
        self.call_typed(LocalControlWireRequestV2 {
            schema_version: LOCAL_CONTROL_SCHEMA_V2,
            request_id: request_id.into(),
            operation_id: "PreviewSessionDeletion".into(),
            payload: serde_json::to_value(query)
                .map_err(|_| ClientFailure::before_send(FailureCode::FrameInvalid))?,
            protected_grant: Some(grant),
        })
        .await
    }
    pub async fn observation_delete_apply(
        &self,
        request_id: &str,
        query: ObservationDeleteApplyRequestV2,
        grant: ProtectedClientGrantV2,
    ) -> Result<MachineEnvelopeV2<SessionDeletionOutcomeV2>, ClientFailure> {
        self.call_typed(LocalControlWireRequestV2 {
            schema_version: LOCAL_CONTROL_SCHEMA_V2,
            request_id: request_id.into(),
            operation_id: "ApplySessionDeletion".into(),
            payload: serde_json::to_value(query)
                .map_err(|_| ClientFailure::before_send(FailureCode::FrameInvalid))?,
            protected_grant: Some(grant),
        })
        .await
    }
}
