use std::path::Path;

use hiroute_domain::CanonicalDigest;

use crate::errors::CpaLifecycleError;

use super::{OwnedNestedSource, SourceStamp, account_digest, path_digest, revision_digest};

/// Stable evidence for one safely read Codex account. It deliberately has no serde wire shape.
#[derive(Clone, Eq, PartialEq)]
pub struct BorrowedCodexEvidence {
    source_path_digest: String,
    account_digest: String,
    revision_digest: String,
    stamp: SourceStamp,
    evidence_digest: CanonicalDigest,
    binding_evidence_digest: CanonicalDigest,
}

impl std::fmt::Debug for BorrowedCodexEvidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BorrowedCodexEvidence")
            .field("source", &"<redacted>")
            .field("account", &"<redacted>")
            .field("evidence_digest", &self.evidence_digest)
            .finish()
    }
}

impl BorrowedCodexEvidence {
    pub(super) fn from_source(
        canonical_source: &Path,
        source: &OwnedNestedSource,
    ) -> Result<Self, CpaLifecycleError> {
        let source_path_digest = path_digest(canonical_source);
        let account_digest = account_digest(source.account_id.as_str());
        let revision_digest = revision_digest(source);
        let evidence_digest = CanonicalDigest::of(&(
            "hiroute.borrowed-codex-evidence/v1",
            source_path_digest.as_str(),
            account_digest.as_str(),
            revision_digest.as_str(),
            source.stamp.len,
            source.stamp.mtime_secs,
            source.stamp.mtime_nanos,
        ))
        .map_err(|_| CpaLifecycleError::InvalidBorrowedCodexAuth)?;
        let binding_evidence_digest = CanonicalDigest::of(&(
            "hiroute.borrowed-codex-binding-evidence/v1",
            source_path_digest.as_str(),
            account_digest.as_str(),
        ))
        .map_err(|_| CpaLifecycleError::InvalidBorrowedCodexAuth)?;
        Ok(Self {
            source_path_digest,
            account_digest,
            revision_digest,
            stamp: source.stamp,
            evidence_digest,
            binding_evidence_digest,
        })
    }

    pub fn account_ref(&self) -> String {
        format!("account/cpa/{}", self.account_digest)
    }

    pub fn evidence_digest(&self) -> &CanonicalDigest {
        &self.evidence_digest
    }

    /// Stable audit identity for the trusted native context and current personal account. Token
    /// revisions remain represented only by the private exact evidence above.
    pub fn binding_evidence_digest(&self) -> &CanonicalDigest {
        &self.binding_evidence_digest
    }

    pub(super) fn source_path_digest(&self) -> &str {
        &self.source_path_digest
    }

    pub(super) fn matches(&self, other: &Self) -> bool {
        self.source_path_digest == other.source_path_digest
            && self.account_digest == other.account_digest
            && self.revision_digest == other.revision_digest
            && self.stamp == other.stamp
            && self.evidence_digest == other.evidence_digest
    }
}
