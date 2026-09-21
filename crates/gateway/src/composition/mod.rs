//! Production composition contracts. Fixtures never enter this module.

mod ports;

pub use ports::{
    ConversationContentSink, CredentialLease, CredentialResolver, ExecutionFactSink,
    FileCredentialResolver, FilePlannerInputAuthority, LifecycleTelemetrySink,
    PlannerInputAuthority, PortError, ProductionPorts, PublicationPlannerInputAuthority,
    RuntimePublicationFeed, RuntimeStateStore,
};
