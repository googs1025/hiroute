#!/usr/bin/env python3
"""Real pinned CPA authentication ablation; no account credentials or model calls.

This component experiment is not Desktop publication acceptance evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def request(port, secret):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/v0/management/auth-files",
                                 headers={"Authorization": f"Bearer {secret}"})
    start = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=3) as response:
            status, body = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, body = error.code, error.read()
    return status, body, (time.perf_counter() - start) * 1000


def sample(binary, pipe, previous_secret):
    secret = secrets.token_urlsafe(32)
    with tempfile.TemporaryDirectory(prefix="hiroute-cpa-auth-") as temporary:
        root = Path(temporary)
        (root / "auth").mkdir(mode=0o700)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        config = root / "config.yaml"
        config.write_text(json.dumps({
            "host": "127.0.0.1", "port": port, "auth-dir": str(root / "auth"),
            "remote-management": {"allow-remote": False, "secret-key": secret,
                                  "disable-control-panel": True, "disable-auto-update-panel": True},
            "api-keys": [secrets.token_urlsafe(32)], "logging-to-file": False,
            "usage-statistics-enabled": False, "plugins": {"enabled": False},
        }))
        config.chmod(0o600)
        args = [str(binary), "--config", str(config), "--local-model"]
        if pipe:
            args.append("--local-password-stdin")
        start = time.perf_counter()
        child = subprocess.Popen(args, cwd=root, env={}, stdin=subprocess.PIPE if pipe else subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            if pipe:
                child.stdin.write(secret.encode())
                child.stdin.close()
            deadline = start + 15
            while True:
                if child.poll() is not None:
                    raise RuntimeError("CPA exited before authenticated readiness")
                try:
                    status, body, first_ms = request(port, secret)
                    if status == 200:
                        assert json.loads(body).get("files") == [], "isolated auth directory must be empty"
                        break
                except (OSError, urllib.error.URLError):
                    pass
                if time.perf_counter() >= deadline:
                    raise RuntimeError("CPA authenticated readiness timed out")
                time.sleep(.02)
            ready_ms = (time.perf_counter() - start) * 1000
            timings = []
            for _ in range(20):
                status, _, elapsed = request(port, secret)
                assert status == 200
                timings.append(elapsed)
            assert request(port, previous_secret or "wrong-credential")[0] in (401, 403)
            return {"mode": "parent_pipe" if pipe else "config_bcrypt", "ready_ms": ready_ms,
                    "first_authenticated_request_ms": first_ms, "repeat_count": len(timings),
                    "repeat_median_ms": statistics.median(timings), "repeat_max_ms": max(timings),
                    "wrong_or_previous_instance_credential": "rejected", "model_calls": 0}, secret
        finally:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    runs, previous = [], None
    for pipe in [False, True, True]:
        result, previous = sample(binary, pipe, previous)
        runs.append(result)
    report = {"scope": "real_cpa_component_only", "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "runs": runs}
    with args.output.open("x") as output:
        json.dump(report, output, indent=2)
        output.write("\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
