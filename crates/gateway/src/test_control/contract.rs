use hiroute_gateway_core::core::publication::InstallError;
use http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::server::composition::RuntimeStateStore;
use crate::server::publication::{
    GatewayPrepareOutcome, GatewayPublicationInstaller, GatewayPublicationSnapshotV3,
    PublicationInstallError, PublicationSchemaError, PublishedGatewayPublication,
};

use super::runtime_state::{
    FaultingRuntimeStateStore, RuntimeStateFaultControl, RuntimeStateWriteOperation,
};

pub const REQUEST_SCHEMA: &str = "hiroute.gateway.e2e-control-request/v1";
pub const RESPONSE_SCHEMA: &str = "hiroute.gateway.e2e-control-response/v1";

const MAX_REQUEST_ID_BYTES: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlRequestEnvelope {
    schema_version: String,
    request_id: String,
    command: TestControlCommand,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TestControlCommand {
    PublicationUpdate {
        snapshot: GatewayPublicationSnapshotV3,
    },
    RuntimeStateWriteFault {
        operation: RuntimeStateWriteOperation,
        failure_count: u32,
    },
}

/// The command dispatcher is deliberately separate from the listener,
/// authentication, and versioned envelope. PROCESS-22014 can add another
/// typed command here and share this handle without opening a second socket.
#[derive(Clone, Debug)]
pub struct E2eControlHandle {
    publications: std::sync::Arc<GatewayPublicationInstaller>,
    runtime_state_faults: std::sync::Arc<RuntimeStateFaultControl>,
}

impl E2eControlHandle {
    pub fn new(publications: std::sync::Arc<GatewayPublicationInstaller>) -> Self {
        Self {
            publications,
            runtime_state_faults: Default::default(),
        }
    }

    pub(crate) fn wrap_runtime_state(
        &self,
        inner: std::sync::Arc<dyn RuntimeStateStore>,
    ) -> std::sync::Arc<dyn RuntimeStateStore> {
        std::sync::Arc::new(FaultingRuntimeStateStore::new(
            inner,
            std::sync::Arc::clone(&self.runtime_state_faults),
        ))
    }

    pub(super) fn execute(&self, request: ControlRequestEnvelope) -> ControlResponse {
        if request.schema_version != REQUEST_SCHEMA || !valid_request_id(&request.request_id) {
            return ControlResponse::nack(
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                None,
                "E2E_CONTROL_SCHEMA_INCOMPATIBLE",
                None,
                None,
            );
        }
        match request.command {
            TestControlCommand::PublicationUpdate { snapshot } => {
                self.update_publication(request.request_id, snapshot)
            }
            TestControlCommand::RuntimeStateWriteFault {
                operation,
                failure_count,
            } => self.arm_runtime_state_fault(request.request_id, operation, failure_count),
        }
    }

    fn arm_runtime_state_fault(
        &self,
        request_id: String,
        operation: RuntimeStateWriteOperation,
        failure_count: u32,
    ) -> ControlResponse {
        match self.runtime_state_faults.arm(operation, failure_count) {
            Ok(()) => {
                ControlResponse::runtime_state_fault_ack(request_id, operation, failure_count)
            }
            Err(()) => ControlResponse::nack(
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(request_id),
                Some("runtime_state_write_fault"),
                "RUNTIME_STATE_WRITE_FAULT_INVALID",
                None,
                None,
            ),
        }
    }

    fn update_publication(
        &self,
        request_id: String,
        snapshot: GatewayPublicationSnapshotV3,
    ) -> ControlResponse {
        let proposed_revision = snapshot.publication_revision;
        let outcome = match self.publications.prepare(snapshot) {
            Ok(GatewayPrepareOutcome::Prepared(prepared)) => self
                .publications
                .publish(prepared)
                .map(|active| ("PUBLICATION_APPLIED", active)),
            Ok(GatewayPrepareOutcome::Duplicate(active)) => {
                Ok(("PUBLICATION_ALREADY_ACTIVE", active))
            }
            Err(error) => Err(error),
        };
        match outcome {
            Ok((code, active)) => ControlResponse::ack(
                request_id,
                "publication_update",
                code,
                proposed_revision,
                ActivePublicationReceipt::from(active.as_ref()),
            ),
            Err(error) => {
                let (status, code) = publication_nack(&error);
                ControlResponse::nack(
                    status,
                    Some(request_id),
                    Some("publication_update"),
                    code,
                    Some(proposed_revision),
                    self.publications
                        .active()
                        .as_deref()
                        .map(ActivePublicationReceipt::from),
                )
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ControlResponse {
    #[serde(skip)]
    pub(super) status: StatusCode,
    schema_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
    outcome: &'static str,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    proposed_publication_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<ActivePublicationReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_state_operation: Option<RuntimeStateWriteOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining_failures: Option<u32>,
}

impl ControlResponse {
    fn ack(
        request_id: String,
        command: &'static str,
        code: &'static str,
        proposed_revision: u64,
        active: ActivePublicationReceipt,
    ) -> Self {
        Self {
            status: StatusCode::OK,
            schema_version: RESPONSE_SCHEMA,
            request_id: Some(request_id),
            command: Some(command),
            outcome: "ack",
            code,
            proposed_publication_revision: Some(proposed_revision),
            active: Some(active),
            runtime_state_operation: None,
            remaining_failures: None,
        }
    }

    fn runtime_state_fault_ack(
        request_id: String,
        operation: RuntimeStateWriteOperation,
        remaining_failures: u32,
    ) -> Self {
        Self {
            status: StatusCode::OK,
            schema_version: RESPONSE_SCHEMA,
            request_id: Some(request_id),
            command: Some("runtime_state_write_fault"),
            outcome: "ack",
            code: "RUNTIME_STATE_WRITE_FAULT_ARMED",
            proposed_publication_revision: None,
            active: None,
            runtime_state_operation: Some(operation),
            remaining_failures: Some(remaining_failures),
        }
    }

    pub(super) fn unauthorized() -> Self {
        Self::nack(
            StatusCode::FORBIDDEN,
            None,
            None,
            "E2E_CONTROL_UNAUTHORIZED",
            None,
            None,
        )
    }

    pub(super) fn malformed(code: &'static str, status: StatusCode) -> Self {
        Self::nack(status, None, None, code, None, None)
    }

    fn nack(
        status: StatusCode,
        request_id: Option<String>,
        command: Option<&'static str>,
        code: &'static str,
        proposed_revision: Option<u64>,
        active: Option<ActivePublicationReceipt>,
    ) -> Self {
        Self {
            status,
            schema_version: RESPONSE_SCHEMA,
            request_id,
            command,
            outcome: "nack",
            code,
            proposed_publication_revision: proposed_revision,
            active,
            runtime_state_operation: None,
            remaining_failures: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct ActivePublicationReceipt {
    authority_id: String,
    authority_epoch: u64,
    publication_revision: u64,
    publication_digest: String,
}

impl From<&PublishedGatewayPublication> for ActivePublicationReceipt {
    fn from(active: &PublishedGatewayPublication) -> Self {
        Self {
            authority_id: active.authority_id().into(),
            authority_epoch: active.authority_epoch(),
            publication_revision: active.publication_revision(),
            publication_digest: active.payload_digest().into(),
        }
    }
}

fn publication_nack(error: &PublicationInstallError) -> (StatusCode, &'static str) {
    match error {
        PublicationInstallError::Schema(PublicationSchemaError::Schema) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "PUBLICATION_SCHEMA_INCOMPATIBLE",
        ),
        PublicationInstallError::Schema(PublicationSchemaError::DigestMismatch) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "PUBLICATION_DIGEST_INVALID",
        ),
        PublicationInstallError::Schema(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "PUBLICATION_INVALID")
        }
        PublicationInstallError::Core(InstallError::ResyncRequired { .. }) => {
            (StatusCode::CONFLICT, "PUBLICATION_REVISION_GAP")
        }
        PublicationInstallError::Core(InstallError::IncompatibleSchema(_))
        | PublicationInstallError::Core(InstallError::IncompatibleCompiler(_)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "PUBLICATION_CORE_INCOMPATIBLE",
        ),
        PublicationInstallError::Core(
            InstallError::AuthorityConflict | InstallError::StaleAuthorityEpoch,
        ) => (StatusCode::CONFLICT, "PUBLICATION_AUTHORITY_CONFLICT"),
        PublicationInstallError::Core(
            InstallError::RevisionDigestConflict | InstallError::RollbackNotAuthorized,
        ) => (StatusCode::CONFLICT, "PUBLICATION_REVISION_CONFLICT"),
        PublicationInstallError::PrepareBusy => (StatusCode::CONFLICT, "PUBLICATION_UPDATE_BUSY"),
        error if error.is_crash_boundary() => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "PUBLICATION_DURABILITY_UNCERTAIN",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "PUBLICATION_UPDATE_FAILED",
        ),
    }
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
