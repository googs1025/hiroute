//! Branch-local exact MVP-20 permit SQL; final migration numbering belongs to convergence.
pub(crate) const CONTROL: &str = r#"
CREATE TABLE delegation_permits (
 workspace_id TEXT NOT NULL, permit_id TEXT NOT NULL, record_json TEXT NOT NULL,
 PRIMARY KEY(workspace_id, permit_id)
);
CREATE TABLE delegation_permit_operations (
 operation_id TEXT NOT NULL REFERENCES operations(operation_id), workspace_id TEXT NOT NULL,
 permit_id TEXT NOT NULL, checkpoint_json TEXT NOT NULL, applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0,1)), PRIMARY KEY(operation_id,permit_id)
);
"#;
