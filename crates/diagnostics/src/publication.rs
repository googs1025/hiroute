//! Debug-only elapsed wall time for publication and its durable Operation processing.
//! Stages may nest; their inclusive durations must not be summed across levels.
use crate::{
    DiagnosticEvent, correlation::CorrelationDomain, identity::CorrelationToken,
    runtime::DiagnosticsPort,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStage {
    RoutingSnapshot,
    PlanAuthoringSnapshot,
    OperationCurrent,
    PublicationCheckpoint,
    PublicationInstall,
    PlanVersionStage,
    PlanHeadCommit,
    OperationRead,
    OperationDecode,
    OperationJsonParse,
    OperationReconstruct,
    OperationPlanValidate,
    OperationRestore,
    OperationSave,
    OperationSaveTail,
    Admission,
    Execute,
    PublicationApply,
    PublicationObserve,
    PublicationActivate,
    ProductActivationFinish,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationTiming {
    pub stage: PublicationStage,
    pub elapsed_us: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_token: Option<CorrelationToken>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_bytes: Option<usize>,
}

/// Preserve the exact result, including early errors. No payload serialization for sizing.
pub fn measure<T, E>(
    port: &DiagnosticsPort,
    stage: PublicationStage,
    operation: Option<&str>,
    bytes: Option<usize>,
    work: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    if !port.handle().is_enabled()
        || matches!(
            port.handle().level(),
            Some(
                crate::DiagnosticLevel::Info
                    | crate::DiagnosticLevel::Warn
                    | crate::DiagnosticLevel::Error
            )
        )
    {
        return work();
    }
    let started = Instant::now();
    let result = work();
    let elapsed_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    port.emit(DiagnosticEvent::PublicationTiming(PublicationTiming {
        stage,
        elapsed_us,
        ok: result.is_ok(),
        operation_token: operation
            .and_then(|id| port.handle().token(CorrelationDomain::Operation, id)),
        operation_bytes: bytes,
    }));
    result
}
