use super::*;

pub struct TelemetryProjector;

impl TelemetryProjector {
    pub fn publication(
        correlation: Correlation,
        fact: PublicationFact,
        now_nanos: u64,
    ) -> LifecycleEvent {
        LifecycleEvent {
            correlation,
            monotonic_nanos: now_nanos,
            kind: LifecycleKind::Publication(fact),
        }
    }

    pub fn attempt(
        correlation: Correlation,
        snapshot: AttemptSnapshot,
        now_nanos: u64,
    ) -> Vec<LifecycleEvent> {
        let events = vec![
            commit_event(
                correlation.clone(),
                now_nanos,
                FenceKind::UpstreamAttemptRequest,
                snapshot.upstream_request_fence,
                snapshot.connection_sub_attempts,
            ),
            commit_event(
                correlation.clone(),
                now_nanos,
                FenceKind::DownstreamFinalHeaders,
                snapshot.downstream_header_fence,
                snapshot.connection_sub_attempts,
            ),
            commit_event(
                correlation.clone(),
                now_nanos,
                FenceKind::DownstreamSemanticOutput,
                snapshot.downstream_semantic_fence,
                snapshot.connection_sub_attempts,
            ),
        ];
        events
    }

    /// Emits every semantic role, including Native retry at explicit zero.
    pub fn stream_memory(
        correlation: Correlation,
        snapshot: StreamBudgetSnapshot,
        now_nanos: u64,
    ) -> Vec<LifecycleEvent> {
        MemoryRole::ALL
            .iter()
            .copied()
            .map(|role| LifecycleEvent {
                correlation: correlation.clone(),
                monotonic_nanos: now_nanos,
                kind: LifecycleKind::Memory(MemoryFact {
                    level: BudgetLevel::Stream,
                    role,
                    live_bytes: snapshot.role_live[role as usize],
                    peak_bytes: snapshot.role_peak[role as usize],
                    rejections: snapshot.rejected,
                    queue_high_water_bytes: snapshot.role_peak[role as usize],
                }),
            })
            .collect()
    }

    pub fn executor(
        correlation: Correlation,
        kind: ExecutorKind,
        snapshot: ExecutorSnapshot,
        now_nanos: u64,
    ) -> Vec<LifecycleEvent> {
        let facts = [
            (ExecutorOutcome::Admitted, snapshot.admitted),
            (ExecutorOutcome::Overloaded, snapshot.overloaded),
            (ExecutorOutcome::Cancelled, snapshot.cancelled),
            (ExecutorOutcome::Panicked, snapshot.panicked),
            (ExecutorOutcome::LateResultDropped, snapshot.late_dropped),
        ];
        facts
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(outcome, _)| LifecycleEvent {
                correlation: correlation.clone(),
                monotonic_nanos: now_nanos,
                kind: LifecycleKind::Executor(ExecutorFact {
                    executor: kind,
                    outcome,
                    queue_depth: snapshot.queued,
                    queue_wait_micros: 0,
                    concurrency: snapshot.running,
                }),
            })
            .collect()
    }
}

fn commit_event(
    correlation: Correlation,
    monotonic_nanos: u64,
    fence: FenceKind,
    state: CommitFence,
    connection_sub_attempt: usize,
) -> LifecycleEvent {
    LifecycleEvent {
        correlation,
        monotonic_nanos,
        kind: LifecycleKind::Commit(CommitFact {
            fence,
            state,
            connection_sub_attempt,
        }),
    }
}
