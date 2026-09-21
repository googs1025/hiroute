//! Read-only public Worker progress query and its purpose-separated opaque cursor.

use hiroute_application::delegation::tasks::WorkerReadErrorV1;
use hiroute_application_api::{WorkerReadContentStateV1, WorkerReadDataV1, WorkerReadRequestV1};
use hiroute_domain::delegation::{DelegationErrorV1, DelegationRuntimePort};
use hiroute_observation::managed_text::{
    ManagedTextProgressCursor, ManagedTextProgressRead, ManagedTextProgressReadError,
    ManagedTextProgressRecovery, ManagedTextProgressTarget, ManagedTextScope,
};
use serde::{Deserialize, Serialize};

use super::{
    LocalControlAdapter,
    delegation_task_queries::{authorize, run_view},
};

const WORKER_READ_CURSOR_PURPOSE: &str = "WorkerReadV1";
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerReadCursorPayloadV1 {
    workspace_id: String,
    task_id: String,
    run_id: String,
    visibility_generation: String,
    segment: String,
    position: String,
}

impl LocalControlAdapter {
    pub(super) fn read_worker_progress(
        &self,
        request: &WorkerReadRequestV1,
    ) -> Result<WorkerReadDataV1, WorkerReadErrorV1> {
        if !request.valid() {
            return Err(WorkerReadErrorV1::InvalidCursor);
        }
        let caller = self.worker_instance();
        let workspace = caller.workspace_id();
        let run = DelegationRuntimePort::run(self, workspace, &request.run_id)?
            .ok_or(DelegationErrorV1::NotFound)?;
        let task = DelegationRuntimePort::task(self, workspace, &run.task_id)?
            .ok_or(DelegationErrorV1::NotFound)?;
        authorize(&caller, &task, &run)?;
        let accepted_at_ms = run.accepted_at_ms.ok_or(WorkerReadErrorV1::Unavailable)?;
        let created_at_ms =
            i64::try_from(accepted_at_ms).map_err(|_| WorkerReadErrorV1::StorageUnavailable)?;
        let target = ManagedTextProgressTarget {
            scope: ManagedTextScope {
                workspace_id: workspace.clone(),
                task_id: run.task_id.clone(),
                run_id: run.run_id.clone(),
            },
            created_at_ms,
        };
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| self.parse_read_cursor(&target.scope, cursor))
            .transpose()?;
        let read = self
            .delegation_observation
            .managed_text_progress_read(&target, cursor, request.effective_max_bytes() as usize)
            .map_err(|error| self.map_progress_read_error(&target.scope, error))?;
        let view = run_view(&task, &run);
        let common = |content_state| WorkerReadDataV1 {
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            run_state: view.state,
            state_revision: view.state_revision,
            content_state,
            segment: None,
            window_start: None,
            window_end: None,
            text: None,
            next_cursor: None,
            has_more: false,
            truncated: None,
        };
        match read {
            ManagedTextProgressRead::Missing {
                visibility_generation,
            } if !run.progress.prompt_may_have_executed => {
                let cursor = ManagedTextProgressCursor {
                    visibility_generation,
                    segment: 0,
                    position: 0,
                };
                let mut data = common(WorkerReadContentStateV1::Pending);
                data.segment = Some("0".into());
                data.window_start = Some("0".into());
                data.window_end = Some("0".into());
                data.text = Some(String::new());
                data.next_cursor = Some(self.read_cursor(&target.scope, cursor)?);
                data.truncated = Some(false);
                Ok(data)
            }
            ManagedTextProgressRead::Missing { .. } => Err(WorkerReadErrorV1::Unavailable),
            ManagedTextProgressRead::Available(page) => {
                let cursor = ManagedTextProgressCursor {
                    visibility_generation: page.visibility_generation,
                    segment: page.segment,
                    position: page.next_position,
                };
                let mut data = common(WorkerReadContentStateV1::Available);
                data.segment = Some(page.segment.to_string());
                data.window_start = Some(page.window_start.to_string());
                data.window_end = Some(page.window_end.to_string());
                data.text = Some(page.text);
                data.next_cursor = Some(self.read_cursor(&target.scope, cursor)?);
                data.has_more = page.has_more;
                data.truncated = Some(page.truncated);
                Ok(data)
            }
            ManagedTextProgressRead::Deleted => Ok(common(WorkerReadContentStateV1::Deleted)),
            ManagedTextProgressRead::Expired => Ok(common(WorkerReadContentStateV1::Expired)),
        }
    }

    fn map_progress_read_error(
        &self,
        scope: &ManagedTextScope,
        error: ManagedTextProgressReadError,
    ) -> WorkerReadErrorV1 {
        match error {
            ManagedTextProgressReadError::InvalidCursor => WorkerReadErrorV1::InvalidCursor,
            ManagedTextProgressReadError::PageTooSmall => WorkerReadErrorV1::PageTooSmall,
            ManagedTextProgressReadError::Stale => WorkerReadErrorV1::CursorStale,
            ManagedTextProgressReadError::Evicted(recovery) => self
                .read_recovery_cursor(scope, recovery)
                .map_or(WorkerReadErrorV1::StorageUnavailable, |recovery_cursor| {
                    WorkerReadErrorV1::CursorEvicted { recovery_cursor }
                }),
            ManagedTextProgressReadError::Gap(recovery) => self
                .read_recovery_cursor(scope, recovery)
                .map_or(WorkerReadErrorV1::StorageUnavailable, |recovery_cursor| {
                    WorkerReadErrorV1::CursorGap { recovery_cursor }
                }),
            ManagedTextProgressReadError::Storage => WorkerReadErrorV1::StorageUnavailable,
        }
    }

    fn read_recovery_cursor(
        &self,
        scope: &ManagedTextScope,
        recovery: ManagedTextProgressRecovery,
    ) -> Result<String, WorkerReadErrorV1> {
        self.read_cursor(
            scope,
            ManagedTextProgressCursor {
                visibility_generation: recovery.visibility_generation,
                segment: recovery.segment,
                position: recovery.position,
            },
        )
    }

    fn read_cursor(
        &self,
        scope: &ManagedTextScope,
        cursor: ManagedTextProgressCursor,
    ) -> Result<String, WorkerReadErrorV1> {
        let visibility_generation = cursor.visibility_generation.to_string();
        let segment = cursor.segment.to_string();
        let position = cursor.position.to_string();
        self.encode_worker_cursor(
            WORKER_READ_CURSOR_PURPOSE,
            WorkerReadCursorPayloadV1 {
                workspace_id: scope.workspace_id.as_str().to_owned(),
                task_id: scope.task_id.clone(),
                run_id: scope.run_id.clone(),
                visibility_generation,
                segment,
                position,
            },
        )
        .map_err(|_| WorkerReadErrorV1::StorageUnavailable)
    }

    fn parse_read_cursor(
        &self,
        scope: &ManagedTextScope,
        cursor: &str,
    ) -> Result<ManagedTextProgressCursor, WorkerReadErrorV1> {
        let parsed: WorkerReadCursorPayloadV1 = self
            .decode_worker_cursor(WORKER_READ_CURSOR_PURPOSE, cursor)
            .map_err(|_| WorkerReadErrorV1::InvalidCursor)?;
        if parsed.workspace_id != scope.workspace_id.as_str()
            || parsed.task_id != scope.task_id
            || parsed.run_id != scope.run_id
        {
            return Err(WorkerReadErrorV1::InvalidCursor);
        }
        Ok(ManagedTextProgressCursor {
            visibility_generation: canonical_u64(&parsed.visibility_generation)?,
            segment: canonical_u64(&parsed.segment)?,
            position: canonical_u64(&parsed.position)?,
        })
    }
}

fn canonical_u64(value: &str) -> Result<u64, WorkerReadErrorV1> {
    if value.is_empty()
        || value.len() > 19
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(WorkerReadErrorV1::InvalidCursor);
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or(WorkerReadErrorV1::InvalidCursor)
}
