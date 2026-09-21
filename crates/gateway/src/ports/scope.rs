use std::future::Future;
use std::time::Instant;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// One absolute deadline and cancellation source shared by every runtime
/// state, Secret, DNS, upstream and downstream operation for a request.
#[derive(Clone)]
pub struct ExecutionScope {
    deadline: Instant,
    cancellation: CancellationToken,
}

impl ExecutionScope {
    pub fn new(deadline: Instant, cancellation: CancellationToken) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn ensure_active(&self) -> Result<(), ScopeError> {
        if self.cancellation.is_cancelled() {
            Err(ScopeError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(ScopeError::Deadline)
        } else {
            Ok(())
        }
    }

    /// Enforces the shared scope even when an adapter forgets to enforce the
    /// deadline internally. Dropping the adapter future is the cancellation
    /// boundary required by every runtime port.
    pub async fn run<F, T>(&self, operation: F) -> Result<T, ScopeError>
    where
        F: Future<Output = T>,
    {
        self.ensure_active()?;
        let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline));
        tokio::pin!(sleep);
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(ScopeError::Cancelled),
            _ = &mut sleep => Err(ScopeError::Deadline),
            value = operation => Ok(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ScopeError {
    #[error("request cancellation was observed")]
    Cancelled,
    #[error("absolute request deadline was exceeded")]
    Deadline,
}
