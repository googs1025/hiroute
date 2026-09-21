use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
};

use super::{Result, SmokeError, digest, process::Process, require};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    pub root: PathBuf,
    pub revision: String,
    pub tree: String,
    pub inputs: String,
}
impl Source {
    pub(super) fn capture(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let git = |args: &[&str]| -> Result<Vec<u8>> {
            let output = Command::new("git").args(args).current_dir(&root).output()?;
            require(output.status.success(), "git_identity_failed")?;
            Ok(output.stdout)
        };
        require(
            git(&["status", "--porcelain", "--untracked-files=all"])?.is_empty(),
            "candidate_not_clean",
        )?;
        let text = |args: &[&str]| -> Result<String> {
            Ok(String::from_utf8(git(args)?)
                .map_err(|_| SmokeError("git_identity_failed"))?
                .trim()
                .to_owned())
        };
        require(
            Path::new(&text(&["rev-parse", "--show-toplevel"])?).canonicalize()? == root,
            "not_repository_root",
        )?;
        let revision = text(&["rev-parse", "HEAD"])?;
        let tree = text(&["rev-parse", "HEAD^{tree}"])?;
        let inputs = digest(&git(&["ls-tree", "-r", "HEAD"])?);
        Ok(Self {
            root,
            revision,
            tree,
            inputs,
        })
    }
    pub(super) fn verify(&self) -> Result<()> {
        require(Self::capture(&self.root)? == *self, "candidate_changed")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub package: String,
    pub binary: String,
    pub sha256: String,
    pub source_revision: String,
    pub build_argv: Vec<String>,
    pub cargo_version: String,
    pub rustc_version: String,
    pub wrapper_sha256: Option<String>,
    pub wrapper_version: Option<String>,
    #[serde(skip)]
    pub(super) path: PathBuf,
}
impl Artifact {
    pub(super) fn verify(&self, source: &Source) -> Result<()> {
        source.verify()?;
        require(
            self.source_revision == source.revision
                && self
                    .path
                    .canonicalize()?
                    .starts_with(source.root.join("target"))
                && digest(&std::fs::read(&self.path)?) == self.sha256,
            "artifact_changed",
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildRecipe {
    source_revision: String,
    source_tree: String,
    source_inputs: String,
    package: String,
    binary: String,
    cargo: String,
    cargo_sha256: String,
    cargo_version: String,
    rustc_sha256: String,
    rustc_version: String,
    wrapper_sha256: Option<String>,
    wrapper_version: Option<String>,
    cargo_config_sha256: String,
    environment: BTreeMap<String, String>,
    platform: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildReceipt {
    recipe: BuildRecipe,
    artifact: Artifact,
    path: PathBuf,
}

fn command_version(path: &Path) -> Result<String> {
    let output = Command::new(path).arg("-vV").output()?;
    require(output.status.success(), "toolchain_identity_failed")?;
    String::from_utf8(output.stdout).map_err(|_| SmokeError("toolchain_identity_failed"))
}

fn prepared_toolchain(source: &Source, cargo: &Path, rustc: &Path) -> Result<Option<Value>> {
    if std::env::var_os("HIROUTE_VALIDATION_EXECUTION").is_none() {
        return Ok(None);
    }
    let path = std::env::var_os("HIROUTE_VALIDATION_TOOLCHAIN_JSON")
        .ok_or(SmokeError("prepared_toolchain_missing"))?;
    let record: Value = serde_json::from_slice(&fs::read(path)?)?;
    require(
        record["schema"] == "hiroute.validation-toolchain/v1"
            && record["sha"] == source.revision
            && record["cargo"]["path"].as_str() == cargo.to_str()
            && record["rustc"]["path"].as_str() == rustc.to_str()
            && record["cargo"]["sha256"] == digest(&fs::read(cargo)?)
            && record["rustc"]["sha256"] == digest(&fs::read(rustc)?)
            && record["cargo"]["version"]
                .as_str()
                .is_some_and(|version| version.starts_with("cargo "))
            && record["rustc"]["version"]
                .as_str()
                .is_some_and(|version| version.starts_with("rustc ")),
        "prepared_toolchain_changed",
    )?;
    Ok(Some(record))
}

pub(super) fn cargo_config_digest() -> Result<String> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or(SmokeError("cargo_home_unavailable"))?;
    let mut inputs = Vec::new();
    for name in ["config.toml", "config"] {
        let path = home.join(name);
        inputs.push((
            name,
            if path.exists() {
                Some(digest(&fs::read(path)?))
            } else {
                None
            },
        ));
    }
    Ok(digest(&serde_json::to_vec(&inputs)?))
}

fn recipe(source: &Source, package: &str, binary: &str) -> Result<BuildRecipe> {
    let cargo = PathBuf::from(env!("CARGO")).canonicalize()?;
    let rustc = cargo.with_file_name("rustc");
    let prepared = prepared_toolchain(source, &cargo, &rustc)?;
    let wrapper = std::env::var_os("RUSTC_WRAPPER").filter(|v| !v.is_empty());
    let wrapper = if let Some(path) = wrapper {
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(&path))
                .find(|p| p.is_file())
                .ok_or(SmokeError("wrapper_unavailable"))?
        }
        .canonicalize()?;
        require(
            path.file_name().and_then(|n| n.to_str()) == Some("sccache"),
            "untrusted_compiler_wrapper",
        )?;
        let version = if let Some(record) = &prepared {
            require(
                record["wrapper"]["path"].as_str() == path.to_str()
                    && record["wrapper"]["sha256"] == digest(&fs::read(&path)?),
                "prepared_toolchain_changed",
            )?;
            record["wrapper"]["version"]
                .as_str()
                .ok_or(SmokeError("prepared_toolchain_changed"))?
                .to_owned()
        } else {
            let output = Command::new(&path).arg("--version").output()?;
            require(output.status.success(), "wrapper_identity_failed")?;
            String::from_utf8(output.stdout).map_err(|_| SmokeError("wrapper_identity_failed"))?
        };
        require(
            version.starts_with("sccache "),
            "untrusted_compiler_wrapper",
        )?;
        Some((digest(&fs::read(path)?), version.trim().to_owned()))
    } else {
        None
    };
    let environment = std::env::vars()
        .filter(|(key, _)| {
            [
                "CARGO_",
                "RUST",
                "CC_",
                "CXX_",
                "CMAKE_",
                "PKG_CONFIG_",
                "OPENSSL_",
            ]
            .iter()
            .any(|prefix| key.starts_with(prefix))
                || matches!(
                    key.as_str(),
                    "PATH"
                        | "CC"
                        | "CXX"
                        | "CFLAGS"
                        | "CXXFLAGS"
                        | "CPPFLAGS"
                        | "LDFLAGS"
                        | "HOME"
                        | "SDKROOT"
                )
        })
        .collect();
    Ok(BuildRecipe {
        source_revision: source.revision.clone(),
        source_tree: source.tree.clone(),
        source_inputs: source.inputs.clone(),
        package: package.into(),
        binary: binary.into(),
        cargo: cargo.to_string_lossy().into_owned(),
        cargo_sha256: digest(&fs::read(&cargo)?),
        cargo_version: if let Some(record) = &prepared {
            record["cargo"]["version"]
                .as_str()
                .ok_or(SmokeError("prepared_toolchain_changed"))?
                .to_owned()
        } else {
            command_version(&cargo)?
        },
        rustc_sha256: digest(&fs::read(&rustc)?),
        rustc_version: if let Some(record) = &prepared {
            record["rustc"]["version"]
                .as_str()
                .ok_or(SmokeError("prepared_toolchain_changed"))?
                .to_owned()
        } else {
            command_version(&rustc)?
        },
        wrapper_sha256: wrapper.as_ref().map(|w| w.0.clone()),
        wrapper_version: wrapper.map(|w| w.1),
        cargo_config_sha256: cargo_config_digest()?,
        environment,
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    })
}

/// The caller owns the checkout smoke lock. A bad existing receipt is an error,
/// never an excuse to silently rebuild and turn corrupt provenance green.
pub(super) fn get_or_build(
    source: &Source,
    package: &str,
    binary: &str,
    logs: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<Artifact> {
    source.verify()?;
    let inputs = recipe(source, package, binary)?;
    let key = digest(&serde_json::to_vec(&inputs)?);
    let root = source.root.join("target/smoke/builds");
    if !root.exists() {
        fs::DirBuilder::new().mode(0o700).create(&root)?;
    }
    require(
        !fs::symlink_metadata(&root)?.file_type().is_symlink()
            && fs::metadata(&root)?.permissions().mode() & 0o077 == 0,
        "unsafe_build_cache",
    )?;
    let directory = root.join(&key[7..]).join(package);
    let executable = directory.join(binary);
    let receipt = directory.join("receipt.json");
    if directory.exists() {
        require(
            !fs::symlink_metadata(&directory)?.file_type().is_symlink()
                && fs::metadata(&directory)?.permissions().mode() & 0o077 == 0,
            "unsafe_build_cache",
        )?;
    }
    if receipt.exists() {
        require(
            !fs::symlink_metadata(&receipt)?.file_type().is_symlink(),
            "unsafe_build_cache",
        )?;
        let stored: BuildReceipt = serde_json::from_slice(&fs::read(&receipt)?)?;
        require(
            stored.recipe == inputs && stored.path == executable,
            "build_cache_recipe_changed",
        )?;
        let mut artifact = stored.artifact;
        artifact.path = stored.path;
        artifact.verify(source)?;
        return Ok(artifact);
    }
    require(
        std::env::var_os("HIROUTE_SMOKE_REQUIRE_PREPARED").is_none(),
        "prepared_build_missing",
    )?;
    require(!directory.exists(), "incomplete_build_cache")?;
    let key_dir = directory.parent().ok_or(SmokeError("unsafe_build_cache"))?;
    if !key_dir.exists() {
        fs::DirBuilder::new().mode(0o700).create(key_dir)?;
    }
    require(
        !fs::symlink_metadata(key_dir)?.file_type().is_symlink()
            && fs::metadata(key_dir)?.permissions().mode() & 0o077 == 0,
        "unsafe_build_cache",
    )?;
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut artifact = build(source, package, binary, logs, cancel)?;
    let original_digest = artifact.sha256.clone();
    fs::copy(&artifact.path, &executable)?;
    artifact.path = executable.clone();
    artifact.verify(source)?;
    require(artifact.sha256 == original_digest, "artifact_changed")?;
    let record = BuildReceipt {
        recipe: inputs,
        artifact: artifact.clone(),
        path: executable,
    };
    let bytes = serde_json::to_vec_pretty(&record)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(receipt)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(artifact)
}

pub(super) fn build(
    source: &Source,
    package: &str,
    binary: &str,
    logs: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<Artifact> {
    require(
        matches!(
            (package, binary),
            ("hiroute-e2e", "hiroute-e2e")
                | ("hiroute-cli", "hiroute")
                | ("hiroute-daemon", "hirouted")
        ),
        "unknown_build_target",
    )?;
    source.verify()?;
    let cargo = PathBuf::from(env!("CARGO"));
    let wrapper = std::env::var_os("RUSTC_WRAPPER")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let (wrapper, wrapper_sha256, wrapper_version) = if let Some(path) = wrapper {
        let path = if path.is_absolute() {
            path
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(&path))
                .find(|p| p.is_file())
                .ok_or(SmokeError("wrapper_unavailable"))?
        }
        .canonicalize()?;
        require(
            path.file_name().and_then(|n| n.to_str()) == Some("sccache"),
            "untrusted_compiler_wrapper",
        )?;
        let version = Command::new(&path).arg("--version").output()?;
        require(version.status.success(), "wrapper_identity_failed")?;
        let version =
            String::from_utf8(version.stdout).map_err(|_| SmokeError("wrapper_identity_failed"))?;
        require(
            version.starts_with("sccache "),
            "untrusted_compiler_wrapper",
        )?;
        let hash = digest(&std::fs::read(&path)?);
        (Some(path), Some(hash), Some(version.trim().to_owned()))
    } else {
        (None, None, None)
    };
    let command = |args: &[String]| {
        let mut c = Command::new(&cargo);
        c.current_dir(&source.root)
            .args(args)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS")
            .env("RUSTC", cargo.with_file_name("rustc"))
            .env("RUSTC_WORKSPACE_WRAPPER", "");
        if let Some(wrapper) = &wrapper {
            c.env("RUSTC_WRAPPER", wrapper);
        } else {
            c.env("RUSTC_WRAPPER", "");
        }
        c
    };
    // Metadata runs no compiler and catches configured external targets before any build.
    let metadata = command(&[
        "metadata".into(),
        "--locked".into(),
        "--format-version=1".into(),
        "--no-deps".into(),
    ])
    .output()?;
    require(metadata.status.success(), "cargo_metadata_failed")?;
    let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
    require(
        metadata["target_directory"].as_str() == source.root.join("target").to_str(),
        "external_target_forbidden",
    )?;
    let package_id = metadata["packages"]
        .as_array()
        .ok_or(SmokeError("cargo_metadata_invalid"))?
        .iter()
        .find(|p| p["name"] == package)
        .and_then(|p| p["id"].as_str())
        .ok_or(SmokeError("package_missing"))?;
    let argv = vec![
        "build".into(),
        "--locked".into(),
        "-p".into(),
        package.into(),
        "--bin".into(),
        binary.into(),
        "--profile".into(),
        "dev".into(),
        "--message-format=json-render-diagnostics".into(),
    ];
    let mut process = Process::spawn(&mut command(&argv), logs, cancel)?;
    let status = process.wait(Instant::now() + Duration::from_secs(3600), 64 * 1024 * 1024)?;
    require(status.success(), "cargo_build_failed")?;
    let (stdout, _) = process.output(64 * 1024 * 1024)?;
    let mut paths = Vec::new();
    for line in stdout.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let value: Value = serde_json::from_slice(line)?;
        if value["reason"] == "compiler-artifact"
            && value["package_id"] == package_id
            && value["target"]["name"] == binary
            && value["target"]["kind"] == serde_json::json!(["bin"])
            && let Some(path) = value["executable"].as_str()
        {
            paths.push(PathBuf::from(path).canonicalize()?);
        }
    }
    require(paths.len() == 1, "cargo_artifact_not_exact")?;
    let path = paths.remove(0);
    let version = |tool: &Path| -> Result<String> {
        let output = Command::new(tool)
            .arg("-vV")
            .current_dir(&source.root)
            .output()?;
        require(output.status.success(), "toolchain_identity_failed")?;
        String::from_utf8(output.stdout).map_err(|_| SmokeError("toolchain_identity_failed"))
    };
    let artifact = Artifact {
        package: package.into(),
        binary: binary.into(),
        sha256: digest(&std::fs::read(&path)?),
        source_revision: source.revision.clone(),
        build_argv: argv,
        cargo_version: version(&cargo)?,
        rustc_version: version(&cargo.with_file_name("rustc"))?,
        wrapper_sha256,
        wrapper_version,
        path,
    };
    artifact.verify(source)?;
    Ok(artifact)
}
