//! Branch-local version 10 intent. Final ordering with Plan migrations belongs to convergence.
pub(crate) const CONTROL: &str = r#"
CREATE TABLE agent_collaboration_grants (
    workspace_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    context_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    grant_json TEXT NOT NULL,
    owner_operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    PRIMARY KEY(workspace_id, grant_id),
    UNIQUE(workspace_id, context_id)
);
CREATE TABLE agent_collaboration_revocations (
    workspace_id TEXT NOT NULL,
    operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    grant_id TEXT NOT NULL,
    through_generation INTEGER NOT NULL CHECK(through_generation > 0),
    checkpoint_json TEXT NOT NULL,
    cancel_receipt_ref TEXT,
    cleanup_complete INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_complete IN (0,1)),
    PRIMARY KEY(operation_id, grant_id)
);
CREATE INDEX agent_collaboration_revocations_recovery
    ON agent_collaboration_revocations(workspace_id, grant_id, through_generation);
CREATE TABLE agent_skill_installations (
    workspace_id TEXT NOT NULL,
    root_ref TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    record_json TEXT NOT NULL,
    owner_operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    PRIMARY KEY(workspace_id, root_ref)
);
"#;
pub(crate) const SECRETS: &str = r#"
CREATE TABLE agent_collaboration_credentials (
    workspace_id TEXT NOT NULL,
    context_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    owner_operation_id TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    nonce BLOB NOT NULL,
    aad_schema TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    PRIMARY KEY(workspace_id, grant_id, generation),
    UNIQUE(owner_operation_id, grant_id)
);
"#;
pub(crate) const RUNTIME: &str = "-- Agent settings do not own Worker runtime tables.";
