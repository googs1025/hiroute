use serde_json::{Value, json};

fn caller() -> Value {
    json!({"type":"object","additionalProperties":false,
        "required":["workspace_id","context_id","grant_id","grant_generation"],
        "properties":{"workspace_id":{"type":"string","minLength":1},
            "context_id":{"type":"string","minLength":1,"maxLength":256},
            "grant_id":{"type":"string","pattern":"^collaboration-grant/","maxLength":256},
            "grant_generation":{"type":"integer","minimum":1}}})
}
pub(super) fn start() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
    "$id":"hiroute://contracts/cli/delegation-start-request.v1.schema.json",
    "type":"object","additionalProperties":false,
    "required":["schema","caller","agent_plan_id","permit_id","permit_generation","workspace_path","execution","input","idempotency_key"],
    "properties":{
        "schema":{"const":"hiroute.delegation-start-request/v1"},"caller":caller(),
        "agent_plan_id":{"type":"string","minLength":1},
        "expected_plan":{"type":"object","additionalProperties":false,"required":["workspace_id","plan_id","content_revision","content_digest"],
            "properties":{"workspace_id":{"type":"string"},"plan_id":{"type":"string"},"content_revision":{"type":"integer","minimum":1},"content_digest":{"type":"string","pattern":"^sha256:[0-9a-f]{64}$"}}},
        "permit_id":{"type":"string","minLength":1,"maxLength":256},"permit_generation":{"type":"integer","minimum":1},
        "workspace_path":{"type":"string","minLength":1,"maxLength":4096,"description":"Absolute local directory; the daemon verifies canonical filesystem identity."},
        "execution":{"type":"object","additionalProperties":false,"required":["root_identity","access","tools","network","duration_ms","delegation_depth"],
            "properties":{"root_identity":{"type":"string","minLength":1},"access":{"enum":["read_only","workspace_write","trusted_native"]},
                "tools":{"type":"array","maxItems":3,"items":{"enum":["read","edit","shell"]}},"network":{"enum":["gateway_only","allowed"]},
                "duration_ms":{"type":"integer","minimum":1,"maximum":86400000},"delegation_depth":{"const":1}}},
        "input":{"type":"object","additionalProperties":false,"required":["goal"],"description":"Combined text is limited to 256 KiB.",
            "properties":{"goal":{"type":"string","minLength":1},"context":{"type":"string"},"constraints":{"type":"string"},"acceptance_criteria":{"type":"string"}}},
        "idempotency_key":{"type":"string","minLength":1,"maxLength":256},"parent_task_ref":{"type":"string","minLength":1,"maxLength":256}
    }})
}
pub(super) fn result() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$id":"hiroute://contracts/cli/delegation-result-request.v1.schema.json",
        "type":"object","additionalProperties":false,"required":["caller","run_id"],
        "properties":{"caller":caller(),"run_id":{"type":"string","minLength":1,"maxLength":256},
            "offset":{"type":"integer","minimum":0,"maximum":u32::MAX},
            "max_bytes":{"type":"integer","minimum":1,"maximum":1048576}}})
}

pub(super) fn list() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$id":"hiroute://contracts/cli/delegation-list-request.v1.schema.json",
        "type":"object","additionalProperties":false,"required":["caller"],
        "properties":{"caller":caller(),"cursor":{"type":"string","pattern":"^sequence/[1-9][0-9]*$"},
            "limit":{"type":"integer","minimum":1,"maximum":200}}})
}

pub(super) fn get() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
    "$id":"hiroute://contracts/cli/delegation-get-request.v1.schema.json",
    "type":"object","additionalProperties":false,"required":["caller"],
    "properties":{"caller":caller(),"task_id":{"type":"string","minLength":1,"maxLength":256},
        "run_id":{"type":"string","minLength":1,"maxLength":256},
        "submission_key":{"type":"string","minLength":1,"maxLength":256},
        "submission_operation":{"enum":["start","continue"]}},
    "oneOf":[
        {"required":["task_id"],"not":{"anyOf":[{"required":["submission_key"]},{"required":["submission_operation"]}]}},
        {"required":["submission_key","submission_operation"],"not":{"anyOf":[{"required":["task_id"]},{"required":["run_id"]}]}}
    ]})
}

pub(super) fn wait() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$id":"hiroute://contracts/cli/delegation-wait-request.v1.schema.json",
        "type":"object","additionalProperties":false,"required":["caller","run_id"],
        "properties":{"caller":caller(),"run_id":{"type":"string","minLength":1,"maxLength":256},
            "after_revision":{"type":"integer","minimum":0},
            "wait_ms":{"type":"integer","minimum":1,"maximum":30000}}})
}

pub(super) fn cancel() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$id":"hiroute://contracts/cli/delegation-cancel-request.v1.schema.json",
        "type":"object","additionalProperties":false,
        "required":["caller","run_id","idempotency_key"],
        "properties":{"caller":caller(),"run_id":{"type":"string","minLength":1,"maxLength":256},
            "idempotency_key":{"type":"string","minLength":1,"maxLength":256},
            "reason":{"type":"string","maxLength":256,"pattern":"^[A-Za-z0-9_./:-]*$"}}})
}

pub(super) fn continue_request() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$id":"hiroute://contracts/cli/delegation-continue-request.v1.schema.json",
        "type":"object","additionalProperties":false,
        "required":["caller","task_id","expected_latest_run_id","permit_id","permit_generation","execution","input","idempotency_key"],
        "properties":{"caller":caller(),"task_id":{"type":"string","minLength":1,"maxLength":256},
            "expected_latest_run_id":{"type":"string","minLength":1,"maxLength":256},
            "permit_id":{"type":"string","minLength":1,"maxLength":256},
            "permit_generation":{"type":"integer","minimum":1},
            "execution":{"type":"object","additionalProperties":false,
                "required":["root_identity","access","tools","network","duration_ms","delegation_depth"],
                "properties":{"root_identity":{"type":"string","minLength":1},
                    "access":{"enum":["read_only","workspace_write","trusted_native"]},
                    "tools":{"type":"array","maxItems":3,"items":{"enum":["read","edit","shell"]}},
                    "network":{"enum":["gateway_only","allowed"]},
                    "duration_ms":{"type":"integer","minimum":1,"maximum":86400000},
                    "delegation_depth":{"const":1}}},
            "input":{"type":"object","additionalProperties":false,"required":["goal"],
                "description":"Combined text is limited to 256 KiB.",
                "properties":{"goal":{"type":"string","minLength":1},"context":{"type":"string"},
                    "constraints":{"type":"string"},"acceptance_criteria":{"type":"string"}}},
            "idempotency_key":{"type":"string","minLength":1,"maxLength":256}}})
}
