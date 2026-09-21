use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestState {
    Accepted,
    PublicationBound,
    LogicalHeaders,
    LogicalBody,
    Selecting,
    AttemptActive,
    DispositionPending,
    FinalResponseSelected,
    AcceptedResponseActive,
    Cancelling,
    Completed,
}

#[derive(Debug)]
pub struct LogicalRequestDriver {
    state: RequestState,
    scopes: ScopeSupervisor,
    logical: Option<ScopeGuard>,
    attempt: Option<ScopeGuard>,
    accepted: Option<ScopeGuard>,
    telemetry: Option<RequestTelemetry>,
}

impl LogicalRequestDriver {
    pub fn new(scopes: ScopeSupervisor) -> Result<Self, DriverError> {
        let logical = scopes.begin_logical()?;
        Ok(Self {
            state: RequestState::Accepted,
            scopes,
            logical: Some(logical),
            attempt: None,
            accepted: None,
            telemetry: None,
        })
    }

    pub fn with_telemetry(mut self, telemetry: RequestTelemetry) -> Self {
        if let Some(logical) = &self.logical {
            telemetry.scope(
                ScopeKind::LogicalRequest,
                logical.id(),
                ScopePhase::Headers,
                0,
                Duration::ZERO,
            );
        }
        self.telemetry = Some(telemetry);
        self
    }

    pub fn state(&self) -> RequestState {
        self.state
    }

    pub fn bind_publication(&mut self) -> Result<(), DriverError> {
        self.transition(RequestState::Accepted, RequestState::PublicationBound)
    }

    pub fn finish_logical_filters(&mut self) -> Result<(), DriverError> {
        if !matches!(
            self.state,
            RequestState::PublicationBound
                | RequestState::LogicalHeaders
                | RequestState::LogicalBody
        ) {
            return Err(DriverError::InvalidTransition);
        }
        self.state = RequestState::Selecting;
        Ok(())
    }

    pub fn begin_attempt(&mut self) -> Result<(), DriverError> {
        if self.state != RequestState::Selecting || self.attempt.is_some() {
            return Err(DriverError::InvalidTransition);
        }
        self.attempt = Some(self.scopes.begin_attempt()?);
        self.state = RequestState::AttemptActive;
        self.observe_scope(ScopeKind::RouteAttempt, ScopePhase::Headers, 0);
        Ok(())
    }

    pub fn disposition_pending(&mut self) -> Result<(), DriverError> {
        self.transition(
            RequestState::AttemptActive,
            RequestState::DispositionPending,
        )?;
        self.observe_scope(ScopeKind::RouteAttempt, ScopePhase::Paused, 0);
        Ok(())
    }

    pub fn continue_after_attempt(&mut self) -> Result<(), DriverError> {
        if self.state != RequestState::DispositionPending {
            return Err(DriverError::InvalidTransition);
        }
        self.observe_scope(ScopeKind::RouteAttempt, ScopePhase::Finalized, 1);
        self.attempt.take();
        self.state = RequestState::Selecting;
        Ok(())
    }

    pub fn select_final_response(&mut self) -> Result<(), DriverError> {
        if self.state != RequestState::DispositionPending {
            return Err(DriverError::InvalidTransition);
        }
        self.observe_scope(ScopeKind::RouteAttempt, ScopePhase::Finalized, 1);
        self.attempt.take();
        self.state = RequestState::FinalResponseSelected;
        Ok(())
    }

    pub fn select_local_response(&mut self) -> Result<(), DriverError> {
        if self.attempt.is_some()
            || !matches!(
                self.state,
                RequestState::PublicationBound
                    | RequestState::LogicalHeaders
                    | RequestState::LogicalBody
                    | RequestState::Selecting
            )
        {
            return Err(DriverError::InvalidTransition);
        }
        self.state = RequestState::FinalResponseSelected;
        Ok(())
    }

    pub fn begin_accepted_response(&mut self) -> Result<(), DriverError> {
        if self.state != RequestState::FinalResponseSelected {
            return Err(DriverError::InvalidTransition);
        }
        self.accepted = Some(self.scopes.begin_accepted()?);
        self.state = RequestState::AcceptedResponseActive;
        self.observe_scope(ScopeKind::AcceptedResponse, ScopePhase::Headers, 0);
        Ok(())
    }

    pub fn complete(&mut self) {
        if self.attempt.is_some() {
            self.observe_scope(ScopeKind::RouteAttempt, ScopePhase::Finalized, 1);
        }
        if self.accepted.is_some() {
            self.observe_scope(ScopeKind::AcceptedResponse, ScopePhase::Finalized, 1);
        }
        if self.logical.is_some() {
            self.observe_scope(ScopeKind::LogicalRequest, ScopePhase::Finalized, 1);
        }
        self.attempt.take();
        self.accepted.take();
        self.logical.take();
        self.state = RequestState::Completed;
    }

    fn transition(&mut self, from: RequestState, to: RequestState) -> Result<(), DriverError> {
        if self.state != from {
            return Err(DriverError::InvalidTransition);
        }
        self.state = to;
        Ok(())
    }

    pub fn scope_id(&self, kind: ScopeKind) -> Option<ScopeId> {
        match kind {
            ScopeKind::LogicalRequest => self.logical.as_ref(),
            ScopeKind::RouteAttempt => self.attempt.as_ref(),
            ScopeKind::AcceptedResponse => self.accepted.as_ref(),
        }
        .map(ScopeGuard::id)
    }

    fn observe_scope(&self, kind: ScopeKind, phase: ScopePhase, finalize_count: usize) {
        if let (Some(telemetry), Some(scope_id)) = (&self.telemetry, self.scope_id(kind)) {
            telemetry.scope(kind, scope_id, phase, finalize_count, Duration::ZERO);
        }
    }
}

impl Drop for LogicalRequestDriver {
    fn drop(&mut self) {
        self.complete();
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DriverError {
    #[error("request state transition is invalid")]
    InvalidTransition,
    #[error(transparent)]
    Scope(#[from] ScopeError),
}
