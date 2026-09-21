use hiroute_application_api::{
    DelegationCancelRequestV1, DelegationContinueRequestV1, DelegationGetRequestV1,
    DelegationListRequestV1, DelegationResultRequestV1, DelegationStartRequestV1,
    DelegationWaitRequestV1, ErrorCode,
};
use std::io::Read;

pub(crate) fn payload(command: &str, options: &[String]) -> Result<serde_json::Value, ErrorCode> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ErrorCode::InvalidArguments)?;
    if bytes.len() > 1024 * 1024 {
        return Err(ErrorCode::InvalidArguments);
    }
    let value = parse(command, &bytes)?;
    validate_options(command, options, &value)?;
    Ok(value)
}

fn validate_options(
    command: &str,
    options: &[String],
    value: &serde_json::Value,
) -> Result<(), ErrorCode> {
    let invalid = || Err(ErrorCode::InvalidArguments);
    match (command, options) {
        ("tasks.list" | "tasks.start" | "tasks.continue", [stdin])
            if stdin == "--request-stdin" =>
        {
            Ok(())
        }
        ("tasks.wait" | "tasks.result" | "tasks.cancel", [flag, run, stdin])
            if flag == "--run"
                && !run.is_empty()
                && stdin == "--request-stdin"
                && value["run_id"].as_str() == Some(run.as_str()) =>
        {
            Ok(())
        }
        ("tasks.show", [task_flag, task, stdin])
            if task_flag == "--task"
                && !task.is_empty()
                && stdin == "--request-stdin"
                && value["task_id"].as_str() == Some(task.as_str())
                && value.get("run_id").is_none() =>
        {
            Ok(())
        }
        ("tasks.show", [task_flag, task, run_flag, run, stdin])
            if task_flag == "--task"
                && run_flag == "--run"
                && !task.is_empty()
                && !run.is_empty()
                && stdin == "--request-stdin"
                && value["task_id"].as_str() == Some(task.as_str())
                && value["run_id"].as_str() == Some(run.as_str()) =>
        {
            Ok(())
        }
        ("tasks.show", [key_flag, key, operation_flag, operation, stdin])
            if key_flag == "--submission-key"
                && operation_flag == "--operation"
                && !key.is_empty()
                && matches!(operation.as_str(), "start" | "continue")
                && stdin == "--request-stdin"
                && value["submission_key"].as_str() == Some(key.as_str())
                && value["submission_operation"].as_str() == Some(operation.as_str()) =>
        {
            Ok(())
        }
        _ => invalid(),
    }
}

fn parse(command: &str, bytes: &[u8]) -> Result<serde_json::Value, ErrorCode> {
    let invalid = |_| ErrorCode::InvalidArguments;
    match command {
        "tasks.list" => typed::<DelegationListRequestV1>(bytes, invalid),
        "tasks.show" => typed::<DelegationGetRequestV1>(bytes, invalid),
        "tasks.start" => {
            let request: DelegationStartRequestV1 =
                serde_json::from_slice(bytes).map_err(invalid)?;
            if !request.valid() {
                return Err(ErrorCode::InvalidArguments);
            }
            serde_json::to_value(request).map_err(invalid)
        }
        "tasks.wait" => typed::<DelegationWaitRequestV1>(bytes, invalid),
        "tasks.result" => {
            let request: DelegationResultRequestV1 =
                serde_json::from_slice(bytes).map_err(invalid)?;
            if !request.valid() {
                return Err(ErrorCode::InvalidArguments);
            }
            serde_json::to_value(request).map_err(invalid)
        }
        "tasks.cancel" => typed::<DelegationCancelRequestV1>(bytes, invalid),
        "tasks.continue" => typed::<DelegationContinueRequestV1>(bytes, invalid),
        _ => Err(ErrorCode::UnknownCommand),
    }
}

fn typed<T>(
    bytes: &[u8],
    invalid: fn(serde_json::Error) -> ErrorCode,
) -> Result<serde_json::Value, ErrorCode>
where
    T: serde::de::DeserializeOwned + serde::Serialize + ValidRequest,
{
    let request: T = serde_json::from_slice(bytes).map_err(invalid)?;
    if !request.valid() {
        return Err(ErrorCode::InvalidArguments);
    }
    serde_json::to_value(request).map_err(invalid)
}

trait ValidRequest {
    fn valid(&self) -> bool;
}

macro_rules! valid_request {
    ($($type:ty),+ $(,)?) => {$(
        impl ValidRequest for $type {
            fn valid(&self) -> bool { <$type>::valid(self) }
        }
    )+};
}

valid_request!(
    DelegationListRequestV1,
    DelegationGetRequestV1,
    DelegationWaitRequestV1,
    DelegationCancelRequestV1,
    DelegationContinueRequestV1,
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn caller() -> serde_json::Value {
        json!({
            "workspace_id": "personal/default",
            "context_id": "owner",
            "grant_id": "collaboration-grant/one",
            "grant_generation": 1
        })
    }

    #[test]
    fn show_submission_selector_must_match_the_typed_request() {
        let bytes = serde_json::to_vec(&json!({
            "caller": caller(),
            "submission_key": "continue/one",
            "submission_operation": "continue"
        }))
        .unwrap();
        let value = parse("tasks.show", &bytes).unwrap();
        assert!(
            validate_options(
                "tasks.show",
                &[
                    "--submission-key".into(),
                    "continue/one".into(),
                    "--operation".into(),
                    "continue".into(),
                    "--request-stdin".into(),
                ],
                &value,
            )
            .is_ok()
        );
        assert_eq!(
            validate_options(
                "tasks.show",
                &[
                    "--submission-key".into(),
                    "different".into(),
                    "--operation".into(),
                    "continue".into(),
                    "--request-stdin".into(),
                ],
                &value,
            ),
            Err(ErrorCode::InvalidArguments)
        );
    }

    #[test]
    fn wait_run_selector_must_match_the_typed_request() {
        let bytes = serde_json::to_vec(&json!({
            "caller": caller(),
            "run_id": "run/one",
            "after_revision": 2,
            "wait_ms": 1
        }))
        .unwrap();
        let value = parse("tasks.wait", &bytes).unwrap();
        assert!(
            validate_options(
                "tasks.wait",
                &["--run".into(), "run/one".into(), "--request-stdin".into()],
                &value,
            )
            .is_ok()
        );
        assert_eq!(
            validate_options(
                "tasks.wait",
                &["--run".into(), "run/two".into(), "--request-stdin".into()],
                &value,
            ),
            Err(ErrorCode::InvalidArguments)
        );
    }
}
