use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UpstreamSemanticUse {
    pub connections: usize,
    pub headers: usize,
    pub body_bytes: usize,
    pub semantic_bytes: usize,
}

impl UpstreamSemanticUse {
    pub fn is_clear(self) -> bool {
        self.headers == 0 && self.body_bytes == 0 && self.semantic_bytes == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoutedLocalReply {
    TerminalLocalResponse(LocalReply),
    AttemptResponseCandidate {
        response: LocalReply,
        upstream_side_effects: bool,
    },
    AcceptedReplacement(LocalReply),
}

pub fn route_local_reply(
    scope: ScopeKind,
    reply: LocalReply,
    usage: UpstreamSemanticUse,
) -> RoutedLocalReply {
    match scope {
        ScopeKind::LogicalRequest => RoutedLocalReply::TerminalLocalResponse(reply),
        ScopeKind::RouteAttempt => RoutedLocalReply::AttemptResponseCandidate {
            response: reply,
            upstream_side_effects: !usage.is_clear(),
        },
        ScopeKind::AcceptedResponse => RoutedLocalReply::AcceptedReplacement(reply),
    }
}
