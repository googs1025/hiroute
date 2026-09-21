pub(crate) use std::net::{IpAddr, Ipv4Addr, SocketAddr};
pub(crate) use std::sync::Arc;
pub(crate) use std::time::{Duration, Instant};

pub(crate) use hiroute_gateway_core::core::execution_plan::{
    AtomicityGroupId, CompiledIngressPlan, ConfigBindingPolicy, ConfigBundle, ConfigCellDescriptor,
    ConfigCellGroup, ConfigCellHandle, ConfigCellId, ConfigGeneration, ConfigRevision,
    ImmutableConfig, PlanError, PlanRevision, ResolvedTargetBindingId, TransportReuseClassId,
};
pub(crate) use hiroute_gateway_core::core::filter::{
    CompiledFilterDescriptor, FilterCapabilities, FilterConfigDependency,
};
pub(crate) use hiroute_gateway_core::core::publication::{
    InstallError, PrepareOutcome, PublicationInstaller,
};
pub(crate) use hiroute_gateway_core::runtime::body::BodyPlan;
pub(crate) use hiroute_gateway_core::test_support::{
    BootstrapBodyPlans, BootstrapListenerConfig, BootstrapPublicationBuilder, NetworkUseCounter,
    match_bootstrap_request, plain_target, tls_target,
};
pub(crate) use http::Uri;
pub(crate) use static_assertions::assert_not_impl_any;
pub(crate) use tokio_util::sync::CancellationToken;

assert_not_impl_any!(hiroute_gateway_core::core::execution_plan::RequestExecutionBinding: Clone);
assert_not_impl_any!(hiroute_gateway_core::core::execution_plan::ConfigLease: Clone);

pub(crate) fn address(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

pub(crate) fn envelope(
    plan: u64,
    revision: u64,
) -> hiroute_gateway_core::core::publication::CompiledGatewayPublicationEnvelope {
    BootstrapPublicationBuilder::new(plan, revision)
        .route(
            "EXAMPLE.com:8080",
            "/v1",
            7,
            plain_target(address(8081), 11),
        )
        .unwrap()
        .build()
        .unwrap()
}

pub(crate) fn install(
    installer: &PublicationInstaller,
    envelope: hiroute_gateway_core::core::publication::CompiledGatewayPublicationEnvelope,
) {
    let cancel = CancellationToken::new();
    let prepared = match installer
        .prepare(envelope, &cancel, Instant::now() + Duration::from_secs(1))
        .unwrap()
    {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => panic!("unexpected duplicate"),
    };
    installer
        .publish(prepared, &cancel, Instant::now() + Duration::from_secs(1))
        .unwrap();
}

pub(crate) fn prepare_error(
    installer: &PublicationInstaller,
    envelope: hiroute_gateway_core::core::publication::CompiledGatewayPublicationEnvelope,
) -> InstallError {
    installer
        .prepare(
            envelope,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err()
}
