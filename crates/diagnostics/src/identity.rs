//! Validated identifier and version tokens that may appear in diagnostic records.
//!
//! Every string that reaches a diagnostic record goes through one of these types. None of
//! them can carry arbitrary text: constructors and deserializers only accept fixed-shape
//! hex identifiers, controlled version tokens, relative source paths or a narrow target
//! triple. A record produced from another process is re-validated while it is parsed.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Hex length of a 16-byte random identifier (boot/session/span ids).
pub const RANDOM_ID_HEX_LEN: usize = 32;
/// Hex length of a full HMAC-SHA256 correlation token.
pub const CORRELATION_TOKEN_HEX_LEN: usize = 64;

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

macro_rules! random_hex_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            pub fn random() -> Result<Self, RandomIdError> {
                let mut bytes = [0u8; 16];
                getrandom::fill(&mut bytes).map_err(|_| RandomIdError)?;
                Ok(Self(bytes))
            }

            pub fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            pub fn to_hex(self) -> String {
                encode_hex(&self.0)
            }

            pub fn parse(value: &str) -> Result<Self, InvalidTokenError> {
                if !is_lower_hex(value, RANDOM_ID_HEX_LEN) {
                    return Err(InvalidTokenError);
                }
                let mut bytes = [0u8; 16];
                for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
                    let hi = (chunk[0] as char).to_digit(16).ok_or(InvalidTokenError)?;
                    let lo = (chunk[1] as char).to_digit(16).ok_or(InvalidTokenError)?;
                    bytes[index] = ((hi << 4) | lo) as u8;
                }
                Ok(Self(bytes))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_hex())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_hex())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(D::Error::custom)
            }
        }
    };
}

random_hex_id!(BootId);
random_hex_id!(SessionId);
random_hex_id!(SpanId);

/// Random identifier generation failed; the caller degrades instead of faking an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("diagnostic random id unavailable")]
pub struct RandomIdError;

/// A value that was expected to be a fixed-shape lower-hex identifier was not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid diagnostic identifier token")]
pub struct InvalidTokenError;

/// A full 32-byte HMAC token. Only [`crate::correlation`] constructs real values; the
/// parser re-validates tokens that arrive from another process or from an export file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CorrelationToken([u8; 32]);

impl CorrelationToken {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn to_hex(self) -> String {
        encode_hex(&self.0)
    }

    pub fn parse(value: &str) -> Result<Self, InvalidTokenError> {
        if !is_lower_hex(value, CORRELATION_TOKEN_HEX_LEN) {
            return Err(InvalidTokenError);
        }
        let mut bytes = [0u8; 32];
        for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16).ok_or(InvalidTokenError)?;
            let lo = (chunk[1] as char).to_digit(16).ok_or(InvalidTokenError)?;
            bytes[index] = ((hi << 4) | lo) as u8;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for CorrelationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for CorrelationToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for CorrelationToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

/// A version or source-revision token. Accepts `unknown`, a full 40-hex git revision or a
/// narrow `major.minor.patch[-suffix]` version string; nothing else can be constructed or
/// parsed, so arbitrary text cannot be smuggled through a "version" field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionToken(String);

impl VersionToken {
    pub const UNKNOWN: &'static str = "unknown";

    pub fn unknown() -> Self {
        Self(Self::UNKNOWN.to_string())
    }

    pub fn from_revision(value: &str) -> Result<Self, InvalidTokenError> {
        if value.len() == 40
            && value
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b.is_ascii_digit())
        {
            let lowered = value.to_ascii_lowercase();
            if is_lower_hex(&lowered, 40) {
                return Ok(Self(lowered));
            }
        }
        Err(InvalidTokenError)
    }

    pub fn from_version(value: &str) -> Result<Self, InvalidTokenError> {
        let (core, suffix) = match value.find(['-', '+']) {
            Some(index) => (&value[..index], Some(&value[index..])),
            None => (value, None),
        };
        let mut parts = core.split('.');
        let mut count = 0;
        for part in &mut parts {
            if part.is_empty() || part.len() > 6 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(InvalidTokenError);
            }
            count += 1;
        }
        if count != 3 {
            return Err(InvalidTokenError);
        }
        if let Some(suffix) = suffix
            && (suffix.len() > 33
                || !suffix[1..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+')))
        {
            return Err(InvalidTokenError);
        }
        Ok(Self(value.to_string()))
    }

    pub fn parse(value: &str) -> Result<Self, InvalidTokenError> {
        if value == Self::UNKNOWN {
            return Ok(Self::unknown());
        }
        Self::from_revision(value).or_else(|_| Self::from_version(value))
    }

    /// The compiled package version of the current binary, validated.
    pub fn current_package() -> Self {
        Self::from_version(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Self::unknown())
    }

    /// An optional compile-time injected source revision; `unknown` when not injected.
    pub fn source_revision() -> Self {
        match option_env!("HIROUTE_BUILD_SOURCE_SHA") {
            Some(value) => Self::from_revision(value).unwrap_or_else(|_| Self::unknown()),
            None => Self::unknown(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VersionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for VersionToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for VersionToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

/// A compile-time-registered source location: a relative path inside the workspace that
/// contains no parent traversal, stays within the allowed length and charset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceFileRef(String);

impl SourceFileRef {
    pub fn parse(value: &str) -> Result<Self, InvalidTokenError> {
        if value.is_empty() || value.len() > 256 || value.starts_with('/') {
            return Err(InvalidTokenError);
        }
        if value.contains("..") || value.contains('\\') || value.contains(':') {
            return Err(InvalidTokenError);
        }
        let allowed = value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'));
        if !allowed || !value.ends_with(".rs") {
            return Err(InvalidTokenError);
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceFileRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for SourceFileRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SourceFileRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

/// A narrow OS/architecture target token such as `macos-aarch64` or `linux-x86_64`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetTriple(String);

impl TargetTriple {
    pub fn current() -> Self {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        Self(format!("{os}-{arch}"))
    }

    pub fn parse(value: &str) -> Result<Self, InvalidTokenError> {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            return Err(InvalidTokenError);
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for TargetTriple {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TargetTriple {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_round_trip_through_hex() {
        let id = BootId::random().expect("random id");
        let parsed = BootId::parse(&id.to_hex()).expect("parse");
        assert_eq!(id, parsed);
        assert_eq!(id.to_hex().len(), RANDOM_ID_HEX_LEN);
    }

    #[test]
    fn identifiers_reject_non_hex_and_wrong_length() {
        assert!(BootId::parse("z".repeat(32).as_str()).is_err());
        assert!(BootId::parse("ab").is_err());
        assert!(CorrelationToken::parse(&"a".repeat(64)).is_ok());
        assert!(CorrelationToken::parse(&"A".repeat(64)).is_err());
        assert!(CorrelationToken::parse(&"a".repeat(63)).is_err());
    }

    #[test]
    fn version_tokens_only_accept_controlled_shapes() {
        assert!(VersionToken::parse("unknown").is_ok());
        assert!(VersionToken::parse("0.1.0").is_ok());
        assert!(VersionToken::parse("1.2.3-beta.1").is_ok());
        let revision = "dbfc5569b0114e49bb8ed398af184ffa6f7c2fe9";
        assert_eq!(
            VersionToken::parse(revision).expect("revision").as_str(),
            revision
        );
        assert!(VersionToken::parse("not a version").is_err());
        assert!(VersionToken::parse("1.2").is_err());
        assert!(VersionToken::parse("1.2.3; rm -rf /").is_err());
        assert!(VersionToken::parse(&"f".repeat(41)).is_err());
    }

    #[test]
    fn source_file_refs_reject_traversal_and_absolute_paths() {
        assert!(SourceFileRef::parse("crates/daemon/src/control/bin.rs").is_ok());
        assert!(SourceFileRef::parse("/etc/passwd").is_err());
        assert!(SourceFileRef::parse("crates/../../etc/passwd.rs").is_err());
        assert!(SourceFileRef::parse("crates/daemon/src/control/bin.txt").is_err());
        assert!(SourceFileRef::parse("").is_err());
    }

    #[test]
    fn target_triple_is_narrow() {
        assert!(TargetTriple::parse("macos-aarch64").is_ok());
        assert!(TargetTriple::parse("linux-x86_64").is_ok());
        assert!(TargetTriple::parse("macOS AArch64").is_err());
        assert!(TargetTriple::parse("").is_err());
    }
}
