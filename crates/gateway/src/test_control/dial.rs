use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Component, Path};
use std::sync::Arc;

use hiroute_gateway_core::core::execution_plan::{CaPolicy, TransportScheme, TransportTarget};
use serde::Deserialize;

pub(crate) const DIAL_CONFIG_ENV: &str = "HIROUTE_E2E_DIAL_CONFIG";
const DIAL_CONFIG_SCHEMA: &str = "hiroute.gateway.e2e-dial-map/v1";
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_CA_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct E2eDialMap {
    targets: BTreeMap<Arc<str>, DialTarget>,
    dns_failures: BTreeSet<Arc<str>>,
}

#[derive(Clone)]
struct DialTarget {
    address: SocketAddr,
    ca_pem: Arc<[u8]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DialConfig {
    schema_version: String,
    targets: Vec<DialConfigTarget>,
    #[serde(default)]
    dns_failures: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DialConfigTarget {
    authority: String,
    address: SocketAddr,
    ca_pem: String,
}

impl E2eDialMap {
    pub(crate) fn from_environment() -> Result<Option<Self>, ()> {
        let Some(path) = std::env::var_os(DIAL_CONFIG_ENV) else {
            return Ok(None);
        };
        let path = Path::new(&path);
        if !safe_absolute_path(path) {
            return Err(());
        }
        let metadata = std::fs::metadata(path).map_err(|_| ())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CONFIG_BYTES {
            return Err(());
        }
        let bytes = std::fs::read(path).map_err(|_| ())?;
        Self::decode(&bytes).map(Some)
    }

    fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let config: DialConfig = serde_json::from_slice(bytes).map_err(|_| ())?;
        if config.schema_version != DIAL_CONFIG_SCHEMA || config.targets.is_empty() {
            return Err(());
        }
        let mut targets = BTreeMap::new();
        for target in config.targets {
            if !valid_authority(&target.authority)
                || !target.address.ip().is_loopback()
                || target.address.port() == 0
                || target.ca_pem.is_empty()
                || target.ca_pem.len() > MAX_CA_BYTES
                || !target.ca_pem.starts_with("-----BEGIN CERTIFICATE-----\n")
                || !target.ca_pem.ends_with("-----END CERTIFICATE-----\n")
                || targets
                    .insert(
                        Arc::from(target.authority),
                        DialTarget {
                            address: target.address,
                            ca_pem: Arc::from(target.ca_pem.into_bytes()),
                        },
                    )
                    .is_some()
            {
                return Err(());
            }
        }
        let mut dns_failures = BTreeSet::new();
        for authority in config.dns_failures {
            if !valid_authority(&authority)
                || targets.contains_key(authority.as_str())
                || !dns_failures.insert(Arc::from(authority))
            {
                return Err(());
            }
        }
        Ok(Self {
            targets,
            dns_failures,
        })
    }

    pub(crate) fn fails_dns(&self, target: &TransportTarget) -> bool {
        target
            .unresolved_authority()
            .is_some_and(|authority| self.dns_failures.contains(authority))
    }

    pub(crate) fn apply(&self, mut target: TransportTarget) -> Result<Option<TransportTarget>, ()> {
        let Some(authority) = target.unresolved_authority() else {
            return Ok(None);
        };
        let Some(mapped) = self.targets.get(authority) else {
            return Ok(None);
        };
        let uri = format!("https://{authority}/")
            .parse::<http::Uri>()
            .map_err(|_| ())?;
        let host = uri.authority().ok_or(())?.host();
        if target.scheme != TransportScheme::Https || target.sni.as_deref() != Some(host) {
            return Err(());
        }
        target = target
            .with_resolved_addresses(Arc::from([mapped.address]))
            .map_err(|_| ())?;
        target.ca = CaPolicy::Pem(Arc::clone(&mapped.ca_pem));
        target = target.with_derived_connection_fingerprint();
        target.validate().map_err(|_| ())?;
        Ok(Some(target))
    }
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn valid_authority(authority: &str) -> bool {
    let Ok(uri) = format!("https://{authority}/").parse::<http::Uri>() else {
        return false;
    };
    uri.authority().is_some_and(|parsed| {
        parsed.as_str() == authority
            && !authority.contains('@')
            && !parsed.host().is_empty()
            && parsed.host().parse::<std::net::IpAddr>().is_err()
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hiroute_gateway_core::core::execution_plan::{PoolEpoch, TransportReuseClassId};

    use super::*;

    #[test]
    fn exact_https_authority_maps_to_loopback_without_rewriting_identity() {
        let map = E2eDialMap::decode(
            br#"{"schema_version":"hiroute.gateway.e2e-dial-map/v1","targets":[{"authority":"provider.invalid","address":"127.0.0.1:4317","ca_pem":"-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----\n"}]}"#,
        )
        .unwrap();
        let target = TransportTarget {
            scheme: TransportScheme::Https,
            authority: TransportTarget::mark_resolution_required("provider.invalid"),
            addresses: Arc::from([]),
            sni: Some(Arc::from("provider.invalid")),
            ca: CaPolicy::System,
            alpn: Arc::from([Arc::from("http/1.1")]),
            connect_timeout: Duration::from_secs(1),
            transport_read_buffer_bytes: 1024,
            h2_stream_window_bytes: 1024,
            h2_connection_window_bytes: 1024,
            h2_max_concurrent_streams: 1,
            reuse_class: TransportReuseClassId(1),
            pool_epoch: PoolEpoch(1),
            connection_fingerprint: [0; 32],
        }
        .with_derived_connection_fingerprint();

        let mapped = map.apply(target).unwrap().unwrap();
        assert_eq!(mapped.authority.as_ref(), "provider.invalid");
        assert_eq!(mapped.sni.as_deref(), Some("provider.invalid"));
        assert_eq!(
            mapped.addresses.as_ref(),
            &["127.0.0.1:4317".parse().unwrap()]
        );
        assert!(matches!(mapped.ca, CaPolicy::Pem(_)));
    }
}
