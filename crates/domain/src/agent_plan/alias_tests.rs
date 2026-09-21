use super::*;

#[test]
fn custom_alias_first_publish_is_stable_unique_and_tombstoned() {
    let mut registry = AliasRegistryV1::default();
    let first = AgentPlanId::parse("plan/a").unwrap();
    let second = AgentPlanId::parse("plan/b").unwrap();
    let alias = ModelAlias::parse_custom("hiroute-coding").unwrap();
    assert_eq!(
        registry
            .allocate_custom(first.clone(), alias.clone())
            .unwrap(),
        alias
    );
    assert_eq!(registry.allocate(first.clone()).unwrap(), alias);
    let before = registry.clone();
    assert_eq!(
        registry.allocate_custom(second.clone(), alias.clone()),
        Err(AgentPlanIdentityError::AliasReused)
    );
    assert_eq!(registry, before);
    assert_eq!(
        registry.allocate_custom(
            first.clone(),
            ModelAlias::parse_custom("different").unwrap()
        ),
        Err(AgentPlanIdentityError::AliasReused)
    );
    registry.retire(&first).unwrap();
    assert_eq!(
        registry.allocate_custom(second, alias),
        Err(AgentPlanIdentityError::AliasReused)
    );
    registry.validate().unwrap();
}

#[test]
fn custom_alias_grammar_is_exact_without_normalization() {
    for good in ["a", "0", "hiroute-coding", "model.a_b-1"] {
        assert!(ModelAlias::parse_custom(good).is_ok(), "{good}");
        assert!(ModelAlias::parse(good).is_ok(), "{good}");
    }
    for bad in ["", "A", " a", "a ", "a/b", "_a", "-a", ".a", "模型", "a\n"] {
        assert!(ModelAlias::parse_custom(bad).is_err(), "{bad:?}");
    }
    assert!(ModelAlias::parse_custom("a".repeat(65)).is_err());
    assert!(ModelAlias::parse_custom("a".repeat(64)).is_ok());
    assert!(ModelAlias::parse("hiroute/0011223344556677").is_ok());
    assert!(ModelAlias::parse_custom("hiroute/0011223344556677").is_err());
}

#[test]
fn readable_defaults_use_slug_or_number_and_never_repurpose_legacy_hex() {
    let mut registry = AliasRegistryV1::default();
    let coding = registry
        .allocate_named(AgentPlanId::parse("internal/one").unwrap(), "Coding")
        .unwrap();
    assert_eq!(coding.as_str(), "hiroute-coding");
    assert_eq!(
        registry.suggest_alias("Coding").unwrap().as_str(),
        "hiroute-coding-2"
    );
    assert_eq!(
        registry.suggest_alias("hiroute-coding").unwrap().as_str(),
        "hiroute-coding-2"
    );
    let chinese = registry
        .allocate_named(AgentPlanId::parse("internal/two").unwrap(), "代码整理")
        .unwrap();
    assert_eq!(chinese.as_str(), "hiroute-daimazhengli");
    assert_eq!(
        registry.suggest_alias("😀").unwrap().as_str(),
        "hiroute-plan-1"
    );
    assert_eq!(
        registry
            .allocate_named(AgentPlanId::parse("internal/one").unwrap(), "Changed")
            .unwrap(),
        coding
    );
    registry
        .retire(&AgentPlanId::parse("internal/one").unwrap())
        .unwrap();
    assert_eq!(
        registry.suggest_alias("Coding").unwrap().as_str(),
        "hiroute-coding-2"
    );
    let legacy_id = AgentPlanId::parse("legacy/a").unwrap();
    let legacy = ModelAlias::parse("hiroute/0011223344556677").unwrap();
    registry.active.insert(legacy_id.clone(), legacy.clone());
    assert_eq!(
        registry.allocate_named(legacy_id, "Readable now").unwrap(),
        legacy
    );
    let long = registry.suggest_alias(&"a".repeat(128)).unwrap();
    assert_eq!(long.as_str().len(), 64);
    registry
        .allocate_custom(AgentPlanId::parse("long/a").unwrap(), long)
        .unwrap();
    let next = registry.suggest_alias(&"a".repeat(128)).unwrap();
    assert_eq!(next.as_str().len(), 64);
    assert!(next.as_str().ends_with("-2"));
}

#[test]
fn local_pinyin_defaults_are_readable_deterministic_and_keep_mixed_segments() {
    let registry = AliasRegistryV1::default();
    for (display, expected) in [
        ("日常编码", "hiroute-richangbianma"),
        ("资料整理", "hiroute-ziliaozhengli"),
        ("Code Review", "hiroute-code-review"),
        ("代码 Review", "hiroute-daima-review"),
        ("API编码", "hiroute-apibianma"),
        (" Code__Review / Notes ", "hiroute-code-review-notes"),
        ("😀✨", "hiroute-plan-1"),
    ] {
        assert_eq!(registry.suggest_alias(display).unwrap().as_str(), expected);
    }
    // 固定库默认的多音字读音；不会按“重庆”的词义另做消歧。
    use pinyin::ToPinyin;
    let expected = format!(
        "hiroute-{}{}",
        '重'.to_pinyin().unwrap().plain(),
        '庆'.to_pinyin().unwrap().plain()
    );
    assert_eq!(registry.suggest_alias("重庆").unwrap().as_str(), expected);
    assert_eq!(
        registry.suggest_alias("重庆").unwrap(),
        registry.suggest_alias("重庆").unwrap()
    );
}
