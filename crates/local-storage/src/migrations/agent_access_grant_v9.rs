pub(crate) const SECRETS_V9: &str = r#"
CREATE TABLE agent_access_grant_versions (
    connection_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    grant_id TEXT NOT NULL,
    owner_scope TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    scope_hash TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    nonce BLOB NOT NULL,
    aad_schema TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    material_sha256 TEXT NOT NULL,
    owner_operation_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(connection_id, generation),
    UNIQUE(grant_id, generation)
);

CREATE TABLE agent_access_grant_heads (
    connection_id TEXT PRIMARY KEY,
    owner_scope TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation >= 0),
    active_version_generation INTEGER,
    owner_operation_id TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK(active_version_generation IS NULL OR active_version_generation <= generation)
);

CREATE TABLE agent_access_grant_effects (
    operation_id TEXT NOT NULL,
    connection_id TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('create', 'reuse', 'rotate', 'revoke', 'revoke_noop')),
    owner_scope TEXT NOT NULL,
    before_generation INTEGER NOT NULL CHECK(before_generation >= 0),
    before_active_version_generation INTEGER,
    before_owner_operation_id TEXT,
    after_generation INTEGER NOT NULL CHECK(after_generation >= 0),
    after_active_version_generation INTEGER,
    after_grant_id TEXT,
    after_scope_hash TEXT,
    after_material_sha256 TEXT,
    compensated INTEGER NOT NULL DEFAULT 0,
    activated INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(operation_id, connection_id),
    CHECK(before_active_version_generation IS NULL OR before_active_version_generation <= before_generation),
    CHECK(after_active_version_generation IS NULL OR after_active_version_generation <= after_generation)
);

CREATE INDEX agent_access_grant_effects_recovery_idx
    ON agent_access_grant_effects(compensated, activated, operation_id);
CREATE INDEX agent_access_grant_versions_owner_idx
    ON agent_access_grant_versions(owner_scope, connection_id, generation);
"#;
