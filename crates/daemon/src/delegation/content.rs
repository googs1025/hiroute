//! Run output uses MVP-15's body store and its durable visibility barriers.
//! All calls perform body I/O outside the shared admission gate. Scope and timestamps
//! come from committed run records, never an external request or the Worker environment.
use hiroute_domain::delegation::DelegationErrorV1;
use hiroute_observation::LocalObservationStore;
use hiroute_observation::managed_text::{
    CHUNK_BYTES, ManagedTextInput, ManagedTextPurpose, ManagedTextRef, ManagedTextScope,
    ManagedTextState, PAGE_BYTES,
};
use std::sync::Arc;

pub struct RunBodyWriter {
    store: Arc<LocalObservationStore>,
    reference: ManagedTextRef,
    next_chunk: u64,
    failed: bool,
}

impl RunBodyWriter {
    pub fn create(
        store: Arc<LocalObservationStore>,
        trusted_scope: ManagedTextScope,
        purpose: ManagedTextPurpose,
        stable_event_id: String,
        original_created_at_ms: i64,
        now_ms: i64,
    ) -> Result<Self, DelegationErrorV1> {
        let reference = store
            .managed_text_put(
                &ManagedTextInput {
                    scope: trusted_scope,
                    purpose,
                    source_event_id: stable_event_id,
                    source_revision: 1,
                    original_created_at_ms,
                    import_origin: None,
                },
                now_ms,
            )
            .map_err(|_| DelegationErrorV1::ContentUnavailable)?;
        if reference.state != ManagedTextState::Pending {
            return Err(DelegationErrorV1::ContentUnavailable);
        }
        Ok(Self {
            store,
            reference,
            next_chunk: 0,
            failed: false,
        })
    }

    /// One writer per stream. Failed writes permanently stop this writer: retrying a
    /// different notification at the same ordinal could otherwise corrupt event identity.
    pub fn append(&mut self, bytes: &[u8], now_ms: i64) -> Result<(), DelegationErrorV1> {
        if self.failed || bytes.len() > PAGE_BYTES {
            self.failed = true;
            return Err(DelegationErrorV1::ContentUnavailable);
        }
        for chunk in bytes.chunks(CHUNK_BYTES) {
            if self
                .store
                .managed_text_append(
                    &self.reference.scope,
                    &self.reference,
                    self.next_chunk,
                    chunk,
                    now_ms,
                )
                .is_err()
            {
                self.failed = true;
                return Err(DelegationErrorV1::ContentUnavailable);
            }
            self.next_chunk += 1;
        }
        Ok(())
    }

    /// Call only after a protocol completion whose output stream is complete. A run's
    /// execution success remains separate from body availability and user feedback.
    pub fn finish(self, now_ms: i64) -> Result<ManagedTextRef, DelegationErrorV1> {
        if self.failed {
            return Err(DelegationErrorV1::ContentUnavailable);
        }
        self.store
            .managed_text_finish(
                &self.reference.scope,
                &self.reference,
                self.next_chunk,
                now_ms,
            )
            .map_err(|_| DelegationErrorV1::ContentUnavailable)
    }

    pub fn reference(&self) -> &ManagedTextRef {
        &self.reference
    }
}

/// Caller resolves the old scope through the currently authorized task and exact-version
/// association. A BodyRef's serialized scope/Complete flag is not authorization or evidence
/// that its files still exist. Recheck content authority for every page and before delivery.
pub fn read_required_body(
    store: &LocalObservationStore,
    trusted_original_scope: &ManagedTextScope,
    reference: &ManagedTextRef,
    now_ms: i64,
    max_bytes: usize,
    mut authorize_current: impl FnMut(&ManagedTextScope) -> Result<(), DelegationErrorV1>,
) -> Result<Vec<u8>, DelegationErrorV1> {
    if reference.scope != *trusted_original_scope || max_bytes == 0 || max_bytes > 16 * PAGE_BYTES {
        return Err(DelegationErrorV1::ResumeUnavailable);
    }
    let mut first = 0;
    let mut body = Vec::new();
    loop {
        authorize_current(trusted_original_scope)?;
        let page = store
            .managed_text_read(
                trusted_original_scope,
                reference,
                first,
                PAGE_BYTES / CHUNK_BYTES,
                now_ms,
            )
            .map_err(|_| DelegationErrorV1::ResumeUnavailable)?;
        if page.reference.state != ManagedTextState::Complete
            || body.len().saturating_add(page.bytes.len()) > max_bytes
        {
            return Err(DelegationErrorV1::ResumeUnavailable);
        }
        body.extend(page.bytes);
        match page.next_chunk {
            Some(next) if next > first => first = next,
            Some(_) => return Err(DelegationErrorV1::ResumeUnavailable),
            None => break,
        }
    }
    authorize_current(trusted_original_scope)?;
    let current = store
        .managed_text_resolve(trusted_original_scope, reference, now_ms)
        .map_err(|_| DelegationErrorV1::ResumeUnavailable)?;
    if current.state != ManagedTextState::Complete
        || current.visibility_generation != reference.visibility_generation
    {
        return Err(DelegationErrorV1::ResumeUnavailable);
    }
    Ok(body)
}

#[cfg(test)]
mod tests;
