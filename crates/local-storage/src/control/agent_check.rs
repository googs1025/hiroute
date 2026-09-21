use super::*;

impl ControlStore {
    pub fn consume_agent_live_check_capability(
        &self,
        capability: &ProtectedApplyCapability,
        workspace: &WorkspaceId,
        principal: &str,
        accepted_digest: &CanonicalDigest,
        expected_revisions: &RevisionSetV1,
    ) -> PortResult<()> {
        if !matches!(principal, "desktop" | "interactive-user") {
            return Err(port(
                PortErrorCode::PermissionDenied,
                "agent-check.principal",
            ));
        }
        let capability_digest = CanonicalDigest::of_bytes(capability.expose());
        let revisions_digest = CanonicalDigest::of(expected_revisions)
            .map_err(|_| port(PortErrorCode::InvalidData, "agent-check.revisions"))?;
        let mut connection = self.connection.borrow_mut();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| port(PortErrorCode::Unavailable, "agent-check.begin"))?;
        if read_revisions(&transaction, workspace)? != *expected_revisions {
            return Err(port(
                PortErrorCode::Conflict,
                "agent-check.revision-changed",
            ));
        }
        // A Live check has no configuration Operation: retire its token before any billed work.
        let consumed = transaction
            .execute(
                "UPDATE apply_capabilities SET revoked = 1
                 WHERE capability_digest = ?1 AND workspace_id = ?2 AND principal = ?3
                   AND operation_kind = 'CheckAgentConnection'
                   AND accepted_digest = ?4 AND expected_revisions_digest = ?5
                   AND capability_scope = 'apply:one-shot' AND expires_at > unixepoch()
                   AND revoked = 0 AND consumed_operation_id IS NULL",
                params![
                    capability_digest.as_str(),
                    workspace.as_str(),
                    principal,
                    accepted_digest.as_str(),
                    revisions_digest.as_str(),
                ],
            )
            .map_err(|_| port(PortErrorCode::Unavailable, "agent-check.consume"))?;
        if consumed != 1 {
            return Err(port(PortErrorCode::PermissionDenied, "agent-check.denied"));
        }
        transaction
            .commit()
            .map_err(|_| port(PortErrorCode::Unavailable, "agent-check.commit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register(control: &ControlStore, raw: &str, operation: &str, expires_at: i64) {
        control
            .insert_apply_capability(
                &ProtectedApplyCapability::new(raw.to_owned()).unwrap(),
                "desktop",
                &WorkspaceId::default(),
                operation,
                &CanonicalDigest::of_bytes(b"live-target"),
                &RevisionSetV1 {
                    target: 0,
                    dependencies: BTreeMap::new(),
                },
                expires_at,
            )
            .unwrap();
    }

    fn consume(control: &ControlStore, raw: &str) -> PortResult<()> {
        control.consume_agent_live_check_capability(
            &ProtectedApplyCapability::new(raw.to_owned()).unwrap(),
            &WorkspaceId::default(),
            "desktop",
            &CanonicalDigest::of_bytes(b"live-target"),
            &RevisionSetV1 {
                target: 0,
                dependencies: BTreeMap::new(),
            },
        )
    }

    #[test]
    fn live_capability_is_one_shot_across_connections_and_restart() {
        let directory = crate::test_tempdir().unwrap();
        let path = directory.path().join("control.db");
        let backups = directory.path().join("backups");
        let control =
            ControlStore::open(&crate::test_storage_authority(), &path, &backups).unwrap();
        register(&control, "live-one-shot", "CheckAgentConnection", i64::MAX);
        let peer = ControlStore::open(&crate::test_storage_authority(), &path, &backups).unwrap();
        consume(&control, "live-one-shot").unwrap();
        assert_eq!(
            consume(&peer, "live-one-shot").unwrap_err().code,
            PortErrorCode::PermissionDenied
        );
        register(&control, "live-race", "CheckAgentConnection", i64::MAX);
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let peer_start = start.clone();
        let first = std::thread::spawn(move || {
            start.wait();
            consume(&control, "live-race").map_err(|error| error.code)
        });
        let second = std::thread::spawn(move || {
            peer_start.wait();
            consume(&peer, "live-race").map_err(|error| error.code)
        });
        assert!(matches!(
            (first.join().unwrap(), second.join().unwrap()),
            (Ok(()), Err(PortErrorCode::PermissionDenied))
                | (Err(PortErrorCode::PermissionDenied), Ok(()))
        ));
        let reopened =
            ControlStore::open(&crate::test_storage_authority(), &path, &backups).unwrap();
        assert_eq!(
            consume(&reopened, "live-one-shot").unwrap_err().code,
            PortErrorCode::PermissionDenied
        );
        assert!(!reopened.writer_recovery_required().unwrap());
    }

    #[test]
    fn live_capability_rejects_wrong_scope_without_consuming_the_valid_request() {
        let directory = crate::test_tempdir().unwrap();
        let control = ControlStore::open(
            &crate::test_storage_authority(),
            directory.path().join("control.db"),
            directory.path().join("backups"),
        )
        .unwrap();
        register(&control, "live-bound", "CheckAgentConnection", i64::MAX);
        let capability = ProtectedApplyCapability::new("live-bound".into()).unwrap();
        for (principal, digest) in [
            (
                "interactive-user",
                CanonicalDigest::of_bytes(b"live-target"),
            ),
            ("skill", CanonicalDigest::of_bytes(b"live-target")),
            ("desktop", CanonicalDigest::of_bytes(b"different-target")),
        ] {
            assert_eq!(
                control
                    .consume_agent_live_check_capability(
                        &capability,
                        &WorkspaceId::default(),
                        principal,
                        &digest,
                        &RevisionSetV1 {
                            target: 0,
                            dependencies: BTreeMap::new()
                        },
                    )
                    .unwrap_err()
                    .code,
                PortErrorCode::PermissionDenied
            );
        }
        consume(&control, "live-bound").unwrap();
        register(&control, "apply-only", "ApplySetup", i64::MAX);
        register(&control, "expired", "CheckAgentConnection", 1);
        for raw in ["apply-only", "expired", "missing"] {
            assert_eq!(
                consume(&control, raw).unwrap_err().code,
                PortErrorCode::PermissionDenied
            );
        }
    }

    #[test]
    fn live_capability_rechecks_revisions_before_consuming() {
        let directory = crate::test_tempdir().unwrap();
        let control = ControlStore::open(
            &crate::test_storage_authority(),
            directory.path().join("control.db"),
            directory.path().join("backups"),
        )
        .unwrap();
        register(&control, "live-stale", "CheckAgentConnection", i64::MAX);
        control
            .set_dependency_revision(&WorkspaceId::default(), "source", 1)
            .unwrap();
        assert_eq!(
            consume(&control, "live-stale").unwrap_err().code,
            PortErrorCode::Conflict
        );
        let revoked: bool = control
            .connection
            .borrow()
            .query_row("SELECT revoked FROM apply_capabilities", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!revoked);
    }
}
