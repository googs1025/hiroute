use std::collections::BTreeSet;

use hiroute_domain::{
    AuthenticationKind, BillingClass, ComputeSourceControlPort, ComputeSourceMutationV1,
    ComputeSourceV1, ConnectionOptionV1, ConnectionOrigin, ConnectorDescriptorV1,
    ConnectorRegistryBundleV1, ConnectorRuntimeKind, CredentialPoolIdentityV1,
    CredentialPoolMutationKind, CredentialPoolMutationV1, CredentialPoolV1, EndpointProfileV1,
    InventoryStrategyKind, MaterializationState, ProtocolEndpointV1, SourceIdentityV1,
    SourceOrigin, UpstreamProtocol,
};

use super::*;

impl MemoryPorts {
    fn compute_pool_identity() -> CredentialPoolIdentityV1 {
        CredentialPoolIdentityV1 {
            pool_id: "pool/bailian-main".into(),
            binding_id: "binding/bailian-model-one".into(),
            binding_revision: 1,
            binding_digest: CanonicalDigest::of_bytes(b"binding-bailian-model-one"),
            source_id: "source/bailian-main".into(),
            source_revision: 1,
            connection_option_id: "bailian.payg.cn.v1".into(),
            source_identity_digest: CanonicalDigest::of_bytes(b"source-bailian-main"),
            offer_ref: "offer/model-one".into(),
            offer_revision: 1,
            offer_evidence_digest: CanonicalDigest::of_bytes(b"offer-model-one"),
            billing_class: BillingClass::Paid,
            model_configuration_id: "model.one".into(),
            authentication: AuthenticationKind::ProviderApiKey,
        }
    }

    fn secondary_pool_identity() -> CredentialPoolIdentityV1 {
        CredentialPoolIdentityV1 {
            pool_id: "pool/bailian-secondary".into(),
            binding_id: "binding/bailian-model-two".into(),
            binding_digest: CanonicalDigest::of_bytes(b"binding-bailian-model-two"),
            offer_ref: "offer/model-two".into(),
            offer_evidence_digest: CanonicalDigest::of_bytes(b"offer-model-two"),
            model_configuration_id: "model.two".into(),
            ..Self::compute_pool_identity()
        }
    }

    fn compute_pool() -> CredentialPoolV1 {
        Self::compute_pool_identity()
            .materialize_first(
                CredentialRefV1::new(
                    "credential/bailian-existing",
                    "source/source/bailian-main",
                    "hirouted",
                    "provider-auth",
                    ["connection-option/bailian.payg.cn.v1".into()],
                    1,
                )
                .unwrap(),
                CanonicalDigest::of_bytes(b"existing-key"),
            )
            .unwrap()
    }
}

fn existing_compute_ports(input: &[u8]) -> MemoryPorts {
    let ports = MemoryPorts::with_input(input);
    ports.state.borrow_mut().compute_pool = Some(MemoryPorts::compute_pool());
    ports
}

impl CredentialPoolControlPort for MemoryPorts {
    fn apply_credential_pool(
        &self,
        operation_id: &OperationId,
        workspace: &WorkspaceId,
        _expected_target_revision: u64,
        mutation: &CredentialPoolMutationV1,
    ) -> PortResult<OwnedEffectV1> {
        mutation
            .validate_against(self.state.borrow().compute_pool.as_ref())
            .map_err(|_| PortError::new(PortErrorCode::Conflict, "test.pool.cas"))?;
        self.state.borrow_mut().staged_pool = Some(mutation.clone());
        Ok(self.stage(
            operation_id,
            fake_effect(
                operation_id,
                &format!("control:{workspace}"),
                workspace.as_str(),
                OwnedEffectKind::Control,
            ),
        ))
    }
}

impl ComputeSourceControlPort for MemoryPorts {
    fn apply_compute_source(
        &self,
        operation_id: &OperationId,
        workspace: &WorkspaceId,
        _expected_target_revision: u64,
        mutation: &ComputeSourceMutationV1,
    ) -> PortResult<OwnedEffectV1> {
        mutation
            .validate_against(None)
            .map_err(|_| PortError::new(PortErrorCode::Conflict, "test.source.cas"))?;
        Ok(self.stage(
            operation_id,
            fake_effect(
                operation_id,
                &format!("control:{workspace}"),
                workspace.as_str(),
                OwnedEffectKind::Control,
            ),
        ))
    }
}

impl ConnectionOptionAuthorizationPort for MemoryPorts {
    fn connection_option_authorization(
        &self,
        connection_option_id: &str,
    ) -> PortResult<Option<crate::change::ConnectionOptionAuthorizationV1>> {
        Ok(match connection_option_id {
            "source-a" => Some(crate::change::ConnectionOptionAuthorizationV1 {
                requires_explicit_materialization: false,
                accepts_native_secret: true,
            }),
            "bailian.payg.cn.v1"
            | "zhipu.general.cn.v1"
            | "kimi.open-platform.cn.v1"
            | "deepseek.official.global.v1" => {
                Some(crate::change::ConnectionOptionAuthorizationV1 {
                    requires_explicit_materialization: true,
                    accepts_native_secret: true,
                })
            }
            _ => None,
        })
    }

    fn source_uses_connection_option(
        &self,
        source_id: &str,
        connection_option_id: &str,
    ) -> PortResult<bool> {
        Ok(source_id == "source/bailian-main" && connection_option_id == "bailian.payg.cn.v1")
    }

    fn is_registered_compute_source(&self, source_id: &str) -> PortResult<bool> {
        Ok(source_id == "source/bailian-main")
    }

    fn is_registered_price_target(
        &self,
        offer_ref: &str,
        model_configuration_id: &str,
        currency: &str,
        target_rule_id: Option<&str>,
    ) -> PortResult<bool> {
        Ok(offer_ref == "offer/model-one"
            && model_configuration_id == "model.one"
            && currency == "USD"
            && target_rule_id.is_none_or(|rule| rule == "rate/model-one"))
    }

    fn credential_pool_identity(
        &self,
        pool_id: &str,
        binding_id: &str,
    ) -> PortResult<Option<CredentialPoolIdentityV1>> {
        Ok([
            Self::compute_pool_identity(),
            Self::secondary_pool_identity(),
        ]
        .into_iter()
        .find(|identity| identity.pool_id == pool_id && identity.binding_id == binding_id))
    }

    fn credential_pool(&self, pool_id: &str) -> PortResult<Option<CredentialPoolV1>> {
        Ok(self
            .state
            .borrow()
            .compute_pool
            .clone()
            .filter(|pool| pool.pool_id == pool_id))
    }

    fn credential_reference_count(&self, credential_id: &str) -> PortResult<u64> {
        if credential_id != "credential/bailian-existing" {
            return Ok(0);
        }
        Ok(self.state.borrow().credential_reference_count.unwrap_or(1))
    }

    fn compute_source_materialization(
        &self,
        connection_option_id: &str,
        source_id: &str,
        expected_revision: u64,
        explicit_materialization: bool,
    ) -> PortResult<Option<crate::change::RegisteredComputeSourceMaterializationV1>> {
        if connection_option_id != "bailian.payg.cn.v1"
            || source_id != "source/bailian-new"
            || expected_revision != 0
            || !explicit_materialization
        {
            return Ok(None);
        }
        let registry = compute_source_registry();
        let identity = SourceIdentityV1 {
            identity_revision: 1,
            provider_platform_id: "bailian".into(),
            service_offering_id: "model-studio".into(),
            entitlement_id: "payg-api-key".into(),
            usage_scope: "account".into(),
            endpoint_profile_id: "endpoint.bailian.payg.cn.v1".into(),
            endpoint_profile_revision: 1,
            region_id: "cn".into(),
            account_subject_ref: "account/bailian-new".into(),
            evidence_refs: vec![CanonicalDigest::of_bytes(b"test-bailian-source")],
        };
        let desired = ComputeSourceV1 {
            schema: hiroute_domain::COMPUTE_STATE_SCHEMA_V1.into(),
            source_id: source_id.into(),
            revision: 1,
            connection_option_id: connection_option_id.into(),
            connector_id: "connector.bailian.p0".into(),
            connector_revision: 1,
            origin: SourceOrigin::NativeApi,
            identity_digest: identity.digest().unwrap(),
            identity,
            billing_class: BillingClass::Paid,
            state: MaterializationState::NeedsCredential,
        };
        Ok(Some(
            crate::change::RegisteredComputeSourceMaterializationV1 {
                current: None,
                desired,
                registry,
                explicit_materialization,
            },
        ))
    }
}

fn compute_source_registry() -> ConnectorRegistryBundleV1 {
    ConnectorRegistryBundleV1 {
        schema: hiroute_domain::CONNECTOR_REGISTRY_SCHEMA_V1.into(),
        registry_version: "test-registry-v1".into(),
        product_release: "test-release-v1".into(),
        connectors: vec![ConnectorDescriptorV1 {
            connector_id: "connector.bailian.p0".into(),
            revision: 1,
            runtime_kind: ConnectorRuntimeKind::BuiltinNative,
            implementation_ref: "builtin/bailian".into(),
            implementation_revision: 1,
            accepted_origins: BTreeSet::from([ConnectionOrigin::NativeApi]),
            authentication: AuthenticationKind::ProviderApiKey,
            required_secret_slots: vec!["provider_api_key".into()],
            endpoint_profile_refs: vec!["endpoint.bailian.payg.cn.v1".into()],
            catalog_adapter_ref: "catalog.bailian".into(),
            catalog_adapter_revision: 1,
            error_classifier_ref: "errors.bailian".into(),
            error_classifier_revision: 1,
            usage_decoder_ref: "usage.bailian".into(),
            usage_decoder_revision: 1,
            cache_policy_ref: "cache.bailian".into(),
            cache_policy_revision: 1,
        }],
        endpoint_profiles: vec![EndpointProfileV1 {
            endpoint_profile_id: "endpoint.bailian.payg.cn.v1".into(),
            revision: 1,
            connector_id: "connector.bailian.p0".into(),
            connector_revision: 1,
            provider_platform_id: "bailian".into(),
            service_offering_id: "model-studio".into(),
            entitlement_id: "payg-api-key".into(),
            usage_scope: "account".into(),
            region_id: "cn".into(),
            logical_endpoint_group: "bailian-payg".into(),
            protocol_endpoints: vec![ProtocolEndpointV1 {
                protocol_endpoint_id: "endpoint.bailian.payg.cn.v1.messages".into(),
                protocol: UpstreamProtocol::Messages,
                base_url: "https://dashscope.aliyuncs.com".into(),
                request_path: "/apps/anthropic/v1/messages".into(),
                adapter_ref: "adapter.anthropic-messages.v1".into(),
                adapter_revision: 1,
                stable_preference: 0,
                inventory_path: None,
                authentication_semantics: None,
                required_headers: Vec::new(),
            }],
            inventory_strategy: InventoryStrategyKind::BundledCatalog,
            inventory_protocol_endpoint_id: None,
            verification_evidence: CanonicalDigest::of_bytes(b"test-endpoint").to_string(),
            last_verified_at: 1,
        }],
        connection_options: vec![ConnectionOptionV1 {
            connection_option_id: "bailian.payg.cn.v1".into(),
            display_name: "Bailian PAYG".into(),
            origin: ConnectionOrigin::NativeApi,
            connector_id: "connector.bailian.p0".into(),
            connector_revision: 1,
            endpoint_profile_id: "endpoint.bailian.payg.cn.v1".into(),
            endpoint_profile_revision: 1,
            billing_class: BillingClass::Paid,
            free_offer_ref: None,
            direct_verification_evidence: None,
        }],
    }
}

fn compute_credential_change(slot: &str) -> hiroute_domain::ChangeSpecV1 {
    hiroute_domain::ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: "compute.credential.add".to_owned(),
        resource_id: Some("personal/default".to_owned()),
        desired_state: json!({
            "connection_option_id": "bailian.payg.cn.v1",
            "source_id": "source/bailian-main",
            "pool_id": "pool/bailian-main",
            "binding_id": "binding/bailian-model-one",
            "binding_revision": 1,
            "offer_ref": "offer/model-one",
            "offer_revision": 1,
            "model_configuration_id": "model.one",
            "credential_id": "credential/bailian-key-1",
            "input_slot": slot,
            "secret_source": {"kind": "manual_input"},
            "expected_generation": 0,
            "expected_pool_revision": 1,
        }),
    }
}

#[test]
fn prepared_revalidation_holds_writer_and_rejection_does_not_consume_grant() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let ports = existing_compute_ports(b"prepared-secret-sentinel");
    let runtime = Arc::new(TransactionRuntime::default());
    let coordinator = open(&ports, &runtime);
    let prepared = prepare_preview(
        &ports,
        &ports,
        &ports,
        &ports,
        &ports,
        PreviewRequestV1::new(compute_credential_change("stdin")),
        ports.current_revisions(&WorkspaceId::default()).unwrap(),
    )
    .unwrap();
    let request = ApplyRequestV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        spec: prepared.result.normalized_spec,
        accept_digest: prepared.result.change_digest,
        expected_revisions: prepared.result.expected_revisions,
        idempotency_key: "prepared-revalidation".into(),
        apply_capability: Some("prepared-capability".into()),
    };
    let kind = command_by_id(&request.spec.command_id)
        .unwrap()
        .operation_id;
    ports.grant_for(
        "prepared-capability",
        &request.accept_digest,
        &request.expected_revisions,
        &kind,
    );
    let stale = Arc::new(AtomicBool::new(true));
    let checked = Arc::new(AtomicBool::new(false));
    let seal = || {
        let runtime = runtime.clone();
        let stale = stale.clone();
        let checked = checked.clone();
        PreparedTransactionV1::for_setup(
            request.clone(),
            request.accept_digest.clone(),
            prepared.plan.clone(),
        )
        .unwrap()
        .with_revalidation(move || {
            assert!(
                runtime.writer.try_lock().is_err(),
                "revalidation ran outside writer"
            );
            checked.store(true, Ordering::Release);
            if stale.load(Ordering::Acquire) {
                Err(TransactionError::ChangePreviewStale)
            } else {
                Ok(())
            }
        })
    };
    assert!(matches!(
        coordinator.accept_prepared(&WorkspaceId::default(), &principal(), seal()),
        Err(TransactionError::ChangePreviewStale)
    ));
    assert!(checked.load(Ordering::Acquire));
    assert_eq!(ports.state.borrow().durable_writes, 0);
    assert!(
        ports
            .state
            .borrow()
            .grants
            .values()
            .all(|grant| grant.consumed.is_none())
    );
    stale.store(false, Ordering::Release);
    let first = coordinator
        .accept_prepared(&WorkspaceId::default(), &principal(), seal())
        .unwrap();
    checked.store(false, Ordering::Release);
    stale.store(true, Ordering::Release);
    let replay = coordinator
        .accept_prepared(&WorkspaceId::default(), &principal(), seal())
        .unwrap();
    assert!(replay.existing());
    assert_eq!(
        first.operation().operation_id,
        replay.operation().operation_id
    );
    assert!(
        !checked.load(Ordering::Acquire),
        "same operation replay must precede revalidation"
    );
}

#[test]
fn compute_protected_input_channels_bind_only_fingerprint_and_reference() {
    let sentinel = b"p25003-compute-secret-sentinel-77b13";
    for slot in ["hidden_tty", "file_fd", "stdin", "discovered_ref"] {
        let ports = existing_compute_ports(sentinel);
        let runtime = TransactionRuntime::default();
        let coordinator = open(&ports, &runtime);
        let mut spec = compute_credential_change(slot);
        if slot == "discovered_ref" {
            spec.desired_state["secret_source"] = json!({
                "kind": "discovered_config",
                "scanner_id": "scanner.fixture",
                "scanner_version": "v1",
                "source_ref": "config.fixture",
                "field_selector": "api_key",
                "observed_revision": 1,
            });
        }
        let preview = coordinator
            .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
            .unwrap();
        let encoded = serde_json::to_vec(&preview).unwrap();
        assert!(
            !encoded
                .windows(sentinel.len())
                .any(|window| window == sentinel)
        );
        assert_eq!(ports.counters(), (0, 1, 0, 1));
    }
}

#[test]
fn compute_secret_source_descriptor_is_validated_before_protected_input_read() {
    let ports = existing_compute_ports(b"never-read");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let mut spec = compute_credential_change("discovered_ref");
    spec.desired_state["secret_source"] = json!({
        "kind": "discovered_config",
        "scanner_id": "scanner.fixture",
        "scanner_version": "v1",
        "source_ref": "config.fixture",
        "field_selector": "api_key",
        "observed_revision": 0,
    });
    let before = ports.counters();
    assert!(
        coordinator
            .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
            .is_err()
    );
    assert_eq!(ports.counters(), before);
}

#[test]
fn compute_apply_rejects_secret_changed_after_preview_before_operation_write() {
    let ports = existing_compute_ports(b"preview-secret-a");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let preview = coordinator
        .preview(
            &WorkspaceId::default(),
            PreviewRequestV1::new(compute_credential_change("stdin")),
        )
        .unwrap();
    ports.grant_for(
        "compute-capability",
        &preview.change_digest,
        &preview.expected_revisions,
        "ApplyCredentialAdd",
    );
    ports.state.borrow_mut().input = b"apply-secret-b".to_vec();
    let request = ApplyRequestV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        spec: preview.normalized_spec,
        accept_digest: preview.change_digest,
        expected_revisions: preview.expected_revisions,
        idempotency_key: "compute-preview-a-apply-b".into(),
        apply_capability: Some("compute-capability".into()),
    };
    let before_writes = ports.state.borrow().durable_writes;
    assert!(matches!(
        coordinator.accept(&WorkspaceId::default(), &principal(), request),
        Err(TransactionError::ChangePreviewStale)
    ));
    assert_eq!(ports.state.borrow().durable_writes, before_writes);
    assert!(ports.state.borrow().operations.is_empty());
}

#[test]
fn compute_add_stages_secret_and_exact_offer_bound_pool_in_one_operation() {
    let ports = existing_compute_ports(b"new-key-material");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let preview = coordinator
        .preview(
            &WorkspaceId::default(),
            PreviewRequestV1::new(compute_credential_change("stdin")),
        )
        .unwrap();
    ports.grant_for(
        "compute-pool-capability",
        &preview.change_digest,
        &preview.expected_revisions,
        "ApplyCredentialAdd",
    );
    let accepted = coordinator
        .accept(
            &WorkspaceId::default(),
            &principal(),
            ApplyRequestV1 {
                schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
                spec: preview.normalized_spec,
                accept_digest: preview.change_digest,
                expected_revisions: preview.expected_revisions,
                idempotency_key: "compute-pool-atomic".into(),
                apply_capability: Some("compute-pool-capability".into()),
            },
        )
        .unwrap();
    let terminal = coordinator.run(&accepted.operation.operation_id).unwrap();
    assert_eq!(terminal.state, OperationState::Succeeded);
    let staged = ports.state.borrow().staged_pool.clone().unwrap();
    assert_eq!(
        staged.kind(),
        hiroute_domain::CredentialPoolMutationKind::Add
    );
    assert_eq!(staged.expected_revision(), 1);
    assert_eq!(staged.desired().revision, 2);
    assert_eq!(staged.desired().offer_ref, "offer/model-one");
    assert_eq!(staged.desired().binding_id, "binding/bailian-model-one");
    assert_eq!(staged.desired().model_configuration_id, "model.one");
    assert_eq!(staged.desired().credentials.len(), 2);
    assert_eq!(staged.desired().credentials[1].credential.generation(), 1);
}

#[test]
fn compute_first_add_materializes_one_key_pool_from_exact_binding_identity() {
    let ports = MemoryPorts::with_input(b"first-key-material");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let mut spec = compute_credential_change("stdin");
    spec.desired_state["expected_pool_revision"] = json!(0);
    let preview = coordinator
        .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
        .unwrap();
    ports.grant_for(
        "compute-first-key-capability",
        &preview.change_digest,
        &preview.expected_revisions,
        "ApplyCredentialAdd",
    );
    let accepted = coordinator
        .accept(
            &WorkspaceId::default(),
            &principal(),
            ApplyRequestV1 {
                schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
                spec: preview.normalized_spec,
                accept_digest: preview.change_digest,
                expected_revisions: preview.expected_revisions,
                idempotency_key: "compute-first-key".into(),
                apply_capability: Some("compute-first-key-capability".into()),
            },
        )
        .unwrap();
    assert_eq!(
        coordinator
            .run(&accepted.operation.operation_id)
            .unwrap()
            .state,
        OperationState::Succeeded
    );
    let staged = ports.state.borrow().staged_pool.clone().unwrap();
    assert_eq!(staged.expected_revision(), 0);
    assert_eq!(staged.desired().revision, 1);
    assert_eq!(staged.desired().credentials.len(), 1);
    assert_eq!(staged.desired().binding_id, "binding/bailian-model-one");
}

#[test]
fn compute_credential_target_is_unambiguous_for_multi_binding_source() {
    for (pool_id, binding_id) in [
        ("pool/bailian-main", "binding/bailian-model-two"),
        ("pool/bailian-secondary", "binding/bailian-model-two"),
    ] {
        let ports = existing_compute_ports(b"never-read");
        let runtime = TransactionRuntime::default();
        let coordinator = open(&ports, &runtime);
        let mut spec = compute_credential_change("stdin");
        spec.desired_state["pool_id"] = json!(pool_id);
        spec.desired_state["binding_id"] = json!(binding_id);
        let before = ports.counters();
        assert!(
            coordinator
                .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
                .is_err()
        );
        assert_eq!(ports.counters(), before);
    }
}

#[test]
fn compute_remove_preserves_secret_material_while_another_pool_reference_exists() {
    let ports = MemoryPorts::default();
    ports.state.borrow_mut().compute_pool = Some(MemoryPorts::compute_pool());
    let second = CredentialRefV1::new(
        "credential/bailian-second",
        "source/source/bailian-main",
        "hirouted",
        "provider-auth",
        ["connection-option/bailian.payg.cn.v1".to_owned()],
        1,
    )
    .unwrap();
    let pool = MemoryPorts::compute_pool()
        .add(1, second, CanonicalDigest::of_bytes(b"existing-second-key"))
        .unwrap();
    ports.state.borrow_mut().compute_pool = Some(pool);
    ports.state.borrow_mut().credential_reference_count = Some(2);
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let spec = hiroute_domain::ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: "compute.credential.remove".into(),
        resource_id: Some("personal/default".into()),
        desired_state: json!({
            "connection_option_id": "bailian.payg.cn.v1",
            "source_id": "source/bailian-main",
            "pool_id": "pool/bailian-main",
            "binding_id": "binding/bailian-model-one",
            "binding_revision": 1,
            "offer_ref": "offer/model-one",
            "offer_revision": 1,
            "model_configuration_id": "model.one",
            "credential_id": "credential/bailian-existing",
            "expected_generation": 1,
            "expected_pool_revision": 2,
        }),
    };
    let preview = coordinator
        .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
        .unwrap();
    assert_eq!(ports.state.borrow().secret_reads, 0);
    ports.grant_for(
        "compute-remove-capability",
        &preview.change_digest,
        &preview.expected_revisions,
        "ApplyCredentialRemove",
    );
    let accepted = coordinator
        .accept(
            &WorkspaceId::default(),
            &principal(),
            ApplyRequestV1 {
                schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
                spec: preview.normalized_spec,
                accept_digest: preview.change_digest,
                expected_revisions: preview.expected_revisions,
                idempotency_key: "compute-shared-reference-remove".into(),
                apply_capability: Some("compute-remove-capability".into()),
            },
        )
        .unwrap();
    assert!(accepted.operation.plan.secrets().is_empty());
    let terminal = coordinator.run(&accepted.operation.operation_id).unwrap();
    assert_eq!(terminal.state, OperationState::Succeeded);
    let staged = ports.state.borrow().staged_pool.clone().unwrap();
    assert_eq!(staged.kind(), CredentialPoolMutationKind::Remove);
    assert_eq!(staged.desired().credentials.len(), 1);
    assert_eq!(ports.state.borrow().secret_reads, 0);
}

#[test]
fn compute_arbitrary_destination_fields_fail_before_secret_network_or_operation() {
    for (field, value) in [
        ("endpoint_url", "https://api.example.invalid"),
        ("host", "api.example.invalid."),
        ("protocol", "responses"),
        ("auth", "bearer"),
        ("connector_target", "similar.example.invalid"),
        ("redirect", "https://evil.invalid"),
    ] {
        let ports = existing_compute_ports(b"never-read");
        let runtime = TransactionRuntime::default();
        let coordinator = open(&ports, &runtime);
        let before = ports.counters();
        let mut spec = compute_credential_change("stdin");
        spec.desired_state[field] = json!(value);
        assert!(
            coordinator
                .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
                .is_err()
        );
        assert_eq!(ports.counters(), before);
    }
    for malicious_option in [
        "unknown.similar.v1",
        "BAILIAN.payg.cn.v1",
        "bailian.payg.cn.v1.",
        "bailian.payg.cn.v1:443",
        "bailian.payg.cn.v1/path",
        "bailian.payg.cn.v1?redirect=evil",
        "xn--bailian-9za.payg.cn.v1",
        "bailian.payg.cn.v1.evil.test",
        "百炼.payg.cn.v1",
    ] {
        let ports = existing_compute_ports(b"never-read");
        let runtime = TransactionRuntime::default();
        let coordinator = open(&ports, &runtime);
        let before = ports.counters();
        let mut spec = compute_credential_change("stdin");
        spec.desired_state["connection_option_id"] = json!(malicious_option);
        assert!(
            coordinator
                .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
                .is_err(),
            "accepted connection option variant {malicious_option}"
        );
        assert_eq!(ports.counters(), before);
    }
    let ports = existing_compute_ports(b"never-read");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let before = ports.counters();
    let mut mismatched_source = compute_credential_change("stdin");
    mismatched_source.desired_state["source_id"] = json!("source/other-provider");
    assert!(
        coordinator
            .preview(
                &WorkspaceId::default(),
                PreviewRequestV1::new(mismatched_source)
            )
            .is_err()
    );
    assert_eq!(ports.counters(), before);

    let ports = existing_compute_ports(b"never-read");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let before = ports.counters();
    let mut unregistered_setup = change(true);
    unregistered_setup.desired_state["connection_option_id"] =
        json!("bailian.payg.cn.v1.evil.test");
    assert!(
        coordinator
            .preview(
                &WorkspaceId::default(),
                PreviewRequestV1::new(unregistered_setup)
            )
            .is_err()
    );
    assert_eq!(ports.counters(), before);
}

#[test]
fn compute_price_override_is_exact_and_catalog_disable_requires_a_rule() {
    let ports = MemoryPorts::default();
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let spec = hiroute_domain::ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: "prices.override.apply".into(),
        resource_id: Some("personal/default".into()),
        desired_state: json!({
            "override_id": "override/model-one",
            "offer_ref": "offer/model-one",
            "model_configuration_id": "model.one",
            "currency": "USD",
            "operation": "replace",
            "input_value": 70,
            "output_value": 140,
            "expected_override_revision": 0,
        }),
    };
    coordinator
        .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec.clone()))
        .unwrap();
    assert_eq!(ports.counters(), (0, 0, 0, 0));

    let mut invalid = spec.clone();
    invalid.desired_state["operation"] = json!("disable_catalog_rule");
    invalid
        .desired_state
        .as_object_mut()
        .unwrap()
        .remove("input_value");
    invalid
        .desired_state
        .as_object_mut()
        .unwrap()
        .remove("output_value");
    assert!(
        coordinator
            .preview(&WorkspaceId::default(), PreviewRequestV1::new(invalid))
            .is_err()
    );
    assert_eq!(ports.counters(), (0, 0, 0, 0));

    let mut unknown = spec;
    unknown.desired_state["offer_ref"] = json!("offer/unknown");
    assert!(
        coordinator
            .preview(&WorkspaceId::default(), PreviewRequestV1::new(unknown))
            .is_err()
    );
    assert_eq!(ports.counters(), (0, 0, 0, 0));
}

#[test]
fn compute_paid_connection_is_explicit_and_rotate_waits_for_safe_probe_adapter() {
    let ports = existing_compute_ports(b"never-read");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let mut connection = hiroute_domain::ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: "compute.connection.apply".into(),
        resource_id: Some("personal/default".into()),
        desired_state: json!({
            "connection_option_id": "bailian.payg.cn.v1",
            "source_id": "source/bailian-new",
            "explicit_materialization": false,
            "expected_source_revision": 0,
        }),
    };
    assert!(
        coordinator
            .preview(
                &WorkspaceId::default(),
                PreviewRequestV1::new(connection.clone())
            )
            .is_err()
    );
    connection.desired_state["explicit_materialization"] = json!(true);
    coordinator
        .preview(&WorkspaceId::default(), PreviewRequestV1::new(connection))
        .unwrap();

    let mut rotate = compute_credential_change("stdin");
    rotate.command_id = "compute.credential.rotate".into();
    assert!(
        coordinator
            .preview(&WorkspaceId::default(), PreviewRequestV1::new(rotate))
            .is_err()
    );
    assert_eq!(ports.counters(), (0, 0, 0, 0));
}
