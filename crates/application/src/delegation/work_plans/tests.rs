use super::*;
use hiroute_domain::AgentCollaborationGrant;
use hiroute_domain::delegation::WorkerHarnessV1;
use std::sync::Mutex;

struct Authority(Mutex<AgentCollaborationGrant>);
impl DelegationGrantAuthorityPort for Authority {
    fn current_grant(
        &self,
        _: &WorkspaceId,
        _: &str,
    ) -> Result<Option<AgentCollaborationGrant>, hiroute_domain::delegation::DelegationErrorV1>
    {
        Ok(Some(self.0.lock().unwrap().clone()))
    }
}
struct RacingMetadata {
    authority: Arc<Authority>,
    revoke: bool,
}
impl WorkPlanMetadataPort for RacingMetadata {
    fn current_plans(
        &self,
        _: &WorkspaceId,
        _: &BTreeSet<AgentPlanId>,
    ) -> Result<Vec<WorkPlanMetadataV1>, ErrorCode> {
        let mut grant = self.authority.0.lock().unwrap();
        if self.revoke {
            grant.enabled = false;
            grant.credential_verifier = None;
        } else {
            grant.allowed_plan_ids.clear();
        }
        Ok(vec![WorkPlanMetadataV1 {
            agent_plan_id: AgentPlanId::parse("plan").unwrap(),
            alias: "worker".into(),
            display_name: "Worker".into(),
            purpose: "data".into(),
            published: true,
            work: Some((
                WorkerHarnessV1::CodexCli,
                hiroute_domain::AgentIngressProtocolV1::Responses,
            )),
            availability: WorkPlanAvailabilityV1::Ready,
            reason: None,
        }])
    }
}
fn fixture() -> (
    Arc<Authority>,
    AgentCollaborationCredential,
    WorkPlanListRequestV1,
) {
    let material = AgentCollaborationCredential::from_csprng_entropy([9; 32]);
    let query = WorkPlanListRequestV1 {
        workspace_id: WorkspaceId::default(),
        context_id: "owner".into(),
        grant_id: "collaboration-grant/one".into(),
    };
    let grant = AgentCollaborationGrant::issue(
        query.workspace_id.clone(),
        query.context_id.clone(),
        query.grant_id.clone(),
        1,
        BTreeSet::from([AgentPlanId::parse("plan").unwrap()]),
        &material,
    )
    .unwrap();
    (Arc::new(Authority(Mutex::new(grant))), material, query)
}
#[test]
fn work_plans_recheck_shrink_after_metadata_read() {
    let (authority, secret, q) = fixture();
    let metadata = Arc::new(RacingMetadata {
        authority: authority.clone(),
        revoke: false,
    });
    let directory = WorkPlanDirectory::new(authority, metadata);
    assert!(directory.list(&q, &secret).unwrap().plans.is_empty());
}
#[test]
fn work_plans_recheck_revoke_after_metadata_read() {
    let (authority, secret, q) = fixture();
    let metadata = Arc::new(RacingMetadata {
        authority: authority.clone(),
        revoke: true,
    });
    let directory = WorkPlanDirectory::new(authority, metadata);
    assert_eq!(
        directory.list(&q, &secret).unwrap_err(),
        ErrorCode::CapabilityDenied
    );
}
#[test]
fn work_plans_uncomposed_metadata_is_unavailable_only_after_authentication() {
    let (authority, secret, mut q) = fixture();
    let directory = WorkPlanDirectory::new(authority, Arc::new(UnavailableWorkPlanMetadata));
    assert_eq!(
        directory.list(&q, &secret).unwrap_err(),
        ErrorCode::CapabilityUnavailable
    );
    q.context_id = "other".into();
    assert_eq!(
        directory.list(&q, &secret).unwrap_err(),
        ErrorCode::CapabilityDenied
    );
}
