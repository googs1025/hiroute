use crate::fixture::*;

#[test]
fn config_atomicity_group_rejects_partial_update_before_single_pointer_swap() {
    let descriptors = [
        ConfigCellDescriptor {
            id: ConfigCellId(1),
            compatibility_hash: [1; 32],
            atomicity_group: AtomicityGroupId(9),
            binding_policy: ConfigBindingPolicy::RequestPinned,
        },
        ConfigCellDescriptor {
            id: ConfigCellId(2),
            compatibility_hash: [2; 32],
            atomicity_group: AtomicityGroupId(9),
            binding_policy: ConfigBindingPolicy::EventLive,
        },
    ];
    let config = |id, generation, compatibility_hash| ImmutableConfig {
        generation: ConfigGeneration(generation),
        compatibility_hash,
        bytes: Arc::from([id as u8, generation as u8]),
    };
    let initial = Arc::new(ConfigBundle::new(
        AtomicityGroupId(9),
        std::collections::HashMap::from([
            (ConfigCellId(1), config(1, 1, [1; 32])),
            (ConfigCellId(2), config(2, 1, [2; 32])),
        ]),
    ));
    let group = ConfigCellGroup::new(descriptors, initial).unwrap();
    let first = group.handle(ConfigCellId(1)).unwrap();
    let second = group.handle(ConfigCellId(2)).unwrap();

    let partial = Arc::new(ConfigBundle::new(
        AtomicityGroupId(9),
        std::collections::HashMap::from([(ConfigCellId(1), config(1, 2, [1; 32]))]),
    ));
    assert_eq!(
        group.publish(partial).unwrap_err(),
        PlanError::MissingConfigCell(ConfigCellId(2))
    );
    assert_eq!(
        first.acquire_request().unwrap().value().generation,
        ConfigGeneration(1)
    );
    assert_eq!(
        second.acquire_event().unwrap().value().generation,
        ConfigGeneration(1)
    );

    let torn = Arc::new(ConfigBundle::new(
        AtomicityGroupId(9),
        std::collections::HashMap::from([
            (ConfigCellId(1), config(1, 2, [1; 32])),
            (ConfigCellId(2), config(2, 1, [2; 32])),
        ]),
    ));
    assert_eq!(
        group.publish(torn).unwrap_err(),
        PlanError::MixedConfigGeneration
    );
    assert_eq!(
        first.acquire_request().unwrap().value().generation,
        ConfigGeneration(1)
    );

    group
        .publish(Arc::new(ConfigBundle::new(
            AtomicityGroupId(9),
            std::collections::HashMap::from([
                (ConfigCellId(1), config(1, 2, [1; 32])),
                (ConfigCellId(2), config(2, 2, [2; 32])),
            ]),
        )))
        .unwrap();
    assert_eq!(
        first.acquire_request().unwrap().value().generation,
        ConfigGeneration(2)
    );
    assert_eq!(
        second.acquire_event().unwrap().value().generation,
        ConfigGeneration(2)
    );

    let stale = Arc::new(ConfigBundle::new(
        AtomicityGroupId(9),
        std::collections::HashMap::from([
            (ConfigCellId(1), config(1, 1, [1; 32])),
            (ConfigCellId(2), config(2, 1, [2; 32])),
        ]),
    ));
    assert_eq!(
        second.publish(stale).unwrap_err(),
        PlanError::StaleConfigGeneration {
            active: ConfigGeneration(2),
            candidate: ConfigGeneration(1),
        }
    );

    let same_generation_different_values = Arc::new(ConfigBundle::new(
        AtomicityGroupId(9),
        std::collections::HashMap::from([
            (
                ConfigCellId(1),
                ImmutableConfig {
                    bytes: Arc::from([99]),
                    ..config(1, 2, [1; 32])
                },
            ),
            (ConfigCellId(2), config(2, 2, [2; 32])),
        ]),
    ));
    assert_eq!(
        group.publish(same_generation_different_values).unwrap_err(),
        PlanError::ConfigGenerationConflict(ConfigGeneration(2))
    );
    assert_eq!(
        first.acquire_request().unwrap().value().bytes.as_ref(),
        [1, 2]
    );
    assert!(matches!(
        first.acquire_event(),
        Err(PlanError::ConfigBindingPolicyMismatch { .. })
    ));
    assert!(matches!(
        second.acquire_request(),
        Err(PlanError::ConfigBindingPolicyMismatch { .. })
    ));
}

#[test]
fn group_snapshot_cannot_mix_generations_when_publish_occurs_between_value_lookups() {
    let descriptors = [
        ConfigCellDescriptor {
            id: ConfigCellId(11),
            compatibility_hash: [11; 32],
            atomicity_group: AtomicityGroupId(91),
            binding_policy: ConfigBindingPolicy::AttemptPinned,
        },
        ConfigCellDescriptor {
            id: ConfigCellId(12),
            compatibility_hash: [12; 32],
            atomicity_group: AtomicityGroupId(91),
            binding_policy: ConfigBindingPolicy::AttemptPinned,
        },
    ];
    let bundle = |generation| {
        Arc::new(ConfigBundle::new(
            AtomicityGroupId(91),
            std::collections::HashMap::from([
                (
                    ConfigCellId(11),
                    ImmutableConfig {
                        generation: ConfigGeneration(generation),
                        compatibility_hash: [11; 32],
                        bytes: Arc::from([11, generation as u8]),
                    },
                ),
                (
                    ConfigCellId(12),
                    ImmutableConfig {
                        generation: ConfigGeneration(generation),
                        compatibility_hash: [12; 32],
                        bytes: Arc::from([12, generation as u8]),
                    },
                ),
            ]),
        ))
    };
    let group = ConfigCellGroup::new(descriptors, bundle(1)).unwrap();
    let snapshot = group.acquire_attempt_snapshot().unwrap();
    assert_eq!(
        snapshot.value(ConfigCellId(11)).unwrap().generation,
        ConfigGeneration(1)
    );
    let publisher = group.clone();
    std::thread::spawn(move || publisher.publish(bundle(2)))
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.value(ConfigCellId(12)).unwrap().generation,
        ConfigGeneration(1),
        "one group lease retains one immutable bundle across both lookups"
    );
    let fresh = group.acquire_attempt_snapshot().unwrap();
    assert_eq!(
        fresh
            .generations()
            .map(|(_, generation)| generation)
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from([ConfigGeneration(2)])
    );
}

#[test]
fn all_five_config_scopes_are_enforced_by_distinct_acquire_apis() {
    let policies = [
        ConfigBindingPolicy::ConnectionPinned,
        ConfigBindingPolicy::RequestPinned,
        ConfigBindingPolicy::AttemptPinned,
        ConfigBindingPolicy::PhasePinned,
        ConfigBindingPolicy::EventLive,
    ];
    let descriptors = policies
        .iter()
        .enumerate()
        .map(|(index, policy)| ConfigCellDescriptor {
            id: ConfigCellId(index as u64 + 1),
            compatibility_hash: [index as u8 + 1; 32],
            atomicity_group: AtomicityGroupId(20),
            binding_policy: *policy,
        });
    let values = policies
        .iter()
        .enumerate()
        .map(|(index, _)| {
            (
                ConfigCellId(index as u64 + 1),
                ImmutableConfig {
                    generation: ConfigGeneration(1),
                    compatibility_hash: [index as u8 + 1; 32],
                    bytes: Arc::from([index as u8]),
                },
            )
        })
        .collect();
    let group = ConfigCellGroup::new(
        descriptors,
        Arc::new(ConfigBundle::new(AtomicityGroupId(20), values)),
    )
    .unwrap();
    assert!(
        group
            .handle(ConfigCellId(1))
            .unwrap()
            .acquire_connection()
            .is_ok()
    );
    assert!(
        group
            .handle(ConfigCellId(2))
            .unwrap()
            .acquire_request()
            .is_ok()
    );
    assert!(
        group
            .handle(ConfigCellId(3))
            .unwrap()
            .acquire_attempt()
            .is_ok()
    );
    assert!(
        group
            .handle(ConfigCellId(4))
            .unwrap()
            .acquire_phase()
            .is_ok()
    );
    assert!(
        group
            .handle(ConfigCellId(5))
            .unwrap()
            .acquire_event()
            .is_ok()
    );
    assert!(matches!(
        group.handle(ConfigCellId(1)).unwrap().acquire_attempt(),
        Err(PlanError::ConfigBindingPolicyMismatch { .. })
    ));
}
