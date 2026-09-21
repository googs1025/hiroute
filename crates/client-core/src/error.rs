use serde::Serialize;

/// Local failures never pretend to be a daemon business result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    LocatorUnavailable,
    TransportUnavailable,
    Deadline,
    PeerRejected,
    SchemaIncompatible,
    FrameInvalid,
    FrameTooLarge,
    ResponseMismatch,
    ProtectedInputUnavailable,
    ObservationStopped,
    UnsupportedPlatform,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionState {
    NotSent,
    MayHaveReachedServer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ClientFailure {
    pub code: FailureCode,
    pub submission: SubmissionState,
}

impl ClientFailure {
    pub const fn before_send(code: FailureCode) -> Self {
        Self {
            code,
            submission: SubmissionState::NotSent,
        }
    }

    pub(crate) const fn at(code: FailureCode, sent: bool) -> Self {
        Self {
            code,
            submission: if sent {
                SubmissionState::MayHaveReachedServer
            } else {
                SubmissionState::NotSent
            },
        }
    }
}

impl std::fmt::Display for ClientFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No transport error text, paths, payloads or authorization material.
        write!(f, "client.{:?} ({:?})", self.code, self.submission)
    }
}
impl std::error::Error for ClientFailure {}
