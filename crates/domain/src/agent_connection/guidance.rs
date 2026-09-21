use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CanonicalDigest, RoutingInstructionOverlayV1, SpawnGuidanceProfileV1};

/// Product-level context projection proving that the HIRoute overlay is one independent typed
/// developer/system layer. Installing it never rewrites caller messages, instructions, tools,
/// arguments, or attachments.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInvocationContextV1 {
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub instructions: Vec<Value>,
    #[serde(default)]
    pub tools: Vec<FunctionToolV1>,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub attachments: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hiroute_developer_overlay: Option<RoutingInstructionOverlayV1>,
}

impl AgentInvocationContextV1 {
    pub fn install_routing_overlay(&mut self, overlay: RoutingInstructionOverlayV1) {
        self.hiroute_developer_overlay = Some(overlay);
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionToolV1 {
    pub registration_id: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub parameters: Value,
}

impl FunctionToolV1 {
    pub fn shape_digest(&self) -> Result<CanonicalDigest, serde_json::Error> {
        #[derive(Serialize)]
        struct Shape<'a> {
            registration_id: &'a str,
            name: &'a str,
            kind: &'a str,
            parameters: &'a Value,
        }
        CanonicalDigest::of(&Shape {
            registration_id: &self.registration_id,
            name: &self.name,
            kind: &self.kind,
            parameters: &self.parameters,
        })
        .map_err(|error| match error {
            crate::CanonicalDigestError::Serialization(error) => error,
            crate::CanonicalDigestError::InvalidFormat => {
                unreachable!("shape digest is generated, not parsed")
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceRewriteDispositionV1 {
    Rewritten,
    AlreadyCurrent,
    NoRegisteredTool,
    AmbiguousRegisteredTool,
    ShapeMismatch,
    MarkerMismatch,
    UnsafeGuidance,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GuidanceRewriteResultV1 {
    pub disposition: GuidanceRewriteDispositionV1,
    pub tools: Vec<FunctionToolV1>,
}

pub fn rewrite_spawn_guidance(
    profile: &SpawnGuidanceProfileV1,
    tools: &[FunctionToolV1],
    guidance: &str,
) -> GuidanceRewriteResultV1 {
    if guidance.len() > crate::MAX_ROUTING_OVERLAY_BYTES
        || guidance.contains(&profile.begin_marker)
        || guidance.contains(&profile.end_marker)
        || guidance.chars().any(char::is_control)
    {
        return result(GuidanceRewriteDispositionV1::UnsafeGuidance, tools);
    }
    let candidates = tools
        .iter()
        .enumerate()
        .filter(|(_, tool)| {
            tool.registration_id == profile.registration_id
                && tool.name == profile.tool_name
                && tool.kind == "function"
        })
        .collect::<Vec<_>>();
    let disposition = match candidates.as_slice() {
        [] => GuidanceRewriteDispositionV1::NoRegisteredTool,
        [(_, tool)] => match tool.shape_digest() {
            Ok(digest) if digest == profile.expected_shape_digest => {
                let begin_count = tool.description.matches(&profile.begin_marker).count();
                let end_count = tool.description.matches(&profile.end_marker).count();
                if begin_count != 1 || end_count != 1 {
                    GuidanceRewriteDispositionV1::MarkerMismatch
                } else {
                    let Some(begin) = tool.description.find(&profile.begin_marker) else {
                        return result(GuidanceRewriteDispositionV1::MarkerMismatch, tools);
                    };
                    let body_start = begin + profile.begin_marker.len();
                    let Some(relative_end) =
                        tool.description[body_start..].find(&profile.end_marker)
                    else {
                        return result(GuidanceRewriteDispositionV1::MarkerMismatch, tools);
                    };
                    let end = body_start + relative_end;
                    if body_start > end {
                        GuidanceRewriteDispositionV1::MarkerMismatch
                    } else {
                        let wanted = format!("\n{guidance}\n\n");
                        if tool.description[body_start..end] == wanted {
                            GuidanceRewriteDispositionV1::AlreadyCurrent
                        } else {
                            GuidanceRewriteDispositionV1::Rewritten
                        }
                    }
                }
            }
            _ => GuidanceRewriteDispositionV1::ShapeMismatch,
        },
        _ => GuidanceRewriteDispositionV1::AmbiguousRegisteredTool,
    };
    if disposition != GuidanceRewriteDispositionV1::Rewritten {
        return result(disposition, tools);
    }

    let mut rewritten = tools.to_vec();
    let index = candidates[0].0;
    let description = &rewritten[index].description;
    let begin = description
        .find(&profile.begin_marker)
        .expect("validated exact marker");
    let body_start = begin + profile.begin_marker.len();
    let end = body_start
        + description[body_start..]
            .find(&profile.end_marker)
            .expect("validated exact end marker");
    rewritten[index].description = format!(
        "{}\n{guidance}\n\n{}",
        &description[..body_start],
        &description[end..]
    );
    GuidanceRewriteResultV1 {
        disposition,
        tools: rewritten,
    }
}

fn result(
    disposition: GuidanceRewriteDispositionV1,
    tools: &[FunctionToolV1],
) -> GuidanceRewriteResultV1 {
    GuidanceRewriteResultV1 {
        disposition,
        tools: tools.to_vec(),
    }
}
