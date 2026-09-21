use hiroute_application_api::{CHANGE_SPEC_SCHEMA_V1, ChangeSpecV1};
use serde_json::{Value, json};

pub fn settings_change(control: Value) -> ChangeSpecV1 {
    ChangeSpecV1 {
        schema_version: CHANGE_SPEC_SCHEMA_V1,
        command_id: "settings.apply".to_owned(),
        resource_id: Some("personal/default/settings".to_owned()),
        desired_state: json!({
            "control": control,
            "runtime": [],
            "external": []
        }),
    }
}
