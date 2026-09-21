use hiroute_domain::WorkspaceId;
use hiroute_integrations::TrustedReleaseCatalog;
use hiroute_local_storage::LocalStorageSet;

pub(super) fn activate_release_catalog_revisions(
    stores: &LocalStorageSet,
    catalog: &TrustedReleaseCatalog,
) -> Result<(), String> {
    let provenance = catalog
        .compute_catalog_provenance()
        .map_err(|error| format!("ReleaseFacts provenance is invalid: {error}"))?;
    stores
        .control()
        .activate_embedded_release_catalog(&WorkspaceId::default(), provenance.release_sequence)
        .map_err(|error| format!("ReleaseFacts revision activation failed: {error}"))
}
