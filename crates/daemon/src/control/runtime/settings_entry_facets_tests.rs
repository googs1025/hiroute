//! Claude and Skill-specific public V2 settings entry scenarios.
use super::*;
use crate::control::runtime::native_claude_model::is_settings_claude_model;

#[test]
fn v2_settings_skill_only_works_before_the_first_publication() {
    if crate::test_support::isolated_agent_home(
        "control::runtime::native_model::tests::settings_entry_tests::facets::v2_settings_skill_only_works_before_the_first_publication",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let executable = root.path().join("codex-fixture");
    fs::write(&executable, b"#!/bin/sh\nprintf 'codex-cli 99.99.99\\n'\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let mut layout = AgentFilesystemLayoutV1::from_process(&home, root.path());
    layout.codex_executable = executable;
    layout.claude_executable = root.path().join("missing-claude");
    let registry = serde_json::from_slice(include_bytes!(
        "../../../../../assets/connector-registry/current/registry-seed.json"
    ))
    .unwrap();
    let models: hiroute_domain::ReleaseModelDataBundleV2 = serde_json::from_slice(include_bytes!(
        "../../../../../assets/release-facts/current/bundle/model-data.json"
    ))
    .unwrap();
    let scanner = FilesystemAgentScannerV1::new(
        layout,
        ClaudeRegistrationIndexV1::from_verified_model_data(&registry, &models.data).unwrap(),
    );
    let runtime = ProductionControlRuntime::prepare_for_role_all_with_scanner(
        root.path(),
        crate::release_catalog::fixture_catalog(),
        None,
        scanner,
    )
    .unwrap();
    *runtime.adapter.managed_agent_runtime.lock().unwrap() = Some(ManagedAgentRuntimeV1 {
        gateway_base_url: "http://127.0.0.1:5837/v1".into(),
        trusted_hiroute_executable: "/test/hiroute".into(),
        worker_executor_availability: Arc::new(
            crate::delegation::installation::WorkerExecutorAvailabilityRegistry::unconfigured(),
        ),
        resident_service_ready: false,
    });
    runtime.adapter.reconcile_startup_and_open().unwrap();
    assert!(
        runtime
            .adapter
            .stores_lock()
            .unwrap()
            .control()
            .active_publication(&WorkspaceId::default())
            .unwrap()
            .is_none()
    );

    let ordinary = LocalControlDaemon::new(ApplicationService::new(runtime.application_ports()));
    let scan = ordinary
        .dispatch_wire(request("ScanAgents", json!({}), None))
        .data
        .unwrap();
    let context = scan["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == "agent_codex_default")
        .unwrap()["context_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let spec = json!({
        "schema_version":{"major":2,"minor":0}, "context_id":context,
        "collaboration":{"intent":"configure","settings":{"trigger_mode":"explicit"}}
    });
    let unproven = ordinary.dispatch_wire(request(
        "PreviewAgentConnectionChange",
        json!({"spec":spec}),
        None,
    ));
    assert!(unproven.error.is_none(), "{unproven:?}");
    assert_eq!(unproven.data.unwrap()["applicable"], false);

    let service =
        LocalControlDaemon::new(ApplicationService::new(
            runtime.application_ports().with_agent_connection(Arc::new(
                FixtureFacts::collaboration(runtime.adapter.clone()),
            )),
        ));
    apply(&service, &runtime, spec, "skill-before-publication");
    assert!(
        home.join(".agents/skills/hiroute-collaboration/SKILL.md")
            .is_file()
    );
    assert!(
        runtime
            .adapter
            .stores_lock()
            .unwrap()
            .control()
            .active_publication(&WorkspaceId::default())
            .unwrap()
            .is_none(),
        "installing the Skill must not manufacture an empty publication"
    );
}

#[test]
fn v2_settings_dispatch_claude_configures_and_formally_restores_owned_user_file() {
    if crate::test_support::isolated_agent_home(
        "control::runtime::native_model::tests::settings_entry_tests::facets::v2_settings_dispatch_claude_configures_and_formally_restores_owned_user_file",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let settings = home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::set_permissions(
        settings.parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let original = json!({
        "theme": "dark",
        "apiKeyHelper": "user-owned-helper --do-not-run",
        "env": {
            "ANTHROPIC_BASE_URL": "https://open.bigmodel.cn/api/anthropic",
            "ANTHROPIC_MODEL": "opus",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.3[1m]",
            "ANTHROPIC_AUTH_TOKEN": "fixture-user-token",
            "UNRELATED": "keep-me"
        }
    });
    fs::write(&settings, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();
    let executable = root.path().join("claude-fixture");
    fs::write(
        &executable,
        b"#!/bin/sh\nprintf '2.1.231 (Claude Code)\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

    let mut layout = AgentFilesystemLayoutV1::from_process(&home, root.path());
    layout.codex_executable = root.path().join("missing-codex");
    layout.claude_executable = executable.clone();
    layout.claude_launch_settings = None;
    layout.claude_project_settings.clear();
    layout.claude_user_settings = settings.clone();
    layout.claude_managed_settings.clear();
    let registry = serde_json::from_slice(include_bytes!(
        "../../../../../assets/connector-registry/current/registry-seed.json"
    ))
    .unwrap();
    let models: hiroute_domain::ReleaseModelDataBundleV2 = serde_json::from_slice(include_bytes!(
        "../../../../../assets/release-facts/current/bundle/model-data.json"
    ))
    .unwrap();
    let scanner = FilesystemAgentScannerV1::new(
        layout,
        ClaudeRegistrationIndexV1::from_verified_model_data(&registry, &models.data).unwrap(),
    );
    let runtime = open_with_scanner(root.path(), scanner);
    runtime.adapter.reconcile_startup_and_open().unwrap();

    let ordinary = LocalControlDaemon::new(ApplicationService::new(runtime.application_ports()));
    let scan = ordinary
        .dispatch_wire(request("ScanAgents", json!({}), None))
        .data
        .unwrap();
    let claude = scan["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == "agent_claude_default")
        .unwrap();
    assert_eq!(
        claude["supported"], true,
        "the V2 entry does not gate Claude configuration on a version whitelist"
    );
    let context = claude["context_id"].as_str().unwrap().to_owned();
    let plans = runtime
        .adapter
        .stores_lock()
        .unwrap()
        .control()
        .active_publication(&WorkspaceId::default())
        .unwrap()
        .unwrap()
        .verify()
        .unwrap()
        .published_agent_plans()
        .unwrap();
    let plan = plans
        .iter()
        .find(|plan| {
            plan.active
                && plan
                    .supported_ingress
                    .contains(&AgentIngressProtocolV1::Messages)
        })
        .unwrap();
    let extra_plan = plans
        .iter()
        .find(|candidate| {
            candidate.agent_plan_id != plan.agent_plan_id
                && candidate.active
                && candidate
                    .supported_ingress
                    .contains(&AgentIngressProtocolV1::Messages)
        })
        .unwrap();
    let spec = json!({
        "schema_version":{"major":2,"minor":0}, "context_id":context,
        "model":{"intent":"configure","settings":{
            "mode":"claude_launcher","surfaces":["claude_cli"],"fixed_models":[],
            "preset_mappings":{
                "opus":{"kind":"plan","plan_id":plan.agent_plan_id},
                "sonnet":{"kind":"preserve_native"},
                "haiku":{"kind":"preserve_native"}}}}
    });
    let unproven = ordinary.dispatch_wire(request(
        "PreviewAgentConnectionChange",
        json!({"spec":spec}),
        None,
    ));
    assert!(unproven.error.is_none(), "{unproven:?}");
    let unproven = unproven.data.unwrap();
    assert_eq!(unproven["model_effect"]["agent_class"], "claude");
    assert_eq!(unproven["applicable"], false);

    // The ordinary discovery is allowed to report a broken diagnostic version probe, but
    // saving the already located Claude settings must not turn that into a false 404.
    let original_executable = fs::read(&executable).unwrap();
    fs::write(&executable, b"#!/bin/sh\nexit 9\n").unwrap();
    let diagnostic_scan = ordinary.dispatch_wire(request("ScanAgents", json!({}), None));
    assert!(diagnostic_scan.error.is_none(), "{diagnostic_scan:?}");
    let diagnostic_agents = diagnostic_scan.data.unwrap();
    assert!(
        diagnostic_agents["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| {
                agent["agent_id"] == "agent_claude_default" && agent["supported"] == false
            })
    );
    let settings_preview = ordinary.dispatch_wire(request(
        "PreviewAgentConnectionChange",
        json!({"spec":spec}),
        None,
    ));
    assert!(settings_preview.error.is_none(), "{settings_preview:?}");
    fs::write(&executable, original_executable).unwrap();

    let service = LocalControlDaemon::new(ApplicationService::new(
        runtime
            .application_ports()
            .with_agent_connection(Arc::new(FixtureFacts::model(runtime.adapter.clone()))),
    ));
    // The first resident-service connection: the preview announces the login item, an apply
    // without the host's declaration stays service_unavailable, and the sealed operation
    // journals the declared before/after as a LoginItem owned effect.
    let guard_preview = service.dispatch_wire(request(
        "PreviewAgentConnectionChange",
        json!({"spec":spec}),
        None,
    ));
    assert!(guard_preview.error.is_none(), "{guard_preview:?}");
    let guard_preview = guard_preview.data.unwrap();
    assert_eq!(
        guard_preview["resident_service"]["login_item_required"],
        true
    );
    let guard_key = "claude-settings-login-item-guard";
    let guard_payload = json!({
        "spec":spec,
        "accept_digest":guard_preview["accept_digest"],
        "dependency_digest":guard_preview["dependency_digest"],
        "expected_revisions":guard_preview["expected_revisions"],
        "idempotency_key":guard_key,
    });
    let rejected =
        service.dispatch_wire(request("ApplyAgentConnectionChange", guard_payload, None));
    assert_eq!(
        rejected.error.unwrap().code,
        api::ErrorCode::GatewayUnavailable,
        "a first resident-service connection without a host login-item declaration cannot complete"
    );
    let first = apply(&service, &runtime, spec.clone(), "claude-settings-first");
    let first_operation = runtime
        .adapter
        .stores_lock()
        .unwrap()
        .control()
        .load_operation(
            &OperationId::parse(first["operation_id"].as_str().unwrap().to_owned()).unwrap(),
        )
        .unwrap()
        .unwrap();
    let login_effect = first_operation
        .steps
        .iter()
        .flat_map(|step| step.effects.iter())
        .find(|effect| effect.kind == hiroute_domain::OwnedEffectKind::LoginItem)
        .expect("the first connection journals its login-item evidence");
    assert_eq!(login_effect.effect_id, "agent-connection-login-item");
    assert_ne!(
        login_effect.before_fingerprint, login_effect.after_fingerprint,
        "the effect binds the observed not_registered-to-enabled transition"
    );
    assert_eq!(
        *login_effect.compensation,
        json!({"revert":"unregister","owner":"desktop-host"}),
        "only this operation's creation is compensable"
    );
    // The owned launch snapshot is the only file this transaction writes; the user's own
    // settings stay byte-for-byte theirs.
    let snapshot_target = first_operation
        .plan
        .external()
        .iter()
        .find(|intent| intent.effect_id() == "agent-connection-managed-configuration")
        .unwrap()
        .target()
        .to_owned();
    let published_snapshot = |adapter: &LocalControlAdapter| {
        let bytes = adapter
            .artifacts
            .read_native_target(&snapshot_target)
            .unwrap()
            .unwrap_or_else(|| panic!("the owned launch snapshot must be published"));
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    };
    let snapshot = published_snapshot(&runtime.adapter);
    assert_eq!(snapshot["schema"], "hiroute.claude-launch-snapshot/v1");
    assert_eq!(snapshot["context_id"], context);
    assert_eq!(snapshot["gateway_base_url"], "http://127.0.0.1:5837/v1");
    assert_eq!(snapshot["trusted_hiroute_executable"], "/test/hiroute");
    assert!(
        serde_json::to_string(&snapshot)
            .unwrap()
            .find("fixture-user-token")
            .is_none(),
        "the snapshot carries the sealed grant scope, never user credentials"
    );
    // The managed launcher reads exactly that published snapshot: the descriptor carries its
    // three-slot routing facts, the grant generation, and the trusted helper, and derives no
    // routing from the user's own configuration.
    let launch_connection = format!("agent-connection/{context}");
    let launch_descriptor = service.dispatch_wire(request(
        "GetManagedAgentLaunchDescriptor",
        json!({"connection_id": launch_connection}),
        None,
    ));
    assert!(launch_descriptor.error.is_none(), "{launch_descriptor:?}");
    let launch_descriptor = launch_descriptor.data.unwrap();
    assert_eq!(
        launch_descriptor["schema"],
        "hiroute.managed-claude-launch-descriptor/v2"
    );
    assert_eq!(launch_descriptor["connection_id"], launch_connection);
    assert_eq!(
        launch_descriptor["gateway_base_url"],
        "http://127.0.0.1:5837"
    );
    assert_eq!(launch_descriptor["grant_generation"], 1);
    assert_eq!(
        launch_descriptor["presets"],
        snapshot["snapshot"]["presets"]
    );
    assert_eq!(
        launch_descriptor["helper_argv"],
        json!(["__internal-agent-grant-v1", launch_connection])
    );
    assert_eq!(
        launch_descriptor["executable"],
        snapshot["snapshot"]["executable"]
    );
    let stores = runtime.adapter.stores_lock().unwrap();
    let live_revision = stores
        .control()
        .active_publication(&WorkspaceId::default())
        .unwrap()
        .unwrap()
        .publication_revision;
    let live_revisions = stores
        .control()
        .current_revisions(&WorkspaceId::default())
        .unwrap();
    let live_payload = json!({
        "agent_id":"agent_claude_default", "scope":"live", "suite":"quick",
        "allow_model_call":true,
        "target":{
            "context_id":context, "surface":"claude_cli",
            "expected_applied_revision":live_revision,
            "client_model_ids":[launch_descriptor["presets"]["opus"]]
        }
    });
    let live_request: api::AgentCheckRequestV1 =
        serde_json::from_value(live_payload.clone()).unwrap();
    let live_capability = "claude-live-check-one-shot-capability".to_owned();
    stores
        .apply_capability_registrar()
        .register(
            ApplyCapabilityRegistrationV1::from_protected_launcher(
                live_capability.clone(),
                "interactive-user",
                WorkspaceId::default(),
                "CheckAgentConnection",
                CanonicalDigest::of(&live_request).unwrap(),
                live_revisions.clone(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64
                    + 60,
            )
            .unwrap(),
        )
        .unwrap();
    drop(stores);
    for (field, value, expected) in [
        (
            "surface",
            json!("codex_desktop"),
            api::ErrorCode::CapabilityDenied,
        ),
        (
            "context_id",
            json!("agent-context/other"),
            api::ErrorCode::CapabilityDenied,
        ),
        (
            "client_model_ids",
            json!(["unpublished-model"]),
            api::ErrorCode::CapabilityDenied,
        ),
        (
            "expected_applied_revision",
            json!(live_revision.get() + 1),
            api::ErrorCode::ChangePreviewStale,
        ),
    ] {
        let mut invalid_payload = live_payload.clone();
        invalid_payload["target"][field] = value;
        let invalid_request: api::AgentCheckRequestV1 =
            serde_json::from_value(invalid_payload.clone()).unwrap();
        let digest = CanonicalDigest::of(&invalid_request).unwrap();
        let capability = format!("claude-live-check-invalid-target-capability-{field}");
        runtime
            .adapter
            .stores_lock()
            .unwrap()
            .apply_capability_registrar()
            .register(
                ApplyCapabilityRegistrationV1::from_protected_launcher(
                    capability.clone(),
                    "interactive-user",
                    WorkspaceId::default(),
                    "CheckAgentConnection",
                    digest.clone(),
                    live_revisions.clone(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64
                        + 60,
                )
                .unwrap(),
            )
            .unwrap();
        let rejected = ordinary.dispatch_wire(request(
            "CheckAgentConnection",
            invalid_payload,
            Some(capability.clone()),
        ));
        assert_eq!(rejected.error.unwrap().code, expected, "{field}");
        hiroute_application::control::ControlStatePort::validate_protected_capability(
            runtime.adapter.as_ref(),
            &capability,
            &WorkspaceId::default(),
            api::PrincipalKind::InteractiveUser,
            "CheckAgentConnection",
            &digest,
            &live_revisions,
        )
        .expect("invalid targets must be rejected before consuming model-call consent");
    }
    let checked = ordinary.dispatch_wire(request(
        "CheckAgentConnection",
        live_payload.clone(),
        Some(live_capability.clone()),
    ));
    assert!(checked.error.is_none(), "{checked:?}");
    let checked = checked.data.unwrap();
    assert_eq!(checked["state"], "failed");
    assert_eq!(checked["reason_code"], "LIVE_CLIENT_OUTPUT_INVALID");
    assert_eq!(checked["call_count"], 1);
    let replayed = ordinary.dispatch_wire(request(
        "CheckAgentConnection",
        live_payload,
        Some(live_capability),
    ));
    assert_eq!(
        replayed.error.unwrap().code,
        api::ErrorCode::CapabilityDenied
    );
    let surface_checks = runtime
        .adapter
        .stores_lock()
        .unwrap()
        .control()
        .agent_surface_checks(&WorkspaceId::default(), &context)
        .unwrap();
    assert_eq!(surface_checks.len(), 1);
    assert_eq!(surface_checks[0].surface, AgentModelSurfaceV2::ClaudeCli);
    assert_eq!(surface_checks[0].state, AgentSurfaceCheckStateV1::Failed);
    assert_eq!(
        surface_checks[0].reason_code.as_deref(),
        Some("LIVE_CLIENT_OUTPUT_INVALID")
    );
    let connection_id = format!("agent-connection/{context}");
    let first_material = runtime
        .adapter
        .resolve_active_agent_grant(&connection_id)
        .expect("configured V2 settings must expose their exact grant through the owner socket");
    let executable_bytes = fs::read(&executable).unwrap();
    let user_bytes = fs::read(&settings).unwrap();
    fs::write(&executable, b"#!/bin/sh\nexit 99\n").unwrap();
    fs::write(&settings, b"not valid settings JSON").unwrap();
    let after_drift = service.dispatch_wire(request(
        "GetManagedAgentLaunchDescriptor",
        json!({"connection_id": launch_connection}),
        None,
    ));
    let drift_material = runtime.adapter.resolve_active_agent_grant(&connection_id);
    fs::write(&executable, executable_bytes).unwrap();
    fs::write(&settings, user_bytes).unwrap();
    assert!(after_drift.error.is_none(), "{after_drift:?}");
    assert_eq!(after_drift.data.unwrap(), launch_descriptor);
    assert_eq!(
        drift_material
            .expect(
                "the helper uses published authority despite native executable and settings drift"
            )
            .sha256(),
        first_material.sha256()
    );
    let configured: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(
        configured, original,
        "a Claude settings transaction must not rewrite the user's own settings file"
    );
    let reused = apply(&service, &runtime, spec.clone(), "claude-settings-reuse");
    assert_ne!(reused["operation_id"], first["operation_id"]);
    let reused_operation: OperationId =
        serde_json::from_value(reused["operation_id"].clone()).unwrap();
    let reused_material = runtime
        .adapter
        .resolve_active_agent_grant(&connection_id)
        .expect("an identical settings apply must keep exposing the reused grant");
    assert_eq!(reused_material.sha256(), first_material.sha256());
    assert_eq!(
        runtime
            .adapter
            .stores_lock()
            .unwrap()
            .secrets()
            .agent_access_grant_generation(
                WorkspaceId::DEFAULT,
                &format!("agent-connection/{context}")
            )
            .unwrap(),
        1,
        "an identical settings apply reuses the current grant generation"
    );

    let update_spec = json!({
        "schema_version":{"major":2,"minor":0}, "context_id":context,
        "model":{"intent":"configure","settings":{
            "mode":"claude_launcher","surfaces":["claude_cli"],"fixed_models":[],
            "preset_mappings":{
                "opus":{"kind":"plan","plan_id":plan.agent_plan_id},
                "sonnet":{"kind":"plan","plan_id":extra_plan.agent_plan_id},
                "haiku":{"kind":"preserve_native"}}}}
    });
    let updated = apply(&service, &runtime, update_spec, "claude-settings-update");
    let operation: OperationId = serde_json::from_value(updated["operation_id"].clone()).unwrap();
    let update_journal = runtime
        .adapter
        .stores_lock()
        .unwrap()
        .control()
        .load_operation(&operation)
        .unwrap()
        .unwrap();
    let update_intent = update_journal
        .plan
        .external()
        .iter()
        .find(|intent| is_settings_claude_model(intent))
        .unwrap()
        .clone();
    let update_payload =
        settings_claude_model_file_for_operation(&update_journal, &update_intent).unwrap();
    let ClaudeModelFileAction::Configure {
        previous_operation, ..
    } = update_payload.change
    else {
        panic!("the update must be a Claude model configuration");
    };
    assert_eq!(previous_operation, Some(reused_operation));
    let updated_material = runtime
        .adapter
        .resolve_active_agent_grant(&connection_id)
        .expect("updated V2 settings must rotate their exact grant");
    assert_ne!(first_material.sha256(), updated_material.sha256());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&settings).unwrap()).unwrap(),
        original,
        "an update still never touches the user's own settings file"
    );

    let restore_spec = json!({
        "schema_version":{"major":2,"minor":0}, "context_id":context,
        "model":{"intent":"restore","restore_point_ref":codex_model_restore_point_ref(&operation)}
    });
    // The last managed connection's restore must also release the owned login item: a
    // preview announces the removal, and an apply without the host's removal declaration
    // stays service_unavailable exactly like an unconfirmed establishment.
    let removal_preview = service.dispatch_wire(request(
        "PreviewAgentConnectionRestore",
        json!({"spec":restore_spec}),
        None,
    ));
    assert!(removal_preview.error.is_none(), "{removal_preview:?}");
    let removal_preview = removal_preview.data.unwrap();
    assert_eq!(
        removal_preview["resident_service"]["login_item_removal_required"], true,
        "the last managed connection announces its login-item release"
    );
    let removal_key = "claude-settings-login-item-removal-guard";
    let unconfirmed = service.dispatch_wire(request(
        "ApplyAgentConnectionRestore",
        json!({
            "spec":restore_spec,
            "accept_digest":removal_preview["accept_digest"],
            "dependency_digest":removal_preview["dependency_digest"],
            "expected_revisions":removal_preview["expected_revisions"],
            "idempotency_key":removal_key,
        }),
        None,
    ));
    assert_eq!(
        unconfirmed.error.unwrap().code,
        api::ErrorCode::GatewayUnavailable,
        "a restore that leaves the owned login item registered cannot complete"
    );
    assert!(
        runtime
            .adapter
            .resolve_active_agent_grant(&connection_id)
            .is_ok(),
        "the rejected restore keeps the connection's grant"
    );
    let restore = apply(&service, &runtime, restore_spec, "claude-settings-restore");
    let restore_operation = runtime
        .adapter
        .stores_lock()
        .unwrap()
        .control()
        .load_operation(
            &OperationId::parse(restore["operation_id"].as_str().unwrap().to_owned()).unwrap(),
        )
        .unwrap()
        .unwrap();
    let removal_effect = restore_operation
        .steps
        .iter()
        .flat_map(|step| step.effects.iter())
        .find(|effect| effect.kind == hiroute_domain::OwnedEffectKind::LoginItem)
        .expect("the restore journals its login-item removal evidence");
    assert_eq!(removal_effect.effect_id, "agent-connection-login-item");
    assert_eq!(
        *removal_effect.compensation,
        json!({"revert":"register","owner":"desktop-host"}),
        "a failed restore replays the re-registration of the released owned item"
    );
    let restored: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(
        restored, original,
        "a formal restore never rewrites the user's own settings file"
    );
    assert!(
        runtime
            .adapter
            .artifacts
            .read_native_target(&snapshot_target)
            .unwrap()
            .is_none(),
        "the formal restore removes the owned launch snapshot"
    );
    assert!(
        runtime
            .adapter
            .resolve_active_agent_grant(&connection_id)
            .is_err(),
        "a formal restore withdraws the exposed grant"
    );
    let withdrawn = service.dispatch_wire(request(
        "GetManagedAgentLaunchDescriptor",
        json!({"connection_id": connection_id}),
        None,
    ));
    assert!(
        withdrawn.error.is_some(),
        "a restored connection without an active grant has no launch descriptor"
    );
    let again = apply(&service, &runtime, spec.clone(), "claude-settings-again");
    let next_material = runtime
        .adapter
        .resolve_active_agent_grant(&connection_id)
        .expect("reconfigured V2 settings must expose the new exact grant");
    assert_ne!(updated_material.sha256(), next_material.sha256());
    let republished = published_snapshot(&runtime.adapter);
    assert_eq!(republished["schema"], "hiroute.claude-launch-snapshot/v1");
    assert_eq!(
        runtime
            .adapter
            .stores_lock()
            .unwrap()
            .secrets()
            .agent_access_grant_generation(
                WorkspaceId::DEFAULT,
                &format!("agent-connection/{context}")
            )
            .unwrap(),
        4
    );

    // The reconfigured connection created the item again, so its restore releases the owned
    // item once more before the ownership boundary below.
    let again_id: OperationId = serde_json::from_value(again["operation_id"].clone()).unwrap();
    let second_restore_spec = json!({
        "schema_version":{"major":2,"minor":0}, "context_id":context,
        "model":{"intent":"restore","restore_point_ref":codex_model_restore_point_ref(&again_id)}
    });
    let second_restore_preview = service.dispatch_wire(request(
        "PreviewAgentConnectionRestore",
        json!({"spec":second_restore_spec}),
        None,
    ));
    assert!(
        second_restore_preview.error.is_none(),
        "{second_restore_preview:?}"
    );
    assert_eq!(
        second_restore_preview.data.unwrap()["resident_service"]["login_item_removal_required"],
        true,
        "the re-created login item is owned by this feature again"
    );
    apply(
        &service,
        &runtime,
        second_restore_spec,
        "claude-settings-second-restore",
    );
    assert!(
        runtime
            .adapter
            .resolve_active_agent_grant(&connection_id)
            .is_err()
    );

    // The ownership boundary: a connection established over the user's own pre-existing item
    // is never owned, so its restore neither requires nor journals a login-item release.
    let user_item = apply_with_host_login_item(
        &service,
        &runtime,
        spec,
        "claude-settings-user-item",
        Some(json!({"before":"enabled","after":"enabled","created":false})),
    );
    let user_item_id: OperationId =
        serde_json::from_value(user_item["operation_id"].clone()).unwrap();
    let user_item_operation = runtime
        .adapter
        .stores_lock()
        .unwrap()
        .control()
        .load_operation(&user_item_id)
        .unwrap()
        .unwrap();
    let user_item_effect = user_item_operation
        .steps
        .iter()
        .flat_map(|step| step.effects.iter())
        .find(|effect| effect.kind == hiroute_domain::OwnedEffectKind::LoginItem)
        .expect("the connection journals the user's pre-existing item without owning it");
    assert_eq!(*user_item_effect.compensation, json!({}));

    let final_restore_spec = json!({
        "schema_version":{"major":2,"minor":0}, "context_id":context,
        "model":{"intent":"restore","restore_point_ref":codex_model_restore_point_ref(&user_item_id)}
    });
    let final_preview = service.dispatch_wire(request(
        "PreviewAgentConnectionRestore",
        json!({"spec":final_restore_spec}),
        None,
    ));
    assert!(final_preview.error.is_none(), "{final_preview:?}");
    assert_eq!(
        final_preview.data.unwrap()["resident_service"]["login_item_removal_required"],
        false,
        "a pre-existing user login item is never owned and never removed"
    );
    let final_restore = apply(
        &service,
        &runtime,
        final_restore_spec,
        "claude-settings-final-restore",
    );
    let final_restore_operation = runtime
        .adapter
        .stores_lock()
        .unwrap()
        .control()
        .load_operation(
            &OperationId::parse(final_restore["operation_id"].as_str().unwrap().to_owned())
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    assert!(
        final_restore_operation
            .steps
            .iter()
            .flat_map(|step| step.effects.iter())
            .all(|effect| effect.kind != hiroute_domain::OwnedEffectKind::LoginItem),
        "the last restore leaves the user's own login item untouched"
    );
    assert!(
        runtime
            .adapter
            .resolve_active_agent_grant(&connection_id)
            .is_err()
    );
}

#[path = "settings_entry_collaboration_tests.rs"]
mod collaboration;
