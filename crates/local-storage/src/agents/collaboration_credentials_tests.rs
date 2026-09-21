use super::*;
use crate::test_tempdir as tempdir;
use hiroute_domain::{AgentPlanId, WorkspaceId};
use std::collections::BTreeSet;

#[test]
fn collaboration_credential_is_encrypted_idempotent_and_bound_across_restart() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("data/secrets.db");
    let key = dir.path().join("data/master-key");
    let backups = dir.path().join("backups");
    let store =
        LocalSecretStore::open(&crate::test_storage_authority(), &db, &key, &backups).unwrap();
    let operation = OperationId::parse("op_00112233445566778899aabbccddeeff").unwrap();
    let material = AgentCollaborationCredential::from_csprng_entropy([7; 32]);
    let grant = AgentCollaborationGrant::issue(
        WorkspaceId::default(),
        "context/one".into(),
        "collaboration-grant/one".into(),
        1,
        BTreeSet::new(),
        &material,
    )
    .unwrap();
    store
        .prepare_collaboration_credential(&operation, &grant, &material)
        .unwrap();
    store
        .prepare_collaboration_credential(&operation, &grant, &material)
        .unwrap();
    let ciphertext: Vec<u8> = store
        .connection
        .borrow()
        .query_row(
            "SELECT ciphertext FROM agent_collaboration_credentials",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !ciphertext
            .windows(material.expose().len())
            .any(|window| window == material.expose())
    );
    drop(store);
    let store =
        LocalSecretStore::open(&crate::test_storage_authority(), &db, &key, &backups).unwrap();
    let recovered = store
        .recover_prepared_collaboration_credential(&operation, &grant)
        .unwrap();
    assert_eq!(recovered.expose(), material.expose());
    let wrong_op = OperationId::parse("op_ffeeddccbbaa99887766554433221100").unwrap();
    assert!(
        store
            .recover_prepared_collaboration_credential(&wrong_op, &grant)
            .is_err()
    );
    let mut changed = grant.clone();
    changed
        .allowed_plan_ids
        .insert(AgentPlanId::parse("plan/new").unwrap());
    assert!(
        store
            .recover_prepared_collaboration_credential(&operation, &changed)
            .is_err()
    );
    let revoked = grant.plan_revocation(operation.clone(), 1).unwrap();
    assert!(
        store
            .recover_prepared_collaboration_credential(&operation, &revoked.after)
            .is_err()
    );
    store
        .connection
        .borrow()
        .execute(
            "UPDATE agent_collaboration_credentials SET context_id='context/other'",
            [],
        )
        .unwrap();
    assert!(
        store
            .recover_prepared_collaboration_credential(&operation, &grant)
            .is_err()
    );
}
