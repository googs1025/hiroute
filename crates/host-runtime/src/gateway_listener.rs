use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const GATEWAY_LISTENER_SCHEMA_V1: &str = "hiroute.gateway-listener/v1";
const MAX_RECORD_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPortModeV1 {
    Automatic,
    Fixed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerDesiredV1 {
    pub address: String,
    pub port_mode: GatewayPortModeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl GatewayListenerDesiredV1 {
    pub fn automatic(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            port_mode: GatewayPortModeV1::Automatic,
            port: None,
        }
    }

    pub fn fixed(address: impl Into<String>, port: u16) -> Self {
        Self {
            address: address.into(),
            port_mode: GatewayPortModeV1::Fixed,
            port: Some(port),
        }
    }

    pub fn validate(&self) -> Result<Ipv4Addr, GatewayListenerError> {
        let address = self
            .address
            .parse::<Ipv4Addr>()
            .map_err(|_| GatewayListenerError::InvalidConfiguration)?;
        match (self.port_mode, self.port) {
            (GatewayPortModeV1::Automatic, _) => {}
            (GatewayPortModeV1::Fixed, Some(port)) if port != 0 => {}
            _ => return Err(GatewayListenerError::InvalidConfiguration),
        }
        Ok(address)
    }

    pub fn selected_address(&self) -> Result<Option<SocketAddr>, GatewayListenerError> {
        let address = self.validate()?;
        Ok(self
            .port
            .filter(|port| *port != 0)
            .map(|port| SocketAddr::V4(SocketAddrV4::new(address, port))))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerAppliedV1 {
    pub address: String,
    pub port: u16,
    pub applied_at_unix: u64,
}

impl GatewayListenerAppliedV1 {
    pub fn socket_address(&self) -> Result<SocketAddr, GatewayListenerError> {
        parse_selected(&self.address, self.port)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayListenerOperationStateV1 {
    Submitted,
    Restarting,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerOperationV1 {
    pub operation_id: String,
    pub state: GatewayListenerOperationStateV1,
    pub desired: GatewayListenerDesiredV1,
    pub submitted_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl GatewayListenerOperationV1 {
    fn validate(&self) -> Result<(), GatewayListenerError> {
        self.desired.validate()?;
        if self.operation_id.len() != 32
            || !self
                .operation_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.submitted_at_unix == 0
            || self.error_code.as_ref().is_some_and(|code| {
                code.is_empty()
                    || code.len() > 128
                    || !code.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        {
            return Err(GatewayListenerError::InvalidConfiguration);
        }
        match self.state {
            GatewayListenerOperationStateV1::Submitted
            | GatewayListenerOperationStateV1::Restarting => {
                if self.completed_at_unix.is_some() || self.error_code.is_some() {
                    return Err(GatewayListenerError::InvalidConfiguration);
                }
            }
            GatewayListenerOperationStateV1::Succeeded => {
                if self.completed_at_unix.is_none() || self.error_code.is_some() {
                    return Err(GatewayListenerError::InvalidConfiguration);
                }
            }
            GatewayListenerOperationStateV1::Failed => {
                if self.completed_at_unix.is_none() || self.error_code.is_none() {
                    return Err(GatewayListenerError::InvalidConfiguration);
                }
            }
        }
        Ok(())
    }

    pub fn active(&self) -> bool {
        matches!(
            self.state,
            GatewayListenerOperationStateV1::Submitted
                | GatewayListenerOperationStateV1::Restarting
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerConfigV1 {
    pub schema_version: String,
    pub desired: GatewayListenerDesiredV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<GatewayListenerAppliedV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<GatewayListenerOperationV1>,
}

impl Default for GatewayListenerConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: GATEWAY_LISTENER_SCHEMA_V1.to_owned(),
            desired: GatewayListenerDesiredV1::automatic(Ipv4Addr::LOCALHOST.to_string()),
            applied: None,
            operation: None,
        }
    }
}

impl GatewayListenerConfigV1 {
    pub fn validate(&self) -> Result<(), GatewayListenerError> {
        if self.schema_version != GATEWAY_LISTENER_SCHEMA_V1 {
            return Err(GatewayListenerError::InvalidConfiguration);
        }
        self.desired.validate()?;
        if let Some(applied) = &self.applied {
            applied.socket_address()?;
            if applied.applied_at_unix == 0 {
                return Err(GatewayListenerError::InvalidConfiguration);
            }
        }
        if let Some(operation) = &self.operation {
            operation.validate()?;
            if operation.desired != self.desired {
                return Err(GatewayListenerError::InvalidConfiguration);
            }
            if operation.state == GatewayListenerOperationStateV1::Succeeded
                && self
                    .applied
                    .as_ref()
                    .map(|value| (&value.address, value.port))
                    != self.desired.port.map(|port| (&self.desired.address, port))
            {
                return Err(GatewayListenerError::InvalidConfiguration);
            }
        }
        Ok(())
    }

    pub fn desired_address(&self) -> Result<Option<SocketAddr>, GatewayListenerError> {
        self.desired.selected_address()
    }

    pub fn applied_address(&self) -> Result<Option<SocketAddr>, GatewayListenerError> {
        self.applied
            .as_ref()
            .map(GatewayListenerAppliedV1::socket_address)
            .transpose()
    }
}

#[derive(Debug, Error)]
pub enum GatewayListenerError {
    #[error("GATEWAY_LISTENER_CONFIG_INVALID")]
    InvalidConfiguration,
    #[error("GATEWAY_LISTENER_CONFIG_UNAVAILABLE")]
    Unavailable,
    #[error("GATEWAY_LISTENER_ADDRESS_UNAVAILABLE")]
    AddressUnavailable,
}

#[derive(Debug)]
pub struct GatewayListenerReservation {
    pub listen: SocketAddr,
    pub config: GatewayListenerConfigV1,
    listener: TcpListener,
}

impl GatewayListenerReservation {
    pub fn release(self) -> SocketAddr {
        let listen = self.listen;
        drop(self.listener);
        listen
    }
}

#[derive(Clone, Debug)]
pub struct GatewayListenerStore {
    root: PathBuf,
}

impl GatewayListenerStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.root.join("gateway-listener.json")
    }

    pub fn load(&self) -> Result<Option<GatewayListenerConfigV1>, GatewayListenerError> {
        let mut file = match open_no_follow(&self.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(GatewayListenerError::InvalidConfiguration),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| GatewayListenerError::InvalidConfiguration)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(GatewayListenerError::InvalidConfiguration);
        }
        let config: GatewayListenerConfigV1 = serde_json::from_slice(&bytes)
            .map_err(|_| GatewayListenerError::InvalidConfiguration)?;
        config.validate()?;
        Ok(Some(config))
    }

    pub fn load_or_default(&self) -> Result<GatewayListenerConfigV1, GatewayListenerError> {
        Ok(self.load()?.unwrap_or_default())
    }

    pub fn save(&self, config: &GatewayListenerConfigV1) -> Result<(), GatewayListenerError> {
        config.validate()?;
        let metadata =
            std::fs::symlink_metadata(&self.root).map_err(|_| GatewayListenerError::Unavailable)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(GatewayListenerError::Unavailable);
        }
        let bytes = serde_json::to_vec_pretty(config)
            .map_err(|_| GatewayListenerError::InvalidConfiguration)?;
        let temporary = self.root.join(format!(
            ".gateway-listener-{}.tmp",
            random_hex().map_err(|_| GatewayListenerError::Unavailable)?
        ));
        let result = write_atomic(&temporary, &self.path(), &bytes);
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub fn configure(
        &self,
        desired: GatewayListenerDesiredV1,
    ) -> Result<GatewayListenerConfigV1, GatewayListenerError> {
        desired.validate()?;
        let mut config = self.load_or_default()?;
        config.desired = desired.clone();
        config.operation = Some(GatewayListenerOperationV1 {
            operation_id: random_hex().map_err(|_| GatewayListenerError::Unavailable)?,
            state: GatewayListenerOperationStateV1::Submitted,
            desired,
            submitted_at_unix: now_unix()?,
            completed_at_unix: None,
            error_code: None,
        });
        self.save(&config)?;
        Ok(config)
    }

    pub fn recover(&self) -> Result<GatewayListenerConfigV1, GatewayListenerError> {
        let config = self.load_or_default().unwrap_or_default();
        let desired = match config.applied_address()? {
            Some(SocketAddr::V4(address)) => {
                GatewayListenerDesiredV1::fixed(address.ip().to_string(), address.port())
            }
            _ => GatewayListenerConfigV1::default().desired,
        };
        desired.validate()?;
        let mut recovered = config;
        recovered.schema_version = GATEWAY_LISTENER_SCHEMA_V1.to_owned();
        recovered.desired = desired.clone();
        recovered.operation = Some(GatewayListenerOperationV1 {
            operation_id: random_hex().map_err(|_| GatewayListenerError::Unavailable)?,
            state: GatewayListenerOperationStateV1::Submitted,
            desired,
            submitted_at_unix: now_unix()?,
            completed_at_unix: None,
            error_code: None,
        });
        self.save(&recovered)?;
        Ok(recovered)
    }

    pub fn reserve(&self) -> Result<GatewayListenerReservation, GatewayListenerError> {
        let mut config = self.load_or_default()?;
        let address = config.desired.validate()?;
        let requested_port = match config.desired.port_mode {
            GatewayPortModeV1::Automatic => config.desired.port.unwrap_or(0),
            GatewayPortModeV1::Fixed => config
                .desired
                .port
                .filter(|port| *port != 0)
                .ok_or(GatewayListenerError::InvalidConfiguration)?,
        };
        let listener = TcpListener::bind(SocketAddrV4::new(address, requested_port))
            .map_err(|_| GatewayListenerError::AddressUnavailable)?;
        let listen = listener
            .local_addr()
            .map_err(|_| GatewayListenerError::AddressUnavailable)?;
        if !listen.is_ipv4() || listen.port() == 0 {
            return Err(GatewayListenerError::AddressUnavailable);
        }
        let mut changed = false;
        if config.desired.port != Some(listen.port()) {
            config.desired.port = Some(listen.port());
            if let Some(operation) = &mut config.operation {
                operation.desired.port = Some(listen.port());
            }
            changed = true;
        }
        if let Some(operation) = &mut config.operation
            && operation.state == GatewayListenerOperationStateV1::Submitted
        {
            operation.state = GatewayListenerOperationStateV1::Restarting;
            changed = true;
        }
        if changed || self.load()?.is_none() {
            self.save(&config)?;
        }
        Ok(GatewayListenerReservation {
            listen,
            config,
            listener,
        })
    }

    pub fn mark_applied(
        &self,
        listen: SocketAddr,
    ) -> Result<GatewayListenerConfigV1, GatewayListenerError> {
        if !listen.is_ipv4() || listen.port() == 0 {
            return Err(GatewayListenerError::InvalidConfiguration);
        }
        let mut config = self.load_or_default()?;
        if config.desired_address()? != Some(listen) {
            return Err(GatewayListenerError::InvalidConfiguration);
        }
        let completed = now_unix()?;
        config.applied = Some(GatewayListenerAppliedV1 {
            address: listen.ip().to_string(),
            port: listen.port(),
            applied_at_unix: completed,
        });
        if let Some(operation) = &mut config.operation
            && operation.active()
        {
            operation.state = GatewayListenerOperationStateV1::Succeeded;
            operation.completed_at_unix = Some(completed);
            operation.error_code = None;
        }
        self.save(&config)?;
        Ok(config)
    }

    pub fn mark_failed(
        &self,
        error_code: &str,
    ) -> Result<GatewayListenerConfigV1, GatewayListenerError> {
        if error_code.is_empty()
            || error_code.len() > 128
            || !error_code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(GatewayListenerError::InvalidConfiguration);
        }
        let mut config = self.load_or_default()?;
        if let Some(operation) = &mut config.operation
            && operation.active()
        {
            operation.state = GatewayListenerOperationStateV1::Failed;
            operation.completed_at_unix = Some(now_unix()?);
            operation.error_code = Some(error_code.to_owned());
            self.save(&config)?;
        }
        Ok(config)
    }
}

pub fn connect_address(listen: SocketAddr) -> Result<SocketAddr, GatewayListenerError> {
    match listen {
        SocketAddr::V4(address) if address.port() != 0 && address.ip().is_unspecified() => Ok(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, address.port())),
        ),
        SocketAddr::V4(address) if address.port() != 0 => Ok(SocketAddr::V4(address)),
        SocketAddr::V4(_) | SocketAddr::V6(_) => Err(GatewayListenerError::InvalidConfiguration),
    }
}

pub fn validate_gateway_listen(listen: SocketAddr) -> Result<(), GatewayListenerError> {
    if matches!(listen.ip(), IpAddr::V4(_)) && listen.port() != 0 {
        Ok(())
    } else {
        Err(GatewayListenerError::InvalidConfiguration)
    }
}

fn parse_selected(address: &str, port: u16) -> Result<SocketAddr, GatewayListenerError> {
    let address = address
        .parse::<Ipv4Addr>()
        .map_err(|_| GatewayListenerError::InvalidConfiguration)?;
    if port == 0 {
        return Err(GatewayListenerError::InvalidConfiguration);
    }
    Ok(SocketAddr::V4(SocketAddrV4::new(address, port)))
}

fn now_unix() -> Result<u64, GatewayListenerError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().max(1))
        .map_err(|_| GatewayListenerError::Unavailable)
}

fn random_hex() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    let mut output = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("String writes cannot fail");
    }
    Ok(output)
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new().read(true).open(path)
}

fn write_atomic(temporary: &Path, target: &Path, bytes: &[u8]) -> Result<(), GatewayListenerError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(temporary)
        .map_err(|_| GatewayListenerError::Unavailable)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| GatewayListenerError::Unavailable)?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temporary, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| GatewayListenerError::Unavailable)?;
    }
    std::fs::rename(temporary, target).map_err(|_| GatewayListenerError::Unavailable)?;
    if let Some(parent) = target.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| GatewayListenerError::Unavailable)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_selection_is_persisted_once_and_wildcard_connects_through_loopback() {
        let root = tempfile::tempdir().unwrap();
        let store = GatewayListenerStore::new(root.path());
        let first = store.reserve().unwrap();
        assert_eq!(first.listen.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_ne!(first.listen.port(), 0);
        let listen = first.release();
        assert_eq!(
            store.load().unwrap().unwrap().desired.port,
            Some(listen.port())
        );
        assert_eq!(store.reserve().unwrap().listen, listen);

        let wildcard = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5837));
        assert_eq!(
            connect_address(wildcard).unwrap(),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 5837))
        );
    }

    #[test]
    fn desired_applied_and_failure_states_do_not_overstate_activation() {
        let root = tempfile::tempdir().unwrap();
        let store = GatewayListenerStore::new(root.path());
        let desired = GatewayListenerDesiredV1::fixed("127.0.0.1", 45837);
        let configured = store.configure(desired.clone()).unwrap();
        assert!(configured.applied.is_none());
        assert_eq!(
            configured.operation.unwrap().state,
            GatewayListenerOperationStateV1::Submitted
        );
        let failed = store.mark_failed("GATEWAY_START_FAILED").unwrap();
        assert!(failed.applied.is_none());
        assert_eq!(
            failed.operation.unwrap().state,
            GatewayListenerOperationStateV1::Failed
        );

        let configured = store.configure(GatewayListenerDesiredV1::automatic("127.0.0.1"));
        assert!(configured.is_ok());
        let reservation = store.reserve().unwrap();
        let listen = reservation.release();
        let applied = store.mark_applied(listen).unwrap();
        assert_eq!(applied.applied_address().unwrap(), Some(listen));
        assert_eq!(
            applied.operation.unwrap().state,
            GatewayListenerOperationStateV1::Succeeded
        );
        assert_eq!(
            store.recover().unwrap().desired_address().unwrap(),
            Some(listen)
        );
    }

    #[test]
    fn invalid_ipv6_ports_unknown_fields_and_symlinks_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let store = GatewayListenerStore::new(root.path());
        for bytes in [
            br#"{}"#.as_slice(),
            br#"{"schema_version":"hiroute.gateway-listener/v1","desired":{"address":"::1","port_mode":"fixed","port":5837}}"#,
            br#"{"schema_version":"hiroute.gateway-listener/v1","desired":{"address":"127.0.0.1","port_mode":"fixed","port":0}}"#,
            br#"{"schema_version":"hiroute.gateway-listener/v1","desired":{"address":"127.0.0.1","port_mode":"automatic"},"extra":true}"#,
        ] {
            std::fs::write(store.path(), bytes).unwrap();
            assert!(store.load().is_err(), "{bytes:?}");
        }
        #[cfg(unix)]
        {
            let target = root.path().join("target");
            std::fs::write(&target, b"{}").unwrap();
            let _ = std::fs::remove_file(store.path());
            std::os::unix::fs::symlink(target, store.path()).unwrap();
            assert!(store.load().is_err());
        }
        assert!(connect_address("[::1]:5837".parse().unwrap()).is_err());
    }
}
