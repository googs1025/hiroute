use serde_json::Value;

use super::{CandidateProtocolProfile, CapabilityError, NativeReasoningValue};
use crate::server::core_runtime::model_ir::{ModelRequestIRV1, RequestedReasoningDisposition};
use crate::server::request_plan::IngressProtocol;

#[cfg(test)]
#[path = "fixed_reasoning_tests.rs"]
mod tests;

type RequestedControl<'a> = (Option<&'a str>, Option<&'a serde_json::Map<String, Value>>);

impl CandidateProtocolProfile {
    pub(crate) fn for_request_reasoning(
        &self,
        request: &ModelRequestIRV1,
    ) -> Result<Self, CapabilityError> {
        if request.requested_reasoning.disposition
            != RequestedReasoningDisposition::AppliedToFixedBinding
        {
            return Ok(self.clone());
        }
        let digest = request
            .requested_reasoning
            .fixed_profile_digest
            .as_ref()
            .ok_or(CapabilityError::ReasoningProfileMismatch)?;
        let selected = self
            .capability
            .reasoning_profiles
            .iter()
            .find(|profile| {
                hiroute_domain::CanonicalDigest::of(profile).as_ref().ok() == Some(digest)
            })
            .ok_or(CapabilityError::ReasoningProfileMismatch)?;
        if !selected.validate_for(self.capability.upstream_protocol) {
            return Err(CapabilityError::ReasoningProfileMismatch);
        }
        let mut profile = self.clone();
        profile.capability.selected_reasoning_profile_id = selected.profile_id.clone();
        Ok(profile)
    }

    pub(crate) fn select_native_reasoning(
        &self,
        native: &Value,
        ingress: IngressProtocol,
    ) -> Result<Self, CapabilityError> {
        let (effort, thinking) = requested_control(native, ingress)?;
        if effort.is_none() && thinking.is_none() {
            return Ok(self.clone());
        }
        if thinking.is_some() && self.capability.upstream_protocol != IngressProtocol::Messages {
            return Err(CapabilityError::ReasoningProfileMismatch);
        }
        let effort_path: &[&str] = match self.capability.upstream_protocol {
            IngressProtocol::Responses => &["reasoning", "effort"],
            IngressProtocol::ChatCompletions => &["reasoning_effort"],
            IngressProtocol::Messages => &["output_config", "effort"],
        };
        let mut matching = self.capability.reasoning_profiles.iter().filter(|profile| {
            effort.is_none_or(|effort| {
                profile.render.fields().iter().any(|field| {
                    field
                        .path
                        .iter()
                        .map(String::as_str)
                        .eq(effort_path.iter().copied())
                        && field.value == NativeReasoningValue::String(effort.to_owned())
                })
            }) && thinking.is_none_or(|thinking| {
                thinking.iter().all(|(key, expected)| {
                    profile.render.fields().iter().any(|field| {
                        field.path == ["thinking", key.as_str()]
                            && match &field.value {
                                NativeReasoningValue::Bool(value) => {
                                    expected.as_bool() == Some(*value)
                                }
                                NativeReasoningValue::String(value) => {
                                    expected.as_str() == Some(value.as_str())
                                }
                                NativeReasoningValue::U64(value) => {
                                    expected.as_u64() == Some(*value)
                                }
                            }
                    })
                })
            })
        });
        let selected = matching
            .next()
            .ok_or(CapabilityError::ReasoningProfileMismatch)?;
        if matching.next().is_some() || !selected.validate_for(self.capability.upstream_protocol) {
            return Err(CapabilityError::ReasoningProfileMismatch);
        }
        let mut profile = self.clone();
        profile.capability.selected_reasoning_profile_id = selected.profile_id.clone();
        Ok(profile)
    }
}

fn requested_control(
    native: &Value,
    protocol: IngressProtocol,
) -> Result<RequestedControl<'_>, CapabilityError> {
    let (effort, thinking) = match protocol {
        IngressProtocol::Responses => {
            let object = native
                .as_object()
                .ok_or(CapabilityError::ReasoningProfileMismatch)?;
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "effort" | "summary" | "context"))
            {
                return Err(CapabilityError::ReasoningProfileMismatch);
            }
            if object.get("context").is_some_and(|value| {
                !matches!(value.as_str(), Some("auto" | "current_turn" | "all_turns"))
            }) {
                return Err(CapabilityError::ReasoningProfileMismatch);
            }
            (object.get("effort"), None)
        }
        IngressProtocol::ChatCompletions => (Some(native), None),
        IngressProtocol::Messages => {
            let object = native
                .as_object()
                .ok_or(CapabilityError::ReasoningProfileMismatch)?;
            let output = object
                .get("output_config")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_object()
                        .ok_or(CapabilityError::ReasoningProfileMismatch)
                })
                .transpose()?;
            if output.is_some_and(|output| output.keys().any(|key| key != "effort")) {
                return Err(CapabilityError::ReasoningProfileMismatch);
            }
            let thinking = object
                .get("thinking")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_object()
                        .ok_or(CapabilityError::ReasoningProfileMismatch)
                })
                .transpose()?;
            if thinking.is_some_and(|thinking| thinking.is_empty()) {
                return Err(CapabilityError::ReasoningProfileMismatch);
            }
            (output.and_then(|output| output.get("effort")), thinking)
        }
    };
    let effort = effort
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or(CapabilityError::ReasoningProfileMismatch)
        })
        .transpose()?;
    Ok((effort, thinking))
}
