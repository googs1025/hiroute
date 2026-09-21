pub(super) const CONTROL: &str = "
CREATE TABLE IF NOT EXISTS compute_subscription_validations (
    operation_id TEXT PRIMARY KEY REFERENCES operations(operation_id),
    candidate_ref TEXT NOT NULL,
    candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
    record_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('staged','verified','retained','released')),
    save_operation_id TEXT,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS compute_subscription_validations_candidate_idx
    ON compute_subscription_validations(candidate_ref, candidate_revision, updated_at);
";
