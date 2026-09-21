use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

use super::types::{
    ProductionError, ProductionProfile, ResolvedSut, SEALED_SUT_BUILD_INPUT_DIGEST,
    SEALED_SUT_REVISION, SEALED_SUT_TREE, SUT_BUILD_ATTESTATION_SCHEMA, SutBuildAttestation,
    path_text,
};
use crate::p0::canonical::sha256_hex;

const BUILD_INPUTS: [&str; 7] = [
    "Cargo.toml",
    "Cargo.lock",
    ".cargo",
    "rust-toolchain",
    "rust-toolchain.toml",
    "crates/gateway",
    "crates/gateway-core",
];

pub(super) fn resolve(profile: &ProductionProfile) -> Result<ResolvedSut, ProductionError> {
    let requested = std::env::var_os("HIROUTE_E2E_SUT_BIN")
        .map(PathBuf::from)
        .ok_or_else(|| ProductionError::Provenance("HIROUTE_E2E_SUT_BIN is not set".into()))?;
    let requested = requested.canonicalize().map_err(|error| {
        ProductionError::Provenance(format!("cannot resolve HIROUTE_E2E_SUT_BIN: {error}"))
    })?;
    require_executable(&requested)?;
    let source_checkout = source_checkout_for(&requested)?;
    verify_source_checkout(&source_checkout)?;
    let build_nonce = random_hex()?;
    let toolchain = resolve_toolchain(&source_checkout)?;
    let build_command = build_command(&build_nonce);
    let artifact = rebuild(&source_checkout, &toolchain, &build_command)?;
    if artifact != requested {
        return Err(ProductionError::Provenance(format!(
            "HIROUTE_E2E_SUT_BIN is not the cargo-reported hirouted artifact: supplied {}, trusted build {}",
            requested.display(),
            artifact.display()
        )));
    }
    require_executable(&artifact)?;
    let executable_sha256 = sha256_hex(&std::fs::read(&artifact)?);
    if let Ok(expected) = std::env::var("HIROUTE_E2E_SUT_REVISION")
        && expected != profile.sut_source_revision
    {
        return Err(ProductionError::Provenance(format!(
            "SUT revision mismatch: profile {}, environment {expected}",
            profile.sut_source_revision
        )));
    }
    let attestation = SutBuildAttestation {
        schema_version: SUT_BUILD_ATTESTATION_SCHEMA.into(),
        source_revision: profile.sut_source_revision.clone(),
        sealed_source_tree: SEALED_SUT_TREE.into(),
        build_input_digest: SEALED_SUT_BUILD_INPUT_DIGEST.into(),
        source_checkout: path_text(&source_checkout)?,
        cargo_package: "hiroute-gateway".into(),
        cargo_binary: "hirouted".into(),
        cargo_profile: "dev".into(),
        enabled_features: vec!["all".into()],
        target_triple: toolchain.target_triple,
        cargo_version: toolchain.cargo_version,
        rustc_version: toolchain.rustc_version,
        rustc_wrapper: toolchain
            .rustc_wrapper
            .as_deref()
            .map(path_text)
            .transpose()?,
        rustc_wrapper_version: toolchain.rustc_wrapper_version,
        toolchain_digest: toolchain.digest,
        build_nonce,
        build_command,
        executable_path: path_text(&artifact)?,
        executable_sha256: executable_sha256.clone(),
    };
    verify(&attestation)?;
    Ok(ResolvedSut {
        canonical_path: artifact,
        executable_sha256,
        source_revision: profile.sut_source_revision.clone(),
        build_attestation: attestation,
    })
}

pub(super) fn verify(attestation: &SutBuildAttestation) -> Result<(), ProductionError> {
    verify_identity(
        attestation,
        SUT_BUILD_ATTESTATION_SCHEMA,
        SEALED_SUT_REVISION,
        SEALED_SUT_TREE,
        SEALED_SUT_BUILD_INPUT_DIGEST,
    )
}

pub(super) fn verify_identity(
    attestation: &SutBuildAttestation,
    schema: &str,
    revision: &str,
    tree: &str,
    inputs: &str,
) -> Result<(), ProductionError> {
    // The sealed V1 oracle requires a unique rebuild. Current-checkout V3
    // records a fresh challenge in the run evidence while letting Cargo decide
    // whether the unchanged build inputs can reuse an existing artifact.
    let command_matches = if schema == SUT_BUILD_ATTESTATION_SCHEMA {
        attestation.build_command == build_command(&attestation.build_nonce)
    } else if schema == "hiroute.e2e.sut-build-attestation/v3" {
        attestation.build_command == current_build_command()
    } else {
        false
    };
    let source_checkout = Path::new(&attestation.source_checkout);
    let executable = Path::new(&attestation.executable_path);
    let wrapper_pair_is_valid = match (
        attestation.rustc_wrapper.as_deref(),
        attestation.rustc_wrapper_version.as_deref(),
    ) {
        (None, None) => true,
        (Some(wrapper), Some(version)) => {
            Path::new(wrapper).is_absolute()
                && Path::new(wrapper)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(executable_name("sccache"))
                && version.starts_with("sccache ")
        }
        _ => false,
    };
    let executable_name_is_valid = executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == executable_name("hirouted"));
    if attestation.schema_version != schema
        || attestation.source_revision != revision
        || attestation.sealed_source_tree != tree
        || attestation.build_input_digest != inputs
        || !source_checkout.is_absolute()
        || !executable.is_absolute()
        || !executable.starts_with(source_checkout.join("target"))
        || !executable_name_is_valid
        || attestation.cargo_package != "hiroute-gateway"
        || attestation.cargo_binary != "hirouted"
        || attestation.cargo_profile != "dev"
        || attestation.enabled_features != ["all"]
        || attestation.target_triple.is_empty()
        || attestation.cargo_version.is_empty()
        || !attestation.cargo_version.starts_with("cargo ")
        || attestation.rustc_version.is_empty()
        || !attestation.rustc_version.starts_with("rustc ")
        || !wrapper_pair_is_valid
        || attestation.toolchain_digest
            != toolchain_digest(
                &attestation.cargo_version,
                &attestation.rustc_version,
                &attestation.target_triple,
                attestation.rustc_wrapper.as_deref(),
                attestation.rustc_wrapper_version.as_deref(),
            )
        || !lower_hex(&attestation.build_nonce, 64)
        || !command_matches
        || !valid_sha256(&attestation.executable_sha256)
    {
        return Err(ProductionError::Provenance(
            "SUT build attestation is not the exact trusted rebuild contract".into(),
        ));
    }
    Ok(())
}

pub(super) struct Toolchain {
    pub(super) cargo: PathBuf,
    pub(super) cargo_version: String,
    pub(super) rustc_version: String,
    pub(super) target_triple: String,
    pub(super) rustc_wrapper: Option<PathBuf>,
    pub(super) rustc_wrapper_version: Option<String>,
    pub(super) digest: String,
}

pub(super) fn resolve_toolchain(source_checkout: &Path) -> Result<Toolchain, ProductionError> {
    let cargo = PathBuf::from(env!("CARGO"));
    if std::env::var_os("HIROUTE_VALIDATION_EXECUTION").is_some() {
        let path = std::env::var_os("HIROUTE_VALIDATION_TOOLCHAIN_JSON").ok_or_else(|| {
            ProductionError::Provenance("prepared toolchain identity is missing".into())
        })?;
        let record: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let rustc = cargo.with_file_name(executable_name("rustc"));
        let revision = git_text(
            source_checkout,
            &["rev-parse", "HEAD"],
            "candidate revision",
        )?;
        let wrapper = find_on_path(executable_name("sccache"))
            .map(|path| path.canonicalize())
            .transpose()?;
        let wrapper_matches = if let Some(path) = &wrapper {
            record["wrapper"]["path"].as_str() == path.to_str()
                && record["wrapper"]["sha256"] == sha256_hex(&std::fs::read(path)?)
                && record["wrapper"]["version"]
                    .as_str()
                    .is_some_and(|value| value.starts_with("sccache "))
        } else {
            record["wrapper"].is_null()
        };
        if record["schema"] != "hiroute.validation-toolchain/v1"
            || record["sha"] != revision
            || record["cargo"]["path"].as_str() != cargo.to_str()
            || record["rustc"]["path"].as_str() != rustc.to_str()
            || record["cargo"]["sha256"] != sha256_hex(&std::fs::read(&cargo)?)
            || record["rustc"]["sha256"] != sha256_hex(&std::fs::read(&rustc)?)
            || !wrapper_matches
        {
            return Err(ProductionError::Provenance(
                "prepared toolchain identity changed".into(),
            ));
        }
        let (cargo_version, rustc_version, target_triple) = prepared_versions(&record)?;
        let wrapper_version = wrapper.as_ref().map(|_| {
            record["wrapper"]["version"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        });
        let digest = toolchain_digest(
            &cargo_version,
            &rustc_version,
            &target_triple,
            wrapper.as_deref().and_then(Path::to_str),
            wrapper_version.as_deref(),
        );
        return Ok(Toolchain {
            cargo,
            cargo_version,
            rustc_version,
            target_triple,
            rustc_wrapper: wrapper,
            rustc_wrapper_version: wrapper_version,
            digest,
        });
    }
    let cargo_output = checked_output(
        Command::new(&cargo).arg("-Vv").current_dir(source_checkout),
        "cargo toolchain identity",
    )?;
    let rustc = cargo.with_file_name(executable_name("rustc"));
    let rustc_output = checked_output(
        Command::new(&rustc).arg("-vV").current_dir(source_checkout),
        "rustc toolchain identity",
    )?;
    let cargo_text = utf8(&cargo_output.stdout, "cargo version")?;
    let rustc_text = utf8(&rustc_output.stdout, "rustc version")?;
    let cargo_version = first_line(cargo_text, "cargo version")?;
    let rustc_version = first_line(rustc_text, "rustc version")?;
    let target_triple = rustc_text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProductionError::Provenance("rustc host target is missing".into()))?
        .to_owned();
    let (rustc_wrapper, rustc_wrapper_version) = resolve_sccache(source_checkout)?;
    let digest = toolchain_digest(
        &cargo_version,
        &rustc_version,
        &target_triple,
        rustc_wrapper.as_deref().and_then(Path::to_str),
        rustc_wrapper_version.as_deref(),
    );
    Ok(Toolchain {
        cargo,
        cargo_version,
        rustc_version,
        target_triple,
        rustc_wrapper,
        rustc_wrapper_version,
        digest,
    })
}

fn prepared_versions(record: &Value) -> Result<(String, String, String), ProductionError> {
    let cargo = first_line(
        record["cargo"]["version"].as_str().unwrap_or(""),
        "prepared cargo version",
    )?;
    let rustc_text = record["rustc"]["version"].as_str().unwrap_or("");
    let rustc = first_line(rustc_text, "prepared rustc version")?;
    let target = record["target_triple"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProductionError::Provenance("prepared host target is missing".into()))?;
    if !cargo.starts_with("cargo ")
        || !rustc.starts_with("rustc ")
        || !rustc_text
            .lines()
            .any(|line| line == format!("host: {target}"))
    {
        return Err(ProductionError::Provenance(
            "prepared toolchain version or host changed".into(),
        ));
    }
    Ok((cargo, rustc, target.to_owned()))
}

fn resolve_sccache(
    source_checkout: &Path,
) -> Result<(Option<PathBuf>, Option<String>), ProductionError> {
    let Some(path) = find_on_path(executable_name("sccache")) else {
        return Ok((None, None));
    };
    let path = path.canonicalize()?;
    let output = checked_output(
        Command::new(&path)
            .arg("--version")
            .current_dir(source_checkout),
        "sccache identity",
    )?;
    let version = first_line(utf8(&output.stdout, "sccache version")?, "sccache version")?;
    if !version.starts_with("sccache ") {
        return Err(ProductionError::Provenance(
            "RUSTC_WRAPPER is not the expected sccache implementation".into(),
        ));
    }
    Ok((Some(path), Some(version)))
}

pub(super) fn rebuild(
    source_checkout: &Path,
    toolchain: &Toolchain,
    build_command: &[String],
) -> Result<PathBuf, ProductionError> {
    let metadata = checked_output(
        Command::new(&toolchain.cargo)
            .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
            .current_dir(source_checkout)
            .env_remove("CARGO_TARGET_DIR"),
        "Cargo target preflight",
    )?;
    let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
    if metadata["target_directory"].as_str() != source_checkout.join("target").to_str() {
        return Err(ProductionError::Provenance(
            "external Cargo target is forbidden".into(),
        ));
    }
    let package_id = metadata["packages"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["name"] == "hiroute-gateway"))
        .and_then(|item| item["id"].as_str())
        .ok_or_else(|| ProductionError::Provenance("Gateway package identity is missing".into()))?;
    let mut command = Command::new(&toolchain.cargo);
    command
        .args(&build_command[1..])
        .current_dir(source_checkout)
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .env(
            "RUSTC",
            toolchain.cargo.with_file_name(executable_name("rustc")),
        )
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTFLAGS");
    if let Some(wrapper) = &toolchain.rustc_wrapper {
        command.env("RUSTC_WRAPPER", wrapper);
    } else {
        command.env("RUSTC_WRAPPER", "");
    }
    let output = checked_output(&mut command, "trusted hirouted rebuild")?;
    let stdout = utf8(&output.stdout, "cargo build messages")?;
    let mut artifacts = Vec::new();
    for line in stdout.lines() {
        let value: Value = serde_json::from_str(line).map_err(|error| {
            ProductionError::Provenance(format!("cargo emitted corrupt JSON: {error}"))
        })?;
        if value["reason"] == "compiler-artifact"
            && value["package_id"] == package_id
            && value["target"]["name"] == "hirouted"
            && value["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
            && let Some(path) = value["executable"].as_str()
        {
            artifacts.push(PathBuf::from(path));
        }
    }
    if artifacts.len() != 1 {
        return Err(ProductionError::Provenance(format!(
            "trusted build reported {} hirouted artifacts, expected one",
            artifacts.len()
        )));
    }
    artifacts.remove(0).canonicalize().map_err(Into::into)
}

pub(super) fn build_command(build_nonce: &str) -> Vec<String> {
    vec![
        "cargo".into(),
        "rustc".into(),
        "--locked".into(),
        "-p".into(),
        "hiroute-gateway".into(),
        "--bin".into(),
        "hirouted".into(),
        "--profile".into(),
        "dev".into(),
        "--all-features".into(),
        "--message-format=json-render-diagnostics".into(),
        "--".into(),
        "-C".into(),
        format!("metadata=hiroute_e2e_{build_nonce}"),
    ]
}

pub(super) fn current_build_command() -> Vec<String> {
    vec![
        "cargo".into(),
        "build".into(),
        "--locked".into(),
        "-p".into(),
        "hiroute-gateway".into(),
        "--bin".into(),
        "hirouted".into(),
        "--profile".into(),
        "dev".into(),
        "--all-features".into(),
        "--message-format=json-render-diagnostics".into(),
    ]
}

fn source_checkout_for(executable: &Path) -> Result<PathBuf, ProductionError> {
    let parent = executable.parent().ok_or_else(|| {
        ProductionError::Provenance("SUT executable has no parent directory".into())
    })?;
    let output = checked_output(
        Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(parent),
        "SUT source checkout discovery",
    )?;
    let source_checkout = PathBuf::from(utf8(&output.stdout, "SUT source checkout")?.trim());
    source_checkout.canonicalize().map_err(Into::into)
}

fn verify_source_checkout(source_checkout: &Path) -> Result<(), ProductionError> {
    let tree = git_text(
        source_checkout,
        &["rev-parse", &format!("{SEALED_SUT_REVISION}^{{tree}}")],
        "sealed SUT tree",
    )?;
    let listing = git_output(
        source_checkout,
        ["ls-tree", "-r", SEALED_SUT_REVISION, "--"]
            .into_iter()
            .chain(BUILD_INPUTS)
            .collect::<Vec<_>>(),
        "sealed SUT build inputs",
    )?;
    let mut diff_args = vec!["diff", "--quiet", SEALED_SUT_REVISION, "--"];
    diff_args.extend(BUILD_INPUTS);
    let unchanged = Command::new("git")
        .args(&diff_args)
        .current_dir(source_checkout)
        .status()?;
    let mut status_args = vec!["status", "--porcelain", "--untracked-files=all", "--"];
    status_args.extend(BUILD_INPUTS);
    let status = git_output(source_checkout, status_args, "SUT build input status")?;
    if tree != SEALED_SUT_TREE
        || sha256_hex(&listing.stdout) != SEALED_SUT_BUILD_INPUT_DIGEST
        || !unchanged.success()
        || !status.stdout.is_empty()
    {
        return Err(ProductionError::Provenance(
            "SUT checkout build inputs differ from the sealed revision".into(),
        ));
    }
    Ok(())
}

pub(super) fn require_executable(path: &Path) -> Result<(), ProductionError> {
    if !path.is_file() {
        return Err(ProductionError::Provenance(
            "HIROUTE_E2E_SUT_BIN is not a file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)?.permissions().mode() & 0o111 == 0 {
            return Err(ProductionError::Provenance(
                "HIROUTE_E2E_SUT_BIN is not executable".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn random_hex() -> Result<String, ProductionError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| ProductionError::Provenance(format!("randomness unavailable: {error}")))?;
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    Ok(output)
}

pub(super) fn git_text(
    source_checkout: &Path,
    args: &[&str],
    label: &str,
) -> Result<String, ProductionError> {
    let output = git_output(source_checkout, args.iter().copied(), label)?;
    Ok(utf8(&output.stdout, label)?.trim().to_owned())
}

pub(super) fn git_output<I, S>(
    source_checkout: &Path,
    args: I,
    label: &str,
) -> Result<Output, ProductionError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    checked_output(
        Command::new("git").args(args).current_dir(source_checkout),
        label,
    )
}

fn checked_output(command: &mut Command, label: &str) -> Result<Output, ProductionError> {
    let output = command.output()?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(ProductionError::Provenance(format!(
            "{label} failed with {}: {}",
            output.status,
            stderr.chars().take(2_000).collect::<String>()
        )))
    }
}

fn utf8<'a>(bytes: &'a [u8], label: &str) -> Result<&'a str, ProductionError> {
    std::str::from_utf8(bytes)
        .map_err(|error| ProductionError::Provenance(format!("{label} is not UTF-8: {error}")))
}

fn first_line(value: &str, label: &str) -> Result<String, ProductionError> {
    value
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ProductionError::Provenance(format!("{label} is empty")))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn executable_name(stem: &str) -> &str {
    if cfg!(windows) {
        match stem {
            "cargo" => "cargo.exe",
            "hirouted" => "hirouted.exe",
            "rustc" => "rustc.exe",
            "sccache" => "sccache.exe",
            _ => stem,
        }
    } else {
        stem
    }
}

fn valid_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| lower_hex(digest, 64))
}

fn toolchain_digest(
    cargo_version: &str,
    rustc_version: &str,
    target_triple: &str,
    rustc_wrapper: Option<&str>,
    rustc_wrapper_version: Option<&str>,
) -> String {
    sha256_hex(
        format!(
            "cargo={cargo_version}\nrustc={rustc_version}\ntarget={target_triple}\nwrapper={}\nwrapper-version={}",
            rustc_wrapper.unwrap_or("none"),
            rustc_wrapper_version.unwrap_or("none")
        )
        .as_bytes(),
    )
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod current_contract_tests {
    use super::*;

    #[test]
    fn prepared_toolchain_rejects_forged_version_and_host() {
        let mut record = serde_json::json!({
            "cargo": {"version": "cargo 1.97.1\nrelease: 1.97.1\n"},
            "rustc": {"version": "rustc 1.97.1\nhost: x86_64-unknown-linux-gnu\n"},
            "target_triple": "x86_64-unknown-linux-gnu",
        });
        assert!(prepared_versions(&record).is_ok());
        record["cargo"]["version"] = "forged 1.97.1".into();
        assert!(prepared_versions(&record).is_err());
        record["cargo"]["version"] = "cargo 1.97.1".into();
        record["target_triple"] = "aarch64-unknown-linux-gnu".into();
        assert!(prepared_versions(&record).is_err());
        record["target_triple"] = "x86_64-unknown-linux-gnu".into();
        record["rustc"]["version"] = "forged 1.97.1\nhost: x86_64-unknown-linux-gnu".into();
        assert!(prepared_versions(&record).is_err());
    }

    #[test]
    fn current_v3_accepts_stable_cargo_identity_and_rejects_old_or_forged_recipes() {
        const V3: &str = "hiroute.e2e.sut-build-attestation/v3";
        let revision = "a".repeat(40);
        let tree = "b".repeat(40);
        let inputs = format!("sha256:{}", "c".repeat(64));
        let cargo = "cargo 1.97.1";
        let rustc = "rustc 1.97.1";
        let target = "x86_64-unknown-linux-gnu";
        let mut attestation = SutBuildAttestation {
            schema_version: V3.into(),
            source_revision: revision.clone(),
            sealed_source_tree: tree.clone(),
            build_input_digest: inputs.clone(),
            source_checkout: "/tmp/hiroute-candidate".into(),
            cargo_package: "hiroute-gateway".into(),
            cargo_binary: "hirouted".into(),
            cargo_profile: "dev".into(),
            enabled_features: vec!["all".into()],
            target_triple: target.into(),
            cargo_version: cargo.into(),
            rustc_version: rustc.into(),
            rustc_wrapper: None,
            rustc_wrapper_version: None,
            toolchain_digest: toolchain_digest(cargo, rustc, target, None, None),
            build_nonce: "d".repeat(64),
            build_command: current_build_command(),
            executable_path: "/tmp/hiroute-candidate/target/smoke/hirouted".into(),
            executable_sha256: format!("sha256:{}", "e".repeat(64)),
        };
        assert_eq!(attestation.build_command.len(), 11);
        assert!(verify_identity(&attestation, V3, &revision, &tree, &inputs).is_ok());
        attestation.build_command = build_command(&attestation.build_nonce);
        assert!(verify_identity(&attestation, V3, &revision, &tree, &inputs).is_err());
        attestation.build_command = current_build_command();
        attestation.enabled_features = vec![];
        assert!(verify_identity(&attestation, V3, &revision, &tree, &inputs).is_err());
        attestation.enabled_features = vec!["all".into()];
        attestation.cargo_binary = "other".into();
        assert!(verify_identity(&attestation, V3, &revision, &tree, &inputs).is_err());
        attestation.cargo_binary = "hirouted".into();
        attestation.cargo_version = "cargo changed".into();
        assert!(verify_identity(&attestation, V3, &revision, &tree, &inputs).is_err());
    }
}
