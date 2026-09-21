#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/check-p0-gateway-final-result.sh \
  --result PATH --expected-revision SHA [--e2e-root PATH]

Requires a sealed hiroute.e2e.production-result/v1 report to be green,
complete, digest-valid, free of filtered or skipped evidence, and bound to the
specified sealed SUT revision. The caller binds that SUT revision to the exact
checked-out Gate revision before invoking this checker.
USAGE
}

result=''
expected_revision=''
e2e_root='e2e'
while (($#)); do
  case "$1" in
    --result)
      result=${2:-}
      shift 2
      ;;
    --expected-revision)
      expected_revision=${2:-}
      shift 2
      ;;
    --e2e-root)
      e2e_root=${2:-}
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! -f "$result" || ! -d "$e2e_root/schema" || ! "$expected_revision" =~ ^[0-9a-f]{40}$ ]]; then
  usage >&2
  exit 2
fi

python3 - "$result" "$expected_revision" "$e2e_root" <<'PY'
"""Fail-closed verifier for a sealed production Oracle result."""

from __future__ import annotations

import hashlib
import json
import pathlib
import re
import sys
from typing import Any


RESULT_PATH = pathlib.Path(sys.argv[1])
EXPECTED_REVISION = sys.argv[2]
E2E_ROOT = pathlib.Path(sys.argv[3])
SCHEMA_ROOT = E2E_ROOT / "schema"
SHA256 = re.compile(r"sha256:[0-9a-f]{64}\Z")
REQUIRED_CHECKPOINTS = (
    "listener_client",
    "native_provider",
    "semantic_observation",
    "privacy_resource",
)
EXPECTED_ARTIFACT_SCHEMAS = {
    "fixtures/p0-oracle/production-smoke.json": "schema/p0-production-fixture.schema.json",
    "profiles/gateway-isolated.json": "schema/p0-gateway-profile.schema.json",
    "scenarios/p0-gateway.json": "schema/p0-gateway-scenario.schema.json",
    "schema/p0-gateway-profile.schema.json": None,
    "schema/p0-gateway-result.schema.json": None,
    "schema/p0-gateway-scenario.schema.json": None,
    "schema/p0-production-collector.schema.json": None,
    "schema/p0-production-fixture.schema.json": None,
    "schema/p0-production-launcher.schema.json": None,
    "schema/p0-production-manifest.schema.json": None,
    "schema/p0-production-readiness.schema.json": None,
}


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def reject_nonfinite(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value!r}")


def read_json(path: pathlib.Path) -> Any:
    try:
        return json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=reject_nonfinite,
        )
    except (OSError, ValueError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read exact JSON {path}: {error}") from error


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def digest(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical_json(value)).hexdigest()


def materialize_freshness(value: Any, challenge: str) -> Any:
    """Apply the production fixture's exact freshness projection."""

    if value == "${RUN_CHALLENGE}":
        return challenge
    if isinstance(value, list):
        return [materialize_freshness(item, challenge) for item in value]
    if isinstance(value, dict):
        return {key: materialize_freshness(item, challenge) for key, item in value.items()}
    return value


def optional_field_digest(body: dict[str, Any], field: str) -> Any:
    return digest(body[field]) if field in body else None


def native_request_semantics(protocol: str, body: dict[str, Any]) -> dict[str, Any]:
    """Mirror the frozen production Oracle's non-secret Provider projection."""

    if protocol == "responses":
        instructions, content, reasoning = "instructions", "input", "reasoning"
    elif protocol == "chat_completions":
        instructions, content, reasoning = None, "messages", "reasoning_effort"
    elif protocol == "messages":
        instructions, content = "system", "messages"
        reasoning = "thinking" if "thinking" in body else "output_config"
    else:
        raise ValueError("sealed Provider protocol is not recognized")
    return {
        "content_digest": optional_field_digest(body, content),
        "instructions_digest": None
        if instructions is None
        else optional_field_digest(body, instructions),
        "model": body.get("model"),
        "reasoning_digest": optional_field_digest(body, reasoning),
        "stream": body.get("stream", False),
        "tool_choice_digest": optional_field_digest(body, "tool_choice"),
        "tools_digest": optional_field_digest(body, "tools"),
    }


def frozen_expected_request_evidence(
    artifact_values: dict[str, Any], challenge: str, errors: list[str]
) -> tuple[Any, Any]:
    """Derive the frozen listener and Provider evidence, not just its labels.

    A result's digest is only meaningful if it commits to the sealed smoke
    request.  The Rust Oracle makes these exact projections before it emits a
    green result; doing the same here makes a non-empty but fabricated result
    fail just as a missing or zero result does.
    """

    fixture = require_object(
        artifact_values.get("fixtures/p0-oracle/production-smoke.json"),
        "sealed production fixture",
        errors,
    )
    expected_client = fixture.get("expected_client")
    if not isinstance(expected_client, dict) or not expected_client:
        errors.append("sealed production fixture has no listener client evidence")
        expected_client_value: Any = {}
    else:
        expected_client_value = materialize_freshness(expected_client, challenge)

    case = require_object(fixture.get("case"), "sealed production fixture case", errors)
    providers = case.get("providers")
    if not isinstance(providers, list) or len(providers) != 1:
        errors.append("sealed production fixture does not require exactly one native Provider call")
        return expected_client_value, {}
    provider = require_object(providers[0], "sealed native Provider", errors)
    if provider.get("expected_calls") != 1:
        errors.append("sealed production fixture does not require one native Provider call")
    provider_id = provider.get("id")
    protocol = provider.get("protocol")
    expected_request = require_object(provider.get("expected_request"), "sealed native Provider request", errors)
    path = expected_request.get("path")
    body = materialize_freshness(expected_request.get("body"), challenge)
    if (
        not isinstance(provider_id, str)
        or not isinstance(protocol, str)
        or not isinstance(path, str)
        or not isinstance(body, dict)
    ):
        errors.append("sealed production fixture native Provider request is incomplete")
        return expected_client_value, {}
    try:
        entry = {
            "authorization": "exact",
            "body_digest": digest(body),
            "body_semantics": native_request_semantics(protocol, body),
            "content_type": "application/json",
            "method": "POST",
            "ordinal": 1,
            "path": path,
            "protocol": protocol,
            "provider_id": provider_id,
            "body": body,
        }
    except ValueError as error:
        errors.append(str(error))
        return expected_client_value, {}
    return expected_client_value, {
        "entries": [entry],
        "accepted": 1,
        "active": 0,
        "parse_failed": 0,
        "aborted": 0,
        "infrastructure_failures": [],
    }


def require_object(value: Any, field: str, errors: list[str]) -> dict[str, Any]:
    if not isinstance(value, dict):
        errors.append(f"{field} is not an object")
        return {}
    return value


def require_sha256(value: Any, field: str, errors: list[str]) -> str:
    if not isinstance(value, str) or SHA256.fullmatch(value) is None:
        errors.append(f"{field} is not a sha256 digest")
        return ""
    return value


def schema_const(properties: dict[str, Any], field: str) -> Any:
    definition = properties.get(field)
    return definition.get("const") if isinstance(definition, dict) else None


def schema_errors(schema: Any, value: Any, path: str) -> list[str]:
    """Validate the sealed schema subset used by the production artifacts.

    Pulling a general JSON-schema dependency into this Gate would make the
    release decision depend on an unsealed Python environment. The checked-in
    schemas use this small deterministic Draft 2020-12 subset instead.
    """

    if not isinstance(schema, dict):
        return [f"{path}: schema is not an object"]
    if "anyOf" in schema:
        alternatives = schema["anyOf"]
        if not isinstance(alternatives, list) or not any(
            not schema_errors(alternative, value, path) for alternative in alternatives
        ):
            return [f"{path}: value does not satisfy any sealed schema alternative"]
        return []
    errors: list[str] = []
    if "const" in schema and value != schema["const"]:
        errors.append(f"{path}: value differs from the sealed schema constant")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{path}: value is outside the sealed schema enum")
    expected_type = schema.get("type")
    type_matches = {
        "object": isinstance(value, dict),
        "array": isinstance(value, list),
        "string": isinstance(value, str),
        "integer": isinstance(value, int) and not isinstance(value, bool),
        "number": isinstance(value, (int, float)) and not isinstance(value, bool),
        "boolean": isinstance(value, bool),
        "null": value is None,
    }
    if expected_type is not None and not type_matches.get(expected_type, False):
        return errors + [f"{path}: value does not match sealed schema type {expected_type}"]
    if isinstance(value, dict):
        required = schema.get("required", [])
        if not isinstance(required, list):
            errors.append(f"{path}: sealed schema required field set is malformed")
            required = []
        for key in required:
            if key not in value:
                errors.append(f"{path}: required sealed evidence field {key} is missing")
        minimum = schema.get("minProperties")
        if isinstance(minimum, int) and len(value) < minimum:
            errors.append(f"{path}: object has fewer than {minimum} sealed properties")
        properties = schema.get("properties", {})
        if not isinstance(properties, dict):
            errors.append(f"{path}: sealed schema properties are malformed")
            properties = {}
        if schema.get("additionalProperties") is False:
            extras = set(value) - set(properties)
            if extras:
                errors.append(f"{path}: object contains unsealed fields {sorted(extras)}")
        for key, nested in properties.items():
            if key in value:
                errors.extend(schema_errors(nested, value[key], f"{path}/{key}"))
    elif isinstance(value, list):
        minimum = schema.get("minItems")
        maximum = schema.get("maxItems")
        if isinstance(minimum, int) and len(value) < minimum:
            errors.append(f"{path}: array has fewer than {minimum} sealed items")
        if isinstance(maximum, int) and len(value) > maximum:
            errors.append(f"{path}: array has more than {maximum} sealed items")
        if schema.get("uniqueItems") is True:
            canonical_items = [canonical_json(item) for item in value]
            if len(set(canonical_items)) != len(canonical_items):
                errors.append(f"{path}: array has duplicate sealed items")
        item_schema = schema.get("items")
        if item_schema is not None:
            for index, item in enumerate(value):
                errors.extend(schema_errors(item_schema, item, f"{path}/{index}"))
    elif isinstance(value, str):
        minimum = schema.get("minLength")
        maximum = schema.get("maxLength")
        if isinstance(minimum, int) and len(value) < minimum:
            errors.append(f"{path}: string is shorter than the sealed minimum")
        if isinstance(maximum, int) and len(value) > maximum:
            errors.append(f"{path}: string is longer than the sealed maximum")
    elif isinstance(value, (int, float)) and not isinstance(value, bool):
        minimum = schema.get("minimum")
        if isinstance(minimum, (int, float)) and value < minimum:
            errors.append(f"{path}: number is below the sealed minimum")
    return errors


def validate_sealed_schemas(
    manifest: dict[str, Any], artifact_values: dict[str, Any], report: dict[str, Any], errors: list[str]
) -> None:
    targets = (
        ("schema/p0-production-manifest.schema.json", manifest, "manifest"),
        ("schema/p0-gateway-profile.schema.json", artifact_values.get("profiles/gateway-isolated.json"), "profile"),
        ("schema/p0-gateway-scenario.schema.json", artifact_values.get("scenarios/p0-gateway.json"), "scenario"),
        ("schema/p0-production-fixture.schema.json", artifact_values.get("fixtures/p0-oracle/production-smoke.json"), "fixture"),
        ("schema/p0-gateway-result.schema.json", report, "result"),
        ("schema/p0-production-launcher.schema.json", report.get("launcher"), "result/launcher"),
        ("schema/p0-production-readiness.schema.json", report.get("readiness"), "result/readiness"),
        ("schema/p0-production-collector.schema.json", report.get("observations"), "result/observations"),
    )
    for schema_path, value, label in targets:
        schema = artifact_values.get(schema_path)
        if not isinstance(schema, dict):
            errors.append(f"sealed schema is missing: {schema_path}")
            continue
        errors.extend(schema_errors(schema, value, label))


def reject_release_exclusions(value: Any, path: str, errors: list[str]) -> None:
    if isinstance(value, dict):
        for key, nested in value.items():
            if key in {"expected_red", "expected_red_code", "skip", "skipped"}:
                errors.append(f"forbidden release completion field at {path}/{key}")
            reject_release_exclusions(nested, f"{path}/{key}", errors)
    elif isinstance(value, list):
        for index, nested in enumerate(value):
            reject_release_exclusions(nested, f"{path}/{index}", errors)
    elif isinstance(value, str) and value in {"expected_red", "skip", "skipped"}:
        errors.append(f"forbidden release completion value at {path}")


def profile_aggregate_port_digest(artifact_values: dict[str, Any], errors: list[str]) -> str:
    profile = artifact_values.get("profiles/gateway-isolated.json")
    if not isinstance(profile, dict) or not isinstance(profile.get("aggregate_port_digest"), str):
        errors.append("sealed production profile omits aggregate_port_digest")
        return ""
    return profile["aggregate_port_digest"]


def manifest_and_schemas(errors: list[str]) -> tuple[dict[str, Any], dict[str, str], dict[str, Any]]:
    manifest = require_object(
        read_json(SCHEMA_ROOT / "p0-production-manifest.json"),
        "sealed production manifest",
        errors,
    )
    if manifest.get("schema_version") != "hiroute.e2e.production-manifest/v1":
        errors.append("sealed production manifest schema is not v1")
    if manifest.get("oracle_version") != "p0-gateway-production-oracle-v1":
        errors.append("sealed production manifest Oracle version is wrong")
    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list) or not artifacts:
        errors.append("sealed production manifest has no artifacts")
        return manifest, {}, {}

    artifact_paths: set[str] = set()
    schema_digests: dict[str, str] = {}
    artifact_values: dict[str, Any] = {}
    aggregate_items: list[dict[str, str]] = []
    for index, artifact in enumerate(artifacts):
        item = require_object(artifact, f"manifest artifact {index}", errors)
        path = item.get("path")
        expected = require_sha256(item.get("sha256"), f"manifest artifact {index} digest", errors)
        safe_path = isinstance(path, str) and path and not pathlib.PurePosixPath(path).is_absolute()
        safe_path = safe_path and ".." not in pathlib.PurePosixPath(path).parts
        if not safe_path:
            errors.append(f"manifest artifact {index} has an unsafe path")
            continue
        assert isinstance(path, str)
        if path in artifact_paths:
            errors.append(f"manifest repeats artifact {path}")
            continue
        artifact_paths.add(path)
        if item.get("schema") != EXPECTED_ARTIFACT_SCHEMAS.get(path):
            errors.append(f"manifest artifact schema binding is wrong for {path}")
        artifact_path = E2E_ROOT / path
        if not artifact_path.is_file():
            errors.append(f"manifested artifact is missing: {path}")
            continue
        try:
            artifact_path.resolve().relative_to(E2E_ROOT.resolve())
        except ValueError:
            errors.append(f"manifested artifact escapes the sealed E2E root: {path}")
            continue
        value = read_json(artifact_path)
        artifact_values[path] = value
        actual = digest(value)
        if expected and actual != expected:
            errors.append(f"manifested artifact digest is stale for {path}")
        aggregate_items.append({"path": path, "sha256": expected})
        if path.startswith("schema/"):
            schema_digests[path] = actual

    if artifact_paths != set(EXPECTED_ARTIFACT_SCHEMAS):
        errors.append("sealed production manifest artifact set is missing, filtered, or expanded")

    aggregate = digest(
        {
            "schema_version": "hiroute.e2e.production-contract-digest/v1",
            "aggregate_port_digest": profile_aggregate_port_digest(artifact_values, errors),
            "artifacts": aggregate_items,
        }
    )
    if manifest.get("contract_digest") != aggregate:
        errors.append("sealed production manifest contract digest is stale or corrupt")
    return manifest, schema_digests, artifact_values


def validate_exact_seal(
    manifest: dict[str, Any],
    schema_digests: dict[str, str],
    artifact_values: dict[str, Any],
    report: dict[str, Any],
    errors: list[str],
) -> None:
    profile = require_object(
        artifact_values.get("profiles/gateway-isolated.json"),
        "sealed production profile",
        errors,
    )
    launcher_schema = require_object(
        artifact_values.get("schema/p0-production-launcher.schema.json"),
        "sealed production launcher schema",
        errors,
    )
    if profile.get("sut_source_revision") != EXPECTED_REVISION:
        errors.append("sealed production profile revision does not match the expected SUT revision")
    properties = launcher_schema.get("properties")
    launcher_revision = schema_const(properties, "sut_source_revision") if isinstance(properties, dict) else None
    if launcher_revision != EXPECTED_REVISION:
        errors.append("sealed launcher schema revision does not match the expected SUT revision")
    if report.get("contract_digest") != manifest.get("contract_digest"):
        errors.append("result contract digest does not match the checked-in production seal")
    if report.get("aggregate_port_digest") != profile.get("aggregate_port_digest"):
        errors.append("result aggregate port digest does not match the checked-in production seal")
    if report.get("schema_digests") != schema_digests:
        errors.append("result schema digests do not match the checked-in production seal")


def validate_result_shape(report: dict[str, Any], artifact_values: dict[str, Any], errors: list[str]) -> None:
    schema = require_object(
        artifact_values.get("schema/p0-gateway-result.schema.json"),
        "sealed production result schema",
        errors,
    )
    properties = require_object(schema.get("properties"), "result schema properties", errors)
    required = schema.get("required")
    if not isinstance(required, list):
        errors.append("result schema has no required field set")
        return
    if set(report) != set(properties) or set(report) != set(required):
        errors.append("result does not have the exact production-result v1 field set")
    if report.get("schema_version") != schema_const(properties, "schema_version"):
        errors.append("result schema_version is not the sealed production-result v1 schema")
    if report.get("oracle_version") != schema_const(properties, "oracle_version"):
        errors.append("result Oracle version is not the sealed production Oracle")

    checks_schema = require_object(properties.get("checks"), "result checks schema", errors)
    checkpoints_schema = require_object(properties.get("checkpoints"), "result checkpoints schema", errors)
    checks = report.get("checks")
    checkpoints = report.get("checkpoints")
    if not isinstance(checks, list):
        errors.append("result checks evidence is missing")
    else:
        minimum = checks_schema.get("minItems")
        maximum = checks_schema.get("maxItems")
        if not isinstance(minimum, int) or len(checks) != minimum or len(checks) != maximum:
            errors.append("result checks evidence has the wrong required cardinality")
    if not isinstance(checkpoints, list):
        errors.append("result checkpoints evidence is missing")
    else:
        minimum = checkpoints_schema.get("minItems")
        maximum = checkpoints_schema.get("maxItems")
        if not isinstance(minimum, int) or len(checkpoints) != minimum or len(checkpoints) != maximum:
            errors.append("result checkpoints evidence has the wrong required cardinality")


def validate_evidence(report: dict[str, Any], artifact_values: dict[str, Any], errors: list[str]) -> None:
    launcher = require_object(report.get("launcher"), "result launcher", errors)
    attestation = require_object(launcher.get("build_attestation"), "result build attestation", errors)
    readiness = require_object(report.get("readiness"), "result readiness", errors)
    product = require_object(readiness.get("product"), "result readiness product", errors)
    for field in ("evidence_digest", "result_payload_digest"):
        require_sha256(report.get(field), field, errors)
    if report.get("scenario_state") != "green":
        errors.append("production scenario state is not green")
    for field in ("run_nonce", "freshness_challenge"):
        value = report.get(field)
        if not isinstance(value, str) or len(value) < 32:
            errors.append(f"{field} is missing or shorter than the production schema minimum")
    process_exit = require_object(report.get("process_exit"), "result process_exit", errors)
    if process_exit != {
        "test_process_code": 0,
        "sut_termination": "harness_initiated_after_independent_terminals",
        "sut_reaped": True,
    }:
        errors.append("result process_exit is not the complete green production process evidence")
    if launcher.get("sut_source_revision") != EXPECTED_REVISION:
        errors.append("launcher SUT revision does not match the expected SUT revision")
    if attestation.get("source_revision") != EXPECTED_REVISION:
        errors.append("build attestation revision does not match the expected SUT revision")
    if launcher.get("executable_sha256") != attestation.get("executable_sha256"):
        errors.append("launcher and build attestation executable digests differ")
    if launcher.get("executable_sha256") != product.get("executable_sha256"):
        errors.append("launcher and readiness executable digests differ")
    require_sha256(launcher.get("executable_sha256"), "launcher executable digest", errors)
    if not isinstance(report.get("native_provider"), dict) or not report["native_provider"]:
        errors.append("native Provider evidence is missing or zero")
    if not isinstance(report.get("client_output"), dict) or not report["client_output"]:
        errors.append("listener client evidence is missing or zero")
    if not isinstance(report.get("observations"), dict) or not report["observations"]:
        errors.append("independent observation evidence is missing or zero")
    freshness_challenge = report.get("freshness_challenge")
    if isinstance(freshness_challenge, str):
        expected_client, expected_native = frozen_expected_request_evidence(
            artifact_values, freshness_challenge, errors
        )
        if report.get("client_output") != expected_client:
            errors.append("listener client evidence is not the exact non-zero sealed response")
        if report.get("native_provider") != expected_native:
            errors.append("native Provider evidence is not the exact non-zero sealed request")
    privacy = require_object(report.get("privacy"), "result privacy", errors)
    if privacy != {
        "private_root_mode": "owner_only",
        "observation_root_mode": "owner_only",
        "process_root_mode": "owner_only",
        "sensitive_occurrences": 0,
    }:
        errors.append("privacy evidence is not the required owner-only/zero-secret result")

    evidence = {
        key: report.get(key)
        for key in ("launcher", "readiness", "native_provider", "client_output", "observations", "privacy")
    }
    if report.get("evidence_digest") != digest(evidence):
        errors.append("result evidence digest is stale or corrupt")
    payload = dict(report)
    payload["result_payload_digest"] = ""
    if report.get("result_payload_digest") != digest(payload):
        errors.append("result payload digest is stale or corrupt")

    scenario = require_object(
        artifact_values.get("scenarios/p0-gateway.json"),
        "sealed production scenario",
        errors,
    )
    coverage = scenario.get("coverage")
    if not isinstance(coverage, list) or len(coverage) != 1 or not isinstance(coverage[0], dict):
        errors.append("sealed production scenario coverage is missing")
        expected_check_ids: set[str] = set()
    else:
        expected_check_ids = set(coverage[0].get("evidence", []))
    checks = report.get("checks")
    if isinstance(checks, list):
        check_ids: list[str] = []
        check_by_id: dict[str, dict[str, Any]] = {}
        for index, item in enumerate(checks):
            check = require_object(item, f"result check {index}", errors)
            identifier = check.get("id") if isinstance(check.get("id"), str) else ""
            check_ids.append(identifier)
            if identifier:
                check_by_id[identifier] = check
            if check.get("status") != "passed":
                errors.append(f"result check {index} is not passed")
            expected = require_sha256(check.get("expected_digest"), f"result check {index} expected digest", errors)
            actual = require_sha256(check.get("actual_digest"), f"result check {index} actual digest", errors)
            if expected and actual and expected != actual:
                errors.append(f"result check {index} digest does not prove an exact pass")
            if not isinstance(check.get("detail"), str) or not check["detail"]:
                errors.append(f"result check {index} has no evidence detail")
        if len(set(check_ids)) != len(check_ids) or set(check_ids) != expected_check_ids:
            errors.append("result checks are missing, duplicated, or filtered against sealed coverage")
        verify_derivable_check_digests(
            report,
            check_by_id,
            expected_client if isinstance(freshness_challenge, str) else None,
            expected_native if isinstance(freshness_challenge, str) else None,
            errors,
        )

    checkpoint_contract = scenario.get("checkpoints")
    expected_checkpoints: dict[str, list[str]] = {}
    if isinstance(checkpoint_contract, list):
        for item in checkpoint_contract:
            if isinstance(item, dict) and isinstance(item.get("id"), str) and isinstance(item.get("evidence"), list):
                expected_checkpoints[item["id"]] = item["evidence"]
    if tuple(expected_checkpoints) != REQUIRED_CHECKPOINTS:
        errors.append("sealed production checkpoint contract is not the frozen exact set")
    checkpoints = report.get("checkpoints")
    if isinstance(checkpoints, list):
        actual_checkpoints: dict[str, Any] = {}
        for index, item in enumerate(checkpoints):
            checkpoint = require_object(item, f"result checkpoint {index}", errors)
            identifier = checkpoint.get("id")
            if not isinstance(identifier, str) or identifier in actual_checkpoints:
                errors.append(f"result checkpoint {index} is missing or duplicated")
                continue
            actual_checkpoints[identifier] = checkpoint
            if checkpoint.get("status") != "passed":
                errors.append(f"result checkpoint {identifier} is not passed")
            if checkpoint.get("evidence") != expected_checkpoints.get(identifier):
                errors.append(f"result checkpoint {identifier} is missing or filters required evidence")
        if tuple(actual_checkpoints) != REQUIRED_CHECKPOINTS:
            errors.append("result checkpoints do not provide every required exact checkpoint")


def verify_derivable_check_digests(
    report: dict[str, Any],
    checks: dict[str, dict[str, Any]],
    expected_client: Any,
    expected_native: Any,
    errors: list[str],
) -> None:
    """Bind every green check digest to the evidence it names.

    The production Oracle derives these values before it serializes the report.
    Repeating the digest projection here prevents a self-labelled passed check
    from standing in for the associated non-zero evidence.
    """

    observations = report.get("observations")
    observations = observations if isinstance(observations, dict) else {}
    privacy_expected = {
        "private_root_mode": "owner_only",
        "observation_root_mode": "owner_only",
        "process_root_mode": "owner_only",
        "sensitive_occurrences": 0,
    }
    aggregate_expected = {
        "port_digest": report.get("aggregate_port_digest"),
        "independent_streams": True,
        "content_terminal_independent": True,
    }
    aggregate_actual = {
        "port_digest": report.get("aggregate_port_digest"),
        "independent_streams": observations.get("independent_streams"),
        "content_terminal_independent": observations.get("content_terminal_independent"),
    }
    actual_values: dict[str, Any] = {
        "launch_production": report.get("launcher"),
        "readiness_exact": report.get("readiness"),
        "listener_client": report.get("client_output"),
        "native_provider": report.get("native_provider"),
        "lifecycle_stream": observations.get("lifecycle"),
        "execution_stream": observations.get("execution_fact"),
        "content_stream": observations.get("conversation_content"),
        "otel_stream": observations.get("otel"),
        "privacy_resource": report.get("privacy"),
        "aggregate_port": aggregate_actual,
    }
    expected_values: dict[str, Any] = {
        **actual_values,
        "listener_client": expected_client,
        "native_provider": expected_native,
        "privacy_resource": privacy_expected,
        "aggregate_port": aggregate_expected,
    }
    for identifier, actual_value in actual_values.items():
        check = checks.get(identifier)
        if check is None:
            continue
        expected_digest = digest(expected_values[identifier])
        actual_digest = digest(actual_value)
        if check.get("expected_digest") != expected_digest:
            errors.append(f"result check {identifier} expected digest is not derivable from sealed evidence")
        if check.get("actual_digest") != actual_digest:
            errors.append(f"result check {identifier} actual digest is not derivable from recorded evidence")


def main() -> int:
    errors: list[str] = []
    try:
        report = require_object(read_json(RESULT_PATH), "production result", errors)
        manifest, schema_digests, artifacts = manifest_and_schemas(errors)
        reject_release_exclusions(report, "$", errors)
        validate_sealed_schemas(manifest, artifacts, report, errors)
        validate_exact_seal(manifest, schema_digests, artifacts, report, errors)
        validate_result_shape(report, artifacts, errors)
        validate_evidence(report, artifacts, errors)
    except ValueError as error:
        errors.append(str(error))
    payload = {
        "schema_version": "hiroute.p0-gateway-final-gate/v2",
        "result": str(RESULT_PATH),
        "expected_sut_revision": EXPECTED_REVISION,
        "semantic_status": "green" if not errors else "red",
        "errors": errors,
    }
    print(json.dumps(payload, sort_keys=True))
    raise SystemExit(0 if not errors else 1)


main()
PY
