//! Frozen initialization shape used by the sealed Oracle; current inputs adapt separately.
use super::runtime::PUBLICATION_REVISION;
use super::types::*;
use crate::p0::canonical::sha256_hex;
use crate::p0::privacy::{create_private_dir, private_write};
use serde::Serialize;
use serde_json::json;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

const LEGACY_PUBLICATION_SCHEMA: &str = "hiroute.gateway.publication-snapshot/v2";

#[derive(Serialize)]
struct PublicationSnapshot {
    schema_version: String,
    authority_id: String,
    authority_epoch: u64,
    publication_revision: u64,
    payload_digest: String,
    catalog_renderer_revision: String,
    aliases: Vec<AliasPlan>,
    grants: Vec<Grant>,
}

#[derive(Serialize)]
struct AliasPlan {
    served_model_id: String,
    purpose: String,
    agent_plan_revision: u64,
    protocols: Vec<String>,
    overall_timeout_ms: u64,
    max_attempts: u32,
    candidates: Vec<Candidate>,
}

#[derive(Serialize)]
struct Candidate {
    local_id: u32,
    stable_target_key: String,
    adapter_id: String,
    credential_ref: String,
    endpoint: String,
}

#[derive(Serialize)]
struct Grant {
    grant_id: String,
    generation: u64,
    bearer_token_sha256: String,
    allowed_protocols: Vec<String>,
    allowed_aliases: Vec<String>,
}

pub(super) struct RuntimeFiles {
    pub(super) publication_path: PathBuf,
    pub(super) credential_manifest_path: PathBuf,
    pub(super) lkg_path: PathBuf,
    pub(super) observation_dir: PathBuf,
    pub(super) publication_digest: String,
    pub(super) credential_manifest_digest: String,
    pub(super) credential_lease_digest: String,
}

pub(super) fn prepare(
    root: &Path,
    provider_addr: SocketAddr,
    client_token: &str,
    provider_token: &str,
) -> Result<RuntimeFiles, ProductionError> {
    let inputs = root.join("inputs");
    let observation_dir = root.join("observation");
    create_private_dir(&inputs)?;
    create_private_dir(&observation_dir)?;
    let mut publication = PublicationSnapshot {
        schema_version: LEGACY_PUBLICATION_SCHEMA.into(),
        authority_id: "oracle-authority".into(),
        authority_epoch: 1,
        publication_revision: PUBLICATION_REVISION,
        payload_digest: String::new(),
        catalog_renderer_revision: "process-22012-production-oracle/v1".into(),
        aliases: vec![AliasPlan {
            served_model_id: "oracle-smoke".into(),
            purpose: "production listener release smoke".into(),
            agent_plan_revision: PUBLICATION_REVISION,
            protocols: vec!["responses".into()],
            overall_timeout_ms: 5_000,
            max_attempts: 1,
            candidates: vec![Candidate {
                local_id: 1,
                stable_target_key: "oracle-native-provider".into(),
                adapter_id: "builtin-openai".into(),
                credential_ref: "oracle-credential".into(),
                endpoint: format!("http://127.0.0.1:{}/v1", provider_addr.port()),
            }],
        }],
        grants: vec![Grant {
            grant_id: "oracle-grant".into(),
            generation: 1,
            bearer_token_sha256: sha256_hex(client_token.as_bytes()),
            allowed_protocols: vec!["responses".into()],
            allowed_aliases: vec!["oracle-smoke".into()],
        }],
    };
    publication.payload_digest = sha256_hex(&serde_json::to_vec(&publication)?);
    let publication_path = inputs.join("publication.json");
    let publication_bytes = serde_json::to_vec_pretty(&publication)?;
    private_write(&publication_path, &publication_bytes)?;
    let lease = json!({
        "schema_version": CREDENTIAL_LEASE_SCHEMA,
        "credential_ref": "oracle-credential",
        "keys": [{
            "key_id": "oracle-key",
            "generation": 1,
            "authorization": format!("Bearer {provider_token}"),
        }]
    });
    let credential_lease_path = inputs.join("oracle-credential.json");
    let credential_lease_bytes = serde_json::to_vec_pretty(&lease)?;
    private_write(&credential_lease_path, &credential_lease_bytes)?;
    let credential_manifest = json!({
        "schema_version": CREDENTIAL_MANIFEST_SCHEMA,
        "credentials": {"oracle-credential": "oracle-credential.json"}
    });
    let credential_manifest_path = inputs.join("credentials.json");
    let credential_manifest_bytes = serde_json::to_vec_pretty(&credential_manifest)?;
    private_write(&credential_manifest_path, &credential_manifest_bytes)?;
    Ok(RuntimeFiles {
        publication_path,
        credential_manifest_path,
        lkg_path: inputs.join("publication-lkg.json"),
        observation_dir,
        publication_digest: publication.payload_digest,
        credential_manifest_digest: sha256_hex(&credential_manifest_bytes),
        credential_lease_digest: sha256_hex(&credential_lease_bytes),
    })
}
