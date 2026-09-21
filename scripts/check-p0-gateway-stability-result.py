#!/usr/bin/env python3
"""Validate production-subprocess stability evidence independently of its exit."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


SCENARIO_SECONDS = {"sixty-second": 60, "ten-minute": 600}
# These are Gate policy, not driver configuration.  The driver records the
# same values for auditability, but its self-report can never relax them.
RSS_DELTA_CEILING_KIB = 64 * 1024
REPLAY_RETAINED_CAPACITY_CEILING_BYTES = 8 * 1024 * 1024


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--driver-log", type=Path, required=True)
    parser.add_argument("--scenario", choices=sorted(SCENARIO_SECONDS), required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--hirouted-sha256", required=True)
    parser.add_argument("--driver-process-exit", type=int, required=True)
    parser.add_argument("--result", type=Path, required=True)
    return parser.parse_args()


def report_from_log(path: Path) -> dict[str, Any]:
    if not path.is_file():
        raise ValueError(f"missing production stability driver log: {path}")
    for line in reversed(path.read_text(encoding="utf-8", errors="replace").splitlines()):
        try:
            candidate = json.loads(line)
        except json.JSONDecodeError:
            continue
        if candidate.get("schema_version") == "hiroute.p0-gateway-production-stability/v2":
            return candidate
    raise ValueError("production stability driver did not emit its terminal JSON evidence")


def integer(value: Any, field: str, errors: list[str]) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        errors.append(f"{field} is not a non-negative integer")
        return 0
    return value


def validate(args: argparse.Namespace, report: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    expected_seconds = SCENARIO_SECONDS[args.scenario]
    if args.driver_process_exit != 0:
        errors.append(f"production stability driver exited {args.driver_process_exit}")
    if report.get("semantic_status") != "green":
        errors.append("production stability driver did not report semantic green")
    if report.get("scenario") != args.scenario:
        errors.append("driver scenario does not match the requested dedicated case")
    if report.get("expected_duration_seconds") != expected_seconds:
        errors.append("driver duration does not match the required dedicated case")
    if report.get("revision") != args.revision:
        errors.append("driver revision does not match the exact checked-out revision")

    hirouted = report.get("hirouted")
    if not isinstance(hirouted, dict):
        errors.append("driver omitted hirouted subprocess evidence")
    else:
        if hirouted.get("product_subprocess") is not True:
            errors.append("driver did not prove a hirouted product subprocess")
        if not isinstance(hirouted.get("pid"), int) or hirouted["pid"] <= 0:
            errors.append("driver hirouted PID is invalid")
        if hirouted.get("sha256") != args.hirouted_sha256:
            errors.append("driver hirouted digest does not match the release binary built by this run")
        if not isinstance(hirouted.get("path"), str) or not hirouted["path"]:
            errors.append("driver hirouted path is missing")

    if integer(report.get("provider_requests"), "provider_requests", errors) < 2:
        errors.append("driver did not prove warmup and long-stream Provider requests")
    if report.get("response_status") != 200:
        errors.append("long-stream production response was not HTTP 200")
    if integer(report.get("long_stream_seconds"), "long_stream_seconds", errors) < expected_seconds:
        errors.append("long-stream evidence is shorter than the required dedicated duration")

    rss = report.get("rss")
    if not isinstance(rss, dict):
        errors.append("driver omitted RSS evidence")
    else:
        baseline = integer(rss.get("baseline_kib"), "rss.baseline_kib", errors)
        peak = integer(rss.get("peak_kib"), "rss.peak_kib", errors)
        delta = integer(rss.get("delta_kib"), "rss.delta_kib", errors)
        reported_ceiling = integer(rss.get("ceiling_kib"), "rss.ceiling_kib", errors)
        if peak < baseline or delta != peak - baseline:
            errors.append("RSS measurements are internally inconsistent")
        if reported_ceiling != RSS_DELTA_CEILING_KIB:
            errors.append("driver RSS policy does not match the trusted 64 MiB ceiling")
        if delta > RSS_DELTA_CEILING_KIB:
            errors.append("RSS delta exceeds the trusted 64 MiB ceiling")

    replay = report.get("replay")
    if not isinstance(replay, dict):
        errors.append("driver omitted replay-retention evidence")
    else:
        peak = integer(replay.get("peak_bytes"), "replay.peak_bytes", errors)
        reported_ceiling = integer(replay.get("ceiling_bytes"), "replay.ceiling_bytes", errors)
        terminal = integer(replay.get("terminal_bytes"), "replay.terminal_bytes", errors)
        if reported_ceiling != REPLAY_RETAINED_CAPACITY_CEILING_BYTES:
            errors.append("driver replay policy does not match the trusted 8 MiB ceiling")
        if peak > REPLAY_RETAINED_CAPACITY_CEILING_BYTES:
            errors.append("replay retained capacity exceeds the trusted 8 MiB ceiling")
        if peak == 0:
            errors.append("driver did not observe disk-spill replay backing during the long stream")
        if terminal != 0:
            errors.append("replay backing remained after terminal cleanup")

    if not isinstance(report.get("errors"), list):
        errors.append("driver errors field is malformed")
    elif report["errors"]:
        errors.append("driver reported one or more semantic errors")
    return errors


def main() -> int:
    args = parse_args()
    try:
        report = report_from_log(args.driver_log)
        errors = validate(args, report)
    except (OSError, ValueError) as error:
        report = None
        errors = [str(error)]
    payload = {
        "schema_version": "hiroute.p0-gateway-stability-result/v2",
        "scenario": args.scenario,
        "expected_duration_seconds": SCENARIO_SECONDS[args.scenario],
        "revision": args.revision,
        "test_process_exit": args.driver_process_exit,
        "driver_process_exit": args.driver_process_exit,
        "trusted_policy": {
            "rss_delta_ceiling_kib": RSS_DELTA_CEILING_KIB,
            "replay_retained_capacity_ceiling_bytes": REPLAY_RETAINED_CAPACITY_CEILING_BYTES,
        },
        "semantic_status": "green" if not errors else "red",
        "errors": errors,
        "driver": report,
        "driver_log": str(args.driver_log),
    }
    args.result.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"semantic_status": payload["semantic_status"], "errors": errors}, sort_keys=True))
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
