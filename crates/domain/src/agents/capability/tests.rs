use super::*;

fn evidence(capability: AgentCapability, state: CapabilityState) -> CapabilityEvidence {
    CapabilityEvidence {
        capability,
        state,
        adapter_contract: "test-native-schema/1".into(),
        observed_at_unix_ms: 1000,
        dependency_digest: CanonicalDigest::of_bytes(b"context-v1"),
        reason: None,
    }
}

#[test]
fn agent_capabilities_skill_only_does_not_depend_on_model_or_native_spawn() {
    let proofs = AgentCapabilitySet::new([
        evidence(
            AgentCapability::AtomicManagedReplace,
            CapabilityState::Proven,
        ),
        evidence(AgentCapability::SkillLoading, CapabilityState::Proven),
        evidence(
            AgentCapability::TrustedCliExecution,
            CapabilityState::Proven,
        ),
        evidence(
            AgentCapability::IngressAuthentication,
            CapabilityState::Unavailable,
        ),
    ])
    .unwrap();
    let digest = CanonicalDigest::of_bytes(b"context-v1");
    assert!(
        proofs
            .require(AgentAction::InstallCollaborationSkill, &digest)
            .is_ok()
    );
    assert!(
        proofs
            .require(AgentAction::ConfigureModel, &digest)
            .is_err()
    );
    assert!(proofs.require(AgentAction::VerifyModel, &digest).is_err());
}

#[test]
fn agent_capabilities_stale_or_unknown_evidence_cannot_authorize_writes() {
    let proofs = AgentCapabilitySet::new([
        evidence(
            AgentCapability::EffectiveConfiguration,
            CapabilityState::Proven,
        ),
        evidence(
            AgentCapability::AtomicManagedReplace,
            CapabilityState::Proven,
        ),
        evidence(
            AgentCapability::IngressAuthentication,
            CapabilityState::Proven,
        ),
    ])
    .unwrap();
    assert!(
        proofs
            .require(
                AgentAction::ConfigureModel,
                &CanonicalDigest::of_bytes(b"context-v1")
            )
            .is_ok()
    );
    let rejected = proofs
        .require(
            AgentAction::ConfigureModel,
            &CanonicalDigest::of_bytes(b"context-v2"),
        )
        .unwrap_err();
    assert!(
        rejected
            .iter()
            .all(|item| item.reason == CapabilityBlockReason::Stale)
    );
    let unknown = AgentCapabilitySet::new([evidence(
        AgentCapability::EffectiveConfiguration,
        CapabilityState::Unknown,
    )])
    .unwrap();
    let blocked = unknown
        .require(
            AgentAction::ConfigureModel,
            &CanonicalDigest::of_bytes(b"context-v1"),
        )
        .unwrap_err();
    assert_eq!(blocked.len(), 3);
}

#[test]
fn agent_capabilities_reject_duplicate_proofs_instead_of_order_dependent_allow() {
    assert!(
        AgentCapabilitySet::new([
            evidence(AgentCapability::SkillLoading, CapabilityState::Proven),
            evidence(AgentCapability::SkillLoading, CapabilityState::Unavailable),
        ])
        .is_err()
    );
}
