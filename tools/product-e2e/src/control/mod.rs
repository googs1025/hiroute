use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioState {
    Green,
    ExpectedRed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlShellScenarioV1 {
    pub schema: String,
    pub process: String,
    pub scenario_id: String,
    pub command_states: Vec<ControlCommandStateV1>,
    pub gateway_bound_state: ScenarioState,
    pub gateway_bound_evidence: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlCommandStateV1 {
    pub command_id: String,
    pub state: ScenarioState,
    pub expected_exit: u8,
    pub expected_status: String,
    pub expected_error_code: Option<String>,
}

impl ControlShellScenarioV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema != "hiroute.control-shell-scenario/v1"
            || self.process != "PROCESS-25016"
            || self.command_states.is_empty()
            || self.gateway_bound_state != ScenarioState::Green
            || self.gateway_bound_evidence != "installed-standalone-headless-management-loop"
        {
            return Err("invalid control-shell scenario boundary");
        }
        if self.command_states.iter().any(|state| {
            state.command_id.is_empty()
                || state.expected_status.is_empty()
                || state.state != ScenarioState::Green
                || match state.expected_status.as_str() {
                    "succeeded" => state.expected_exit != 0 || state.expected_error_code.is_some(),
                    "usage_error" => {
                        state.expected_exit != 2
                            || state.expected_error_code.as_deref() != Some("UNKNOWN_COMMAND")
                    }
                    "not_found" => {
                        state.expected_exit != 5
                            || state.expected_error_code.as_deref() != Some("RESOURCE_NOT_FOUND")
                    }
                    _ => true,
                }
        }) {
            return Err("control-shell command state is incomplete");
        }
        Ok(())
    }
}
