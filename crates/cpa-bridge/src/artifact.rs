use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use semver::Version;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use thiserror::Error;

const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PROBE_OUTPUT_BYTES: u64 = 128 * 1024;
const MAX_PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const VERSION_PREFIX: &str = "CLIProxyAPI Version:";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCpaBinary {
    path: PathBuf,
    version: Version,
    sha256_hex: String,
}

impl VerifiedCpaBinary {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn sha256_hex(&self) -> &str {
        &self.sha256_hex
    }

    #[cfg(test)]
    pub(crate) fn fixture(path: PathBuf, version: Version, sha256_hex: String) -> Self {
        Self {
            path,
            version,
            sha256_hex,
        }
    }
}

pub trait CpaBinaryLocator: Send + Sync {
    fn locate(&self) -> Result<VerifiedCpaBinary, CpaArtifactError>;
}

#[derive(Clone, Debug)]
pub struct PinnedCpaArtifact {
    pub trusted_root: PathBuf,
    pub relative_binary: PathBuf,
    pub expected_version: Version,
    pub expected_sha256_hex: String,
    pub probe_timeout: Duration,
}

impl PinnedCpaArtifact {
    pub fn new(
        trusted_root: impl Into<PathBuf>,
        relative_binary: impl Into<PathBuf>,
        expected_version: Version,
        expected_sha256_hex: impl Into<String>,
    ) -> Self {
        Self {
            trusted_root: trusted_root.into(),
            relative_binary: relative_binary.into(),
            expected_version,
            expected_sha256_hex: expected_sha256_hex.into(),
            probe_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PinnedCpaBinaryLocator {
    artifact: PinnedCpaArtifact,
}

impl PinnedCpaBinaryLocator {
    pub fn new(artifact: PinnedCpaArtifact) -> Self {
        Self { artifact }
    }
}

impl CpaBinaryLocator for PinnedCpaBinaryLocator {
    fn locate(&self) -> Result<VerifiedCpaBinary, CpaArtifactError> {
        if self.artifact.probe_timeout.is_zero() || self.artifact.probe_timeout > MAX_PROBE_TIMEOUT
        {
            return Err(CpaArtifactError::InvalidProbeTimeout);
        }
        validate_relative_path(&self.artifact.relative_binary)?;
        let root = self
            .artifact
            .trusted_root
            .canonicalize()
            .map_err(CpaArtifactError::TrustedRoot)?;
        let candidate = root
            .join(&self.artifact.relative_binary)
            .canonicalize()
            .map_err(CpaArtifactError::Binary)?;
        if !candidate.starts_with(&root) {
            return Err(CpaArtifactError::OutsideTrustedRoot);
        }
        let metadata = fs::metadata(&candidate).map_err(CpaArtifactError::Binary)?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(CpaArtifactError::InvalidBinaryFile);
        }
        validate_executable(&metadata)?;

        let actual_digest = sha256_file(&candidate)?;
        let expected_digest = normalize_digest(&self.artifact.expected_sha256_hex)?;
        if actual_digest != expected_digest {
            return Err(CpaArtifactError::DigestMismatch);
        }

        let version = probe_version(&candidate, self.artifact.probe_timeout)?;
        if version != self.artifact.expected_version {
            return Err(CpaArtifactError::VersionMismatch {
                expected: self.artifact.expected_version.clone(),
                actual: version,
            });
        }
        Ok(VerifiedCpaBinary {
            path: candidate,
            version,
            sha256_hex: actual_digest,
        })
    }
}

fn validate_relative_path(path: &Path) -> Result<(), CpaArtifactError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CpaArtifactError::InvalidRelativePath);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_executable(metadata: &fs::Metadata) -> Result<(), CpaArtifactError> {
    use std::os::unix::fs::PermissionsExt as _;

    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(CpaArtifactError::NotExecutable);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_executable(_metadata: &fs::Metadata) -> Result<(), CpaArtifactError> {
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, CpaArtifactError> {
    let mut file = File::open(path).map_err(CpaArtifactError::Binary)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(CpaArtifactError::Binary)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or(CpaArtifactError::InvalidBinaryFile)?;
        if total > MAX_ARTIFACT_BYTES {
            return Err(CpaArtifactError::InvalidBinaryFile);
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn normalize_digest(value: &str) -> Result<String, CpaArtifactError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CpaArtifactError::InvalidDigest);
    }
    Ok(normalized)
}

fn probe_version(path: &Path, timeout: Duration) -> Result<Version, CpaArtifactError> {
    let probe_dir = tempdir().map_err(CpaArtifactError::ProbeIo)?;
    let mut output = tempfile::tempfile().map_err(CpaArtifactError::ProbeIo)?;
    let error_output = output.try_clone().map_err(CpaArtifactError::ProbeIo)?;
    let mut child = Command::new(path)
        .arg("--help")
        .env_clear()
        .current_dir(probe_dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            output.try_clone().map_err(CpaArtifactError::ProbeIo)?,
        ))
        .stderr(Stdio::from(error_output))
        .spawn()
        .map_err(CpaArtifactError::ProbeIo)?;
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .map_err(CpaArtifactError::ProbeIo)?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CpaArtifactError::ProbeTimeout);
        }
        thread::sleep(Duration::from_millis(10));
    }
    output
        .seek(SeekFrom::Start(0))
        .map_err(CpaArtifactError::ProbeIo)?;
    let mut bytes = Vec::new();
    output
        .take(MAX_PROBE_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(CpaArtifactError::ProbeIo)?;
    if bytes.len() as u64 > MAX_PROBE_OUTPUT_BYTES {
        return Err(CpaArtifactError::ProbeOutputTooLarge);
    }
    let text = String::from_utf8(bytes).map_err(|_| CpaArtifactError::MalformedVersionOutput)?;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(VERSION_PREFIX) else {
            continue;
        };
        let value = rest
            .trim()
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .strip_prefix('v')
            .unwrap_or_else(|| rest.trim().split(',').next().unwrap_or_default().trim());
        return Version::parse(value).map_err(|_| CpaArtifactError::MalformedVersionOutput);
    }
    Err(CpaArtifactError::MalformedVersionOutput)
}

#[derive(Debug, Error)]
pub enum CpaArtifactError {
    #[error("CPA trusted artifact path is not a bounded relative path")]
    InvalidRelativePath,
    #[error("CPA trusted artifact root is unavailable: {0}")]
    TrustedRoot(std::io::Error),
    #[error("CPA binary is unavailable: {0}")]
    Binary(std::io::Error),
    #[error("CPA binary resolves outside its trusted artifact root")]
    OutsideTrustedRoot,
    #[error("CPA binary is not a bounded regular file")]
    InvalidBinaryFile,
    #[error("CPA binary is not executable")]
    NotExecutable,
    #[error("CPA artifact digest is malformed")]
    InvalidDigest,
    #[error("CPA artifact digest does not match the trusted manifest")]
    DigestMismatch,
    #[error("CPA version probe failed: {0}")]
    ProbeIo(std::io::Error),
    #[error("CPA version probe deadline is invalid")]
    InvalidProbeTimeout,
    #[error("CPA version probe exceeded its deadline")]
    ProbeTimeout,
    #[error("CPA version probe output exceeded its bound")]
    ProbeOutputTooLarge,
    #[error("CPA version probe output is malformed")]
    MalformedVersionOutput,
    #[error("CPA version {actual} does not match required version {expected}")]
    VersionMismatch { expected: Version, actual: Version },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_parser_requires_exact_sha256_hex() {
        assert!(normalize_digest(&"a".repeat(64)).is_ok());
        assert!(normalize_digest(&"a".repeat(63)).is_err());
        assert!(normalize_digest(&"z".repeat(64)).is_err());
    }

    #[test]
    fn artifact_path_cannot_escape_trusted_root() {
        assert!(validate_relative_path(Path::new("bin/cliproxyapi")).is_ok());
        assert!(validate_relative_path(Path::new("../cliproxyapi")).is_err());
        assert!(validate_relative_path(Path::new("/tmp/cliproxyapi")).is_err());
    }

    #[test]
    fn version_probe_deadline_is_bounded() {
        let locator = PinnedCpaBinaryLocator::new(PinnedCpaArtifact {
            trusted_root: PathBuf::from("/fixture"),
            relative_binary: PathBuf::from("cliproxyapi"),
            expected_version: Version::new(7, 2, 140),
            expected_sha256_hex: "a".repeat(64),
            probe_timeout: Duration::ZERO,
        });
        assert!(matches!(
            locator.locate(),
            Err(CpaArtifactError::InvalidProbeTimeout)
        ));
    }
}
