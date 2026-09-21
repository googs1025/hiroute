#!/usr/bin/env python3
"""Summarize exact-base versus candidate fixed-runner Gateway measurements."""

from __future__ import annotations

import argparse
import json
import math
import statistics
from pathlib import Path
from typing import Any


MIN_ROUNDS = 5
THRESHOLD_PERCENT = 5.0
# Per-request variance and mean CI are evidence-quality signals, rather than a
# second latency SLO.  A tenfold ceiling over the exact-base value catches
# broken measurement; a p50-derived 1% noise floor applies only when the base
# reports exact zero, leaving ordinary fixed-run scheduler jitter to the
# primary throughput/latency comparison.
DISPERSION_MAX_MULTIPLIER = 10.0
DISPERSION_NOISE_FLOOR_RATIO = 0.01
DYNAMIC_RESOURCE_DETAIL_METRICS = frozenset(
    {"queue_mean_ns", "run_mean_ns", "resume_mean_ns"}
)


class BenchmarkError(RuntimeError):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-revision", required=True)
    parser.add_argument("--candidate-revision", required=True)
    parser.add_argument("--harness-revision", required=True)
    parser.add_argument("--base-core", type=Path, required=True)
    parser.add_argument("--candidate-core", type=Path, required=True)
    parser.add_argument("--base-resource", type=Path, required=True)
    parser.add_argument("--candidate-resource", type=Path, required=True)
    parser.add_argument("--base-replay", type=Path, required=True)
    parser.add_argument("--candidate-replay", type=Path, required=True)
    parser.add_argument("--base-product", type=Path, required=True)
    parser.add_argument("--candidate-product", type=Path, required=True)
    parser.add_argument("--base-hirouted-sha256", required=True)
    parser.add_argument("--candidate-hirouted-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def text(path: Path) -> str:
    if not path.is_file():
        raise BenchmarkError(f"missing benchmark output: {path}")
    return path.read_text(encoding="utf-8", errors="replace")


def numeric(value: str, field: str) -> float:
    try:
        parsed = float(value)
    except ValueError as error:
        raise BenchmarkError(f"invalid {field}: {value!r}") from error
    if not math.isfinite(parsed):
        raise BenchmarkError(f"invalid {field}: expected a finite number")
    return parsed


def nonnegative_numeric(value: str, field: str) -> float:
    parsed = numeric(value, field)
    if parsed < 0:
        raise BenchmarkError(f"invalid {field}: expected a non-negative number")
    return parsed


def integer(value: str, field: str) -> int:
    try:
        return int(value)
    except ValueError as error:
        raise BenchmarkError(f"invalid {field}: {value!r}") from error


def aggregate(values: list[float]) -> dict[str, float | int]:
    if len(values) < MIN_ROUNDS:
        raise BenchmarkError(f"expected at least {MIN_ROUNDS} independent rounds, got {len(values)}")
    mean = statistics.fmean(values)
    variance = statistics.variance(values) if len(values) > 1 else 0.0
    return {
        "round_count": len(values),
        "mean": mean,
        "round_variance": variance,
        "round_mean_ci95": 1.96 * math.sqrt(variance / len(values)),
    }


def parse_detail(value: str) -> dict[str, str]:
    detail: dict[str, str] = {}
    for item in value.split(";"):
        if not item:
            continue
        key, separator, item_value = item.partition("=")
        if separator:
            detail[key] = item_value
    return detail


def parse_core(path: Path) -> dict[str, Any]:
    rows: dict[str, list[dict[str, float | int]]] = {"h1": [], "h2": []}
    for line in text(path).splitlines():
        fields = line.split("\t")
        if len(fields) != 10 or fields[0] not in rows or fields[2] != "gateway-production-driver":
            continue
        protocol, round_text, _, throughput, ttft_p50, ttft_p99, p50, p99, variance, ci = fields
        rows[protocol].append(
            {
                "round": integer(round_text, "round"),
                "throughput_ops_s": numeric(throughput, "throughput_ops_s"),
                "ttft_p50_ns": integer(ttft_p50, "ttft_p50_ns"),
                "ttft_p99_ns": integer(ttft_p99, "ttft_p99_ns"),
                "p50_ns": integer(p50, "p50_ns"),
                "p99_ns": integer(p99, "p99_ns"),
                "request_sample_variance_ns2": numeric(variance, "variance_ns2"),
                "request_mean_ci95_ns": numeric(ci, "mean_ci95_ns"),
            }
        )
    summary: dict[str, Any] = {}
    for protocol, protocol_rows in rows.items():
        if len(protocol_rows) < MIN_ROUNDS:
            raise BenchmarkError(f"{path}: {protocol} lacks {MIN_ROUNDS} gateway-production-driver rounds")
        rounds = {int(row["round"]) for row in protocol_rows}
        if len(rounds) != len(protocol_rows):
            raise BenchmarkError(f"{path}: {protocol} has duplicate benchmark rounds")
        metrics = {
            key: aggregate([float(row[key]) for row in protocol_rows])
            for key in ("throughput_ops_s", "ttft_p50_ns", "ttft_p99_ns", "p50_ns", "p99_ns")
        }
        metrics["request_sample_variance_ns2"] = {
            "mean": statistics.fmean(float(row["request_sample_variance_ns2"]) for row in protocol_rows)
        }
        metrics["request_mean_ci95_ns"] = {
            "mean": statistics.fmean(float(row["request_mean_ci95_ns"]) for row in protocol_rows)
        }
        summary[protocol] = {"rounds": protocol_rows, "metrics": metrics}
    return summary


def parse_replay(path: Path) -> dict[str, Any]:
    rows: dict[str, list[dict[str, Any]]] = {"memory-retained": [], "disk-spill": []}
    for line in text(path).splitlines():
        fields = line.split("\t")
        if len(fields) != 7 or fields[0] != "gateway_replay" or fields[1] not in rows:
            continue
        _, variant, round_text, payload_text, elapsed_text, ns_per_operation, detail_text = fields
        detail = parse_detail(detail_text)
        required = {"copy_bytes", "scan_bytes", "peak_retained_bytes", "peak_spill_bytes", "disk_backed"}
        if not required.issubset(detail):
            raise BenchmarkError(f"{path}: replay row is missing required resource metrics")
        row = {
            "round": integer(round_text, "replay round"),
            "payload_bytes": integer(payload_text, "payload_bytes"),
            "elapsed_ns": integer(elapsed_text, "elapsed_ns"),
            "ns_per_operation": integer(ns_per_operation, "ns_per_operation"),
            "copy_bytes": integer(detail["copy_bytes"], "copy_bytes"),
            "scan_bytes": integer(detail["scan_bytes"], "scan_bytes"),
            "peak_retained_bytes": integer(detail["peak_retained_bytes"], "peak_retained_bytes"),
            "peak_spill_bytes": integer(detail["peak_spill_bytes"], "peak_spill_bytes"),
            "disk_backed": detail["disk_backed"] == "true",
        }
        if row["copy_bytes"] != row["payload_bytes"] or row["scan_bytes"] != row["payload_bytes"]:
            raise BenchmarkError(f"{path}: replay row did not copy and verify the whole payload")
        if row["disk_backed"] != (variant == "disk-spill"):
            raise BenchmarkError(f"{path}: replay backing mode disagrees with its variant")
        if row["disk_backed"] and row["peak_spill_bytes"] == 0:
            raise BenchmarkError(f"{path}: disk-spill row reports zero backing bytes")
        if not row["disk_backed"] and row["peak_spill_bytes"] != 0:
            raise BenchmarkError(f"{path}: memory row reports unexpected spill bytes")
        rows[variant].append(row)
    summary: dict[str, Any] = {}
    for variant, variant_rows in rows.items():
        if len(variant_rows) < MIN_ROUNDS:
            raise BenchmarkError(f"{path}: {variant} lacks {MIN_ROUNDS} independent rounds")
        rounds = {int(row["round"]) for row in variant_rows}
        if len(rounds) != len(variant_rows):
            raise BenchmarkError(f"{path}: {variant} has duplicate benchmark rounds")
        summary[variant] = {
            "rounds": variant_rows,
            "metrics": {
                key: aggregate([float(row[key]) for row in variant_rows])
                for key in ("elapsed_ns", "copy_bytes", "scan_bytes", "peak_retained_bytes", "peak_spill_bytes")
            },
        }
    return summary


def parse_product(path: Path, expected_hirouted_sha256: str) -> dict[str, Any]:
    rows: dict[str, list[dict[str, Any]]] = {"memory-retained": [], "disk-spill": []}
    for line in text(path).splitlines():
        fields = line.split("\t")
        if len(fields) != 12 or fields[0] != "gateway_product" or fields[1] not in rows:
            continue
        (
            _,
            variant,
            round_text,
            iterations_text,
            throughput,
            ttft_p50,
            ttft_p99,
            p50,
            p99,
            variance,
            ci,
            detail_text,
        ) = fields
        detail = parse_detail(detail_text)
        required = {
            "copy_bytes",
            "scan_bytes",
            "peak_rss_delta_kib",
            "peak_replay_bytes",
            "terminal_replay_bytes",
            "hirouted_sha256",
        }
        if not required.issubset(detail):
            raise BenchmarkError(f"{path}: product row is missing required subprocess resource metrics")
        row = {
            "round": integer(round_text, "product round"),
            "iterations": integer(iterations_text, "product iterations"),
            "throughput_ops_s": numeric(throughput, "product throughput_ops_s"),
            "ttft_p50_ns": integer(ttft_p50, "product ttft_p50_ns"),
            "ttft_p99_ns": integer(ttft_p99, "product ttft_p99_ns"),
            "p50_ns": integer(p50, "product p50_ns"),
            "p99_ns": integer(p99, "product p99_ns"),
            "request_sample_variance_ns2": nonnegative_numeric(variance, "product variance_ns2"),
            "request_mean_ci95_ns": nonnegative_numeric(ci, "product mean_ci95_ns"),
            "copy_bytes": integer(detail["copy_bytes"], "product copy_bytes"),
            "scan_bytes": integer(detail["scan_bytes"], "product scan_bytes"),
            "peak_rss_delta_kib": integer(detail["peak_rss_delta_kib"], "product peak_rss_delta_kib"),
            "peak_replay_bytes": integer(detail["peak_replay_bytes"], "product peak_replay_bytes"),
            "terminal_replay_bytes": integer(detail["terminal_replay_bytes"], "product terminal_replay_bytes"),
            "hirouted_sha256": detail["hirouted_sha256"],
        }
        if row["iterations"] <= 0 or row["throughput_ops_s"] <= 0:
            raise BenchmarkError(f"{path}: product row did not execute a positive production workload")
        if row["copy_bytes"] <= 0 or row["scan_bytes"] <= 0:
            raise BenchmarkError(f"{path}: product row did not report request copy/scan evidence")
        if row["terminal_replay_bytes"] != 0:
            raise BenchmarkError(f"{path}: product row retained replay backing after terminal cleanup")
        if len(row["hirouted_sha256"]) != 64:
            raise BenchmarkError(f"{path}: product row has an invalid release hirouted digest")
        if row["hirouted_sha256"] != expected_hirouted_sha256:
            raise BenchmarkError(f"{path}: product row did not execute the release hirouted binary built for this revision")
        if variant == "memory-retained" and row["peak_replay_bytes"] != 0:
            raise BenchmarkError(f"{path}: memory-retained product row wrote replay backing")
        if variant == "disk-spill" and row["peak_replay_bytes"] == 0:
            raise BenchmarkError(f"{path}: disk-spill product row did not observe replay backing")
        rows[variant].append(row)
    summary: dict[str, Any] = {}
    for variant, variant_rows in rows.items():
        if len(variant_rows) < MIN_ROUNDS:
            raise BenchmarkError(f"{path}: {variant} lacks {MIN_ROUNDS} independent product rounds")
        rounds = {int(row["round"]) for row in variant_rows}
        if len(rounds) != len(variant_rows):
            raise BenchmarkError(f"{path}: {variant} has duplicate product benchmark rounds")
        summary[variant] = {
            "rounds": variant_rows,
            "metrics": {
                key: aggregate([float(row[key]) for row in variant_rows])
                for key in (
                    "throughput_ops_s",
                    "ttft_p50_ns",
                    "ttft_p99_ns",
                    "p50_ns",
                    "p99_ns",
                    "copy_bytes",
                    "scan_bytes",
                    "peak_rss_delta_kib",
                    "peak_replay_bytes",
                    "request_sample_variance_ns2",
                    "request_mean_ci95_ns",
                )
            },
        }
    return summary


def parse_resource(path: Path) -> dict[str, Any]:
    rows: dict[str, list[dict[str, Any]]] = {}
    for line in text(path).splitlines():
        fields = line.split("\t")
        if len(fields) < 6 or fields[0] in {"resource", "release", "rounds"}:
            continue
        try:
            resource = fields[0]
            variant = fields[1]
            row = {
                "iterations": integer(fields[2], "resource iterations"),
                "elapsed_ns": integer(fields[3], "resource elapsed_ns"),
                "ns_per_operation": integer(fields[4], "resource ns_per_operation"),
                "detail": parse_detail(fields[5]),
            }
        except BenchmarkError:
            continue
        rows.setdefault(f"{resource}:{variant}", []).append(row)
    if not rows:
        raise BenchmarkError(f"{path}: no resource benchmark rows")
    summary: dict[str, Any] = {}
    for key, values in rows.items():
        if len(values) < MIN_ROUNDS:
            raise BenchmarkError(f"{path}: {key} lacks {MIN_ROUNDS} independent resource rounds")
        detail_keys = set.intersection(*(set(row["detail"]) for row in values))
        detail_metrics = {
            detail_key: aggregate([numeric(row["detail"][detail_key], f"{key} {detail_key}") for row in values])
            for detail_key in sorted(detail_keys)
            if all(_is_numeric(row["detail"][detail_key]) for row in values)
        }
        summary[key] = {
            "rounds": values,
            "metrics": {
                "ns_per_operation": aggregate([float(row["ns_per_operation"]) for row in values]),
                "detail": detail_metrics,
            },
        }
    return summary


def _is_numeric(value: str) -> bool:
    try:
        float(value)
    except ValueError:
        return False
    return True


def percent_regression(base: float, candidate: float, higher_is_worse: bool) -> float:
    if base < 0 or candidate < 0:
        raise BenchmarkError("benchmark metric must not be negative")
    if base == 0:
        return 0.0 if candidate == 0 else math.inf
    delta = (candidate - base) / base * 100.0
    return max(delta if higher_is_worse else -delta, 0.0)


def mean(metrics: dict[str, Any], key: str) -> float:
    return float(metrics[key]["mean"])


def add_threshold_error(
    errors: list[str],
    label: str,
    base: float,
    candidate: float,
    higher_is_worse: bool,
) -> float:
    regression = percent_regression(base, candidate, higher_is_worse)
    if regression > THRESHOLD_PERCENT:
        errors.append(f"{label} regression exceeds {THRESHOLD_PERCENT}%")
    return regression


def confidence_aware_timing_comparison(
    base_metrics: dict[str, Any], candidate_metrics: dict[str, Any], metric: str = "ns_per_operation"
) -> dict[str, float]:
    """Compare timing means only after allowing each five-round 95% interval.

    This preserves the 5% fixed-run limit while avoiding an exact-value check on
    scheduler-dependent measurements.  A candidate must be slower even at its
    favorable confidence bound than the base at its unfavorable bound.
    """

    base_mean = mean(base_metrics, metric)
    candidate_mean = mean(candidate_metrics, metric)
    base_ci95 = float(base_metrics[metric]["round_mean_ci95"])
    candidate_ci95 = float(candidate_metrics[metric]["round_mean_ci95"])
    base_upper = base_mean + base_ci95
    candidate_lower = max(candidate_mean - candidate_ci95, 0.0)
    return {
        "base_mean": base_mean,
        "candidate_mean": candidate_mean,
        "base_round_mean_ci95": base_ci95,
        "candidate_round_mean_ci95": candidate_ci95,
        "observed_regression_pct": percent_regression(base_mean, candidate_mean, higher_is_worse=True),
        "confidence_aware_regression_pct": percent_regression(
            base_upper, candidate_lower, higher_is_worse=True
        ),
    }


def add_confidence_aware_timing_error(
    errors: list[str],
    label: str,
    base_metrics: dict[str, Any],
    candidate_metrics: dict[str, Any],
    metric: str = "ns_per_operation",
) -> dict[str, float]:
    comparison = confidence_aware_timing_comparison(base_metrics, candidate_metrics, metric)
    comparison["threshold_pct"] = THRESHOLD_PERCENT
    if comparison["confidence_aware_regression_pct"] > THRESHOLD_PERCENT:
        errors.append(f"{label} confidence-aware regression exceeds {THRESHOLD_PERCENT}%")
    return comparison


def compare_dispersion(
    errors: list[str],
    label: str,
    base_value: float,
    candidate_value: float,
    latency_scale_ns: float,
    *,
    squared_units: bool,
) -> dict[str, float]:
    """Gate unusable product variance/CI evidence without treating normal jitter as a regression."""

    noise_floor = max(latency_scale_ns * DISPERSION_NOISE_FLOOR_RATIO, 1.0)
    if squared_units:
        noise_floor *= noise_floor
    reference = base_value if base_value > 0 else noise_floor
    multiplier = candidate_value / reference
    if multiplier > DISPERSION_MAX_MULTIPLIER:
        errors.append(
            f"{label} exceeds {DISPERSION_MAX_MULTIPLIER:g}x exact-base/noise-floor dispersion policy"
        )
    return {
        "base_mean": base_value,
        "candidate_mean": candidate_value,
        "noise_floor": noise_floor,
        "reference": reference,
        "candidate_multiplier": multiplier,
        "max_multiplier": DISPERSION_MAX_MULTIPLIER,
    }


def compare_core(base: dict[str, Any], candidate: dict[str, Any]) -> tuple[dict[str, Any], list[str]]:
    comparison: dict[str, Any] = {}
    errors: list[str] = []
    for protocol in ("h1", "h2"):
        base_metrics = base[protocol]["metrics"]
        candidate_metrics = candidate[protocol]["metrics"]
        throughput_regression = add_threshold_error(
            errors,
            f"{protocol} throughput",
            mean(base_metrics, "throughput_ops_s"),
            mean(candidate_metrics, "throughput_ops_s"),
            higher_is_worse=False,
        )
        p99_increase = add_threshold_error(
            errors,
            f"{protocol} p99",
            mean(base_metrics, "p99_ns"),
            mean(candidate_metrics, "p99_ns"),
            higher_is_worse=True,
        )
        comparison[protocol] = {
            "throughput_regression_pct": throughput_regression,
            "p99_increase_pct": p99_increase,
            "threshold_pct": THRESHOLD_PERCENT,
        }
    return comparison, errors


def compare_replay(base: dict[str, Any], candidate: dict[str, Any]) -> tuple[dict[str, Any], list[str]]:
    result: dict[str, Any] = {}
    errors: list[str] = []
    for variant in ("memory-retained", "disk-spill"):
        base_metrics = base[variant]["metrics"]
        candidate_metrics = candidate[variant]["metrics"]
        comparison: dict[str, Any] = {"threshold_pct": THRESHOLD_PERCENT}
        for metric in ("elapsed_ns", "peak_retained_bytes", "peak_spill_bytes"):
            base_value = mean(base_metrics, metric)
            candidate_value = mean(candidate_metrics, metric)
            comparison[metric] = {
                "base_mean": base_value,
                "candidate_mean": candidate_value,
                "regression_pct": add_threshold_error(
                    errors,
                    f"gateway_replay {variant} {metric}",
                    base_value,
                    candidate_value,
                    higher_is_worse=True,
                ),
            }
        for metric in ("copy_bytes", "scan_bytes"):
            base_value = mean(base_metrics, metric)
            candidate_value = mean(candidate_metrics, metric)
            comparison[metric] = {"base_mean": base_value, "candidate_mean": candidate_value}
            if candidate_value != base_value:
                errors.append(f"gateway_replay {variant} {metric} differs from exact base")
        result[variant] = comparison
    return result, errors


def compare_product(base: dict[str, Any], candidate: dict[str, Any]) -> tuple[dict[str, Any], list[str]]:
    result: dict[str, Any] = {}
    errors: list[str] = []
    for variant in ("memory-retained", "disk-spill"):
        base_metrics = base[variant]["metrics"]
        candidate_metrics = candidate[variant]["metrics"]
        comparison: dict[str, Any] = {
            "threshold_pct": THRESHOLD_PERCENT,
            "dispersion_policy": {
                "max_multiplier": DISPERSION_MAX_MULTIPLIER,
                "noise_floor_ratio_of_base_p50": DISPERSION_NOISE_FLOOR_RATIO,
            },
        }
        for metric, higher_is_worse in (
            ("throughput_ops_s", False),
            ("ttft_p50_ns", True),
            ("ttft_p99_ns", True),
            ("p50_ns", True),
            ("p99_ns", True),
            ("peak_rss_delta_kib", True),
            ("peak_replay_bytes", True),
        ):
            base_value = mean(base_metrics, metric)
            candidate_value = mean(candidate_metrics, metric)
            comparison[metric] = {
                "base_mean": base_value,
                "candidate_mean": candidate_value,
                "regression_pct": add_threshold_error(
                    errors,
                    f"gateway_product {variant} {metric}",
                    base_value,
                    candidate_value,
                    higher_is_worse=higher_is_worse,
                ),
            }
        for metric in ("copy_bytes", "scan_bytes"):
            base_value = mean(base_metrics, metric)
            candidate_value = mean(candidate_metrics, metric)
            comparison[metric] = {"base_mean": base_value, "candidate_mean": candidate_value}
            if candidate_value != base_value:
                errors.append(f"gateway_product {variant} {metric} differs from exact base")
        latency_scale_ns = max(mean(base_metrics, "p50_ns"), 1.0)
        comparison["request_sample_variance_ns2"] = compare_dispersion(
            errors,
            f"gateway_product {variant} request_sample_variance_ns2",
            mean(base_metrics, "request_sample_variance_ns2"),
            mean(candidate_metrics, "request_sample_variance_ns2"),
            latency_scale_ns,
            squared_units=True,
        )
        comparison["request_mean_ci95_ns"] = compare_dispersion(
            errors,
            f"gateway_product {variant} request_mean_ci95_ns",
            mean(base_metrics, "request_mean_ci95_ns"),
            mean(candidate_metrics, "request_mean_ci95_ns"),
            latency_scale_ns,
            squared_units=False,
        )
        result[variant] = comparison
    return result, errors


def compare_resource(base: dict[str, Any], candidate: dict[str, Any]) -> tuple[dict[str, Any], list[str]]:
    result: dict[str, Any] = {}
    errors: list[str] = []
    if set(base) != set(candidate):
        errors.append("resource benchmark paths differ between exact base and candidate")
    for key in sorted(set(base) & set(candidate)):
        base_metrics = base[key]["metrics"]
        candidate_metrics = candidate[key]["metrics"]
        comparison: dict[str, Any] = {
            "threshold_pct": THRESHOLD_PERCENT,
            "timing_policy": "five-round 95% confidence-aware relative regression",
            "ns_per_operation": add_confidence_aware_timing_error(
                errors,
                f"resource {key} ns_per_operation",
                base_metrics,
                candidate_metrics,
            ),
            "detail": {},
        }
        base_detail = base_metrics["detail"]
        candidate_detail = candidate_metrics["detail"]
        if set(base_detail) != set(candidate_detail):
            errors.append(f"resource {key} detail metrics differ between exact base and candidate")
        for detail_key in sorted(set(base_detail) & set(candidate_detail)):
            base_value = mean(base_detail, detail_key)
            candidate_value = mean(candidate_detail, detail_key)
            if detail_key in DYNAMIC_RESOURCE_DETAIL_METRICS:
                comparison["detail"][detail_key] = add_confidence_aware_timing_error(
                    errors,
                    f"resource {key} {detail_key}",
                    base_detail,
                    candidate_detail,
                    detail_key,
                )
            else:
                comparison["detail"][detail_key] = {
                    "base_mean": base_value,
                    "candidate_mean": candidate_value,
                }
                if candidate_value != base_value:
                    errors.append(f"resource {key} {detail_key} differs from exact base")
        result[key] = comparison
    return result, errors


def run(args: argparse.Namespace) -> dict[str, Any]:
    base_core = parse_core(args.base_core)
    candidate_core = parse_core(args.candidate_core)
    core_comparison, core_errors = compare_core(base_core, candidate_core)
    base_replay = parse_replay(args.base_replay)
    candidate_replay = parse_replay(args.candidate_replay)
    replay_comparison, replay_errors = compare_replay(base_replay, candidate_replay)
    base_product = parse_product(args.base_product, args.base_hirouted_sha256)
    candidate_product = parse_product(args.candidate_product, args.candidate_hirouted_sha256)
    product_comparison, product_errors = compare_product(base_product, candidate_product)
    base_resource = parse_resource(args.base_resource)
    candidate_resource = parse_resource(args.candidate_resource)
    resource_comparison, resource_errors = compare_resource(base_resource, candidate_resource)
    errors = core_errors + replay_errors + product_errors + resource_errors
    return {
        "schema_version": "hiroute.p0-gateway-fixed-runner-benchmark/v2",
        "base_revision": args.base_revision,
        "candidate_revision": args.candidate_revision,
        "benchmark_harness_revision": args.harness_revision,
        "release_mode": True,
        "semantic_status": "green" if not errors else "red",
        "errors": errors,
        "core": {"base": base_core, "candidate": candidate_core, "comparison": core_comparison},
        "gateway_replay": {"base": base_replay, "candidate": candidate_replay, "comparison": replay_comparison},
        "gateway_product": {"base": base_product, "candidate": candidate_product, "comparison": product_comparison},
        "resource_paths": {"base": base_resource, "candidate": candidate_resource, "comparison": resource_comparison},
    }


def main() -> int:
    args = parse_args()
    try:
        payload = run(args)
    except (BenchmarkError, OSError, ValueError) as error:
        payload = {
            "schema_version": "hiroute.p0-gateway-fixed-runner-benchmark/v2",
            "base_revision": args.base_revision,
            "candidate_revision": args.candidate_revision,
            "benchmark_harness_revision": args.harness_revision,
            "release_mode": True,
            "semantic_status": "red",
            "errors": [str(error)],
        }
    args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"semantic_status": payload["semantic_status"], "errors": payload["errors"]}, sort_keys=True))
    return 0 if payload["semantic_status"] == "green" else 1


if __name__ == "__main__":
    raise SystemExit(main())
